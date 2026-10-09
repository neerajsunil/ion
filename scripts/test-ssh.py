"""Isolated SSH integration fixture. Requires: pip install paramiko.

Usage: python scripts/test-ssh.py [--serve]
Runs a loopback paramiko SSH server (key, password and one-time-code logins,
direct-tcpip for ProxyJump) in a temporary directory, then the Rust
integration test against it with the real system `ssh`. No real credentials.

The "remote host" is a local shell (Git Bash on Windows, /bin/sh on Linux)
started for the `sh` exec request, so the whole path is real: Ion's bootstrap
uploads a server "binary" (a script that runs the natively built ion-server),
checks it, and execs `ion-server --stdio` over the SSH channel. A PATH shim
makes the host look like x86_64 Linux. On Windows, scripts/test-ssh-proxy.py
translates MSYS paths for the native server and terminals are skipped (the
server's pty support is Unix-only).
"""
import hashlib
import json
import os
import pathlib
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time

import paramiko
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, NoEncryption, PrivateFormat

WINDOWS = os.name == "nt"
HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent
TARGET = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
EXE = ".exe" if WINDOWS else ""


def cargo(*args):
    subprocess.run(["cargo", *args], check=True, cwd=ROOT)


def posix(path):
    """A path as the fixture's shell spells it (Git Bash: /c/Users/...)."""
    text = str(path).replace("\\", "/")
    return "/" + text[0].lower() + text[2:] if WINDOWS else text


def kill_tree(process):
    if WINDOWS:
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(process.pid)], capture_output=True)
    else:
        process.kill()


def write_script(path, text):
    path.write_bytes(text.encode())
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


cargo("build", "-q", "-p", "ion_server")
cargo("build", "-q", "-p", "ion_remote", "--example", "askpass")
server_binary = TARGET / "debug" / f"ion-server{EXE}"
askpass_program = TARGET / "debug" / "examples" / f"askpass{EXE}"
hello = json.loads(subprocess.run([server_binary, "--version"], check=True, capture_output=True).stdout)

with tempfile.TemporaryDirectory(prefix="ion-ssh-fixture-") as temp:
    temp = pathlib.Path(temp).resolve()
    project = temp / "project"
    (project / "protected").mkdir(parents=True)
    (project / "protected/original.txt").write_bytes(b"original\n")
    home = temp / "home"
    home.mkdir()
    ssh_dir = temp / "ssh"
    ssh_dir.mkdir()
    shim = temp / "shim"
    shim.mkdir()
    flags = temp / "flags"
    flags.mkdir()
    stats_path = temp / "stats.json"
    stats = {"transports": 0, "auth_prompts": 0}

    def write_stats():
        stats_path.write_text(json.dumps(stats))

    write_stats()

    # -- the fake host: PATH shims ------------------------------------------------
    write_script(shim / "uname", '#!/bin/sh\ncase "$1" in -m) echo x86_64;; *) echo Linux;; esac\n')
    write_script(shim / "ion-test-stderr", "#!/bin/sh\nprintf out\nprintf err >&2\nexit 3\n")
    # Git Bash puts its own /usr/bin first; this puts the shims ahead of it.
    write_script(temp / "bash-env", f'export PATH="{posix(shim)}:$PATH"\n')
    # Asks the fixture (which polls for this file) to drop every connection.
    write_script(shim / "ion-test-drop-connection", f'#!/bin/sh\n: > "{posix(flags)}/drop"\nsleep 30\n')

    # -- the server "binary" Ion uploads -------------------------------------------
    target = "x86_64-unknown-linux-musl"
    dist = temp / "server-dist"
    dist.mkdir()
    if WINDOWS:
        launcher = (
            f'#!/bin/sh\ncase "$1" in\n--stdio) exec "{posix(sys.executable)}" "{posix(HERE / "test-ssh-proxy.py")}" '
            f'"{posix(server_binary)}" --stdio;;\n*) exec "{posix(server_binary)}" "$@";;\nesac\n'
        )
    else:
        launcher = f'#!/bin/sh\nexec "{server_binary}" "$@"\n'
    artifact = f"ion-server-{hello['version']}-{target}"
    (dist / artifact).write_bytes(launcher.encode())
    (dist / "manifest.json").write_text(json.dumps({
        "version": hello["version"],
        "protocol": hello["protocol"],
        "artifacts": [{"target": target, "file": artifact, "sha256": hashlib.sha256(launcher.encode()).hexdigest()}],
    }))

    # -- keys and the client's private ssh config ----------------------------------
    host_key = paramiko.RSAKey.generate(2048)
    key_path = temp / "identity"
    key_path.write_bytes(Ed25519PrivateKey.generate().private_bytes(Encoding.PEM, PrivateFormat.OpenSSH, NoEncryption()))
    if WINDOWS:
        # OpenSSH refuses keys readable by anyone but their owner.
        subprocess.run(["icacls", str(key_path), "/inheritance:r", "/grant:r", f"{os.environ['USERNAME']}:R"], check=True, capture_output=True)
    identity = paramiko.Ed25519Key.from_private_key_file(str(key_path))
    null_device = "NUL" if WINDOWS else "/dev/null"
    (ssh_dir / "config").write_text(
        "Host *\n"
        "  IdentityAgent none\n"
        "  IdentitiesOnly yes\n"
        f'  IdentityFile "{(ssh_dir / "no-such-key").as_posix()}"\n'
        "  CheckHostIP no\n"
        f"  GlobalKnownHostsFile {null_device}\n"
        # ProxyJump's own ssh gets -F but not the -o options Ion passes.
        f'  UserKnownHostsFile "{(ssh_dir / "known_hosts").as_posix()}"\n'
    )

    # -- the SSH server ----------------------------------------------------------------
    transports = []
    channels = []
    tunnels = {}
    shells = []

    def run_shell(channel):
        env = os.environ.copy()
        env["PATH"] = str(shim) + os.pathsep + env.get("PATH", "")
        env["HOME"] = posix(home)
        env["BASH_ENV"] = posix(temp / "bash-env")
        process = subprocess.Popen(
            ["C:/Program Files/Git/bin/bash.exe" if WINDOWS else "/bin/sh"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=home,
        )
        shells.append(process)

        def pump(stream, send):
            try:
                while True:
                    data = stream.read1(65536)
                    if not data:
                        break
                    send(data)
            except (OSError, EOFError, paramiko.SSHException):
                pass

        def feed():
            try:
                while True:
                    data = channel.recv(65536)
                    if not data:
                        break
                    process.stdin.write(data)
                    process.stdin.flush()
            except (OSError, EOFError, paramiko.SSHException):
                pass
            finally:
                try:
                    process.stdin.close()
                except OSError:
                    pass

        threads = [
            threading.Thread(target=pump, args=(process.stdout, channel.sendall), daemon=True),
            threading.Thread(target=pump, args=(process.stderr, channel.sendall_stderr), daemon=True),
        ]
        for thread in threads:
            thread.start()
        threading.Thread(target=feed, daemon=True).start()
        try:
            status = process.wait()
            for thread in threads:
                thread.join(5)
            channel.send_exit_status(status)
        except (OSError, EOFError, paramiko.SSHException):
            pass
        finally:
            if process.poll() is None:
                kill_tree(process)
            channel.close()

    class Server(paramiko.ServerInterface):
        def check_auth_password(self, username, password):
            ok = username == "dev" and password == "ion-integration"
            return paramiko.AUTH_SUCCESSFUL if ok else paramiko.AUTH_FAILED

        # "mfa" logs in with a one-time code (keyboard-interactive).
        def check_auth_interactive(self, username, submethods):
            if username != "mfa":
                return paramiko.AUTH_FAILED
            stats["auth_prompts"] += 1
            write_stats()
            query = paramiko.InteractiveQuery("", "Enter the code from your app")
            query.add_prompt("Verification code: ", False)
            return query

        def check_auth_interactive_response(self, responses):
            return paramiko.AUTH_SUCCESSFUL if list(responses) == ["123456"] else paramiko.AUTH_FAILED

        def check_auth_publickey(self, username, key):
            return paramiko.AUTH_SUCCESSFUL if username == "dev" and key == identity else paramiko.AUTH_FAILED

        def get_allowed_auths(self, username):
            return "keyboard-interactive" if username == "mfa" else "password,publickey"

        def check_channel_request(self, kind, channel_id):
            return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

        # ProxyJump may only reach this fixture itself.
        def check_channel_direct_tcpip_request(self, channel_id, origin, destination):
            if destination[0] not in ("127.0.0.1", "localhost") or destination[1] != listener.getsockname()[1]:
                return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED
            tunnels[channel_id] = destination
            return paramiko.OPEN_SUCCEEDED

        def check_channel_exec_request(self, channel, command):
            if command != b"sh":
                return False
            threading.Thread(target=run_shell, args=(channel,), daemon=True).start()
            return True

    def forward(channel, destination):
        upstream = socket.create_connection(("127.0.0.1", destination[1]))

        def pump(read, write):
            try:
                while True:
                    data = read(65536)
                    if not data:
                        break
                    write(data)
            except (OSError, EOFError):
                pass
            finally:
                try:
                    upstream.shutdown(socket.SHUT_WR)
                except OSError:
                    pass

        threading.Thread(target=pump, args=(channel.recv, upstream.sendall), daemon=True).start()
        pump(upstream.recv, channel.sendall)
        channel.close()

    def serve_client(client):
        transport = paramiko.Transport(client)
        transports.append(transport)
        stats["transports"] += 1
        write_stats()
        transport.add_server_key(host_key)
        try:
            transport.start_server(server=Server())
        except (EOFError, paramiko.SSHException):
            return
        while transport.is_active():
            channel = transport.accept(1)
            if channel is not None:
                destination = tunnels.pop(channel.get_id(), None)
                if destination is not None:
                    threading.Thread(target=forward, args=(channel, destination), daemon=True).start()
                channels.append(channel)

    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()

    def accept():
        while True:
            try:
                client, _ = listener.accept()
            except OSError:
                break
            threading.Thread(target=serve_client, args=(client,), daemon=True).start()

    def watch_for_drop():
        # Simulates a dropped network link when a test asks for it.
        marker = flags / "drop"
        while listener.fileno() != -1:
            if marker.exists():
                marker.unlink()
                for transport in transports:
                    transport.close()
            time.sleep(0.05)

    threading.Thread(target=accept, daemon=True).start()
    threading.Thread(target=watch_for_drop, daemon=True).start()

    env = os.environ.copy()
    env["ION_SERVER_ASSETS"] = str(dist)
    env["ION_ASKPASS_PROGRAM"] = str(askpass_program)
    env["ION_TEST_SSH_PORT"] = str(listener.getsockname()[1])
    env["ION_TEST_SSH_KEY"] = str(key_path)
    env["ION_TEST_SSH_STATS"] = str(stats_path)
    env["ION_TEST_PROJECT"] = posix(project)
    env["ION_TEST_HOME"] = str(home)
    # Keep the developer's real ~/.ssh (known_hosts, config, keys) out of the test.
    env["ION_SSH_DIR"] = str(ssh_dir)
    # Remote commands run under sh with this PATH (shims first).
    env["PATH"] = str(shim) + os.pathsep + env.get("PATH", "")
    if "--serve" in sys.argv:
        print(f"Fixture SSH server on 127.0.0.1:{listener.getsockname()[1]}", flush=True)
        print("Users: dev (password ion-integration, or the key below), mfa (code 123456)", flush=True)
        print(f"Key: {key_path}", flush=True)
        print(f"Set ION_SSH_DIR={ssh_dir} ION_SERVER_ASSETS={dist} ION_ASKPASS_PROGRAM={askpass_program}", flush=True)
        print(f"Project: {posix(project)}. Press Ctrl+C to stop.", flush=True)
        try:
            threading.Event().wait()
        except KeyboardInterrupt:
            pass
        raise SystemExit(0)
    if WINDOWS:
        print("Note: terminal assertions are skipped on Windows (ion-server's pty support is Unix-only).", flush=True)
    try:
        result = subprocess.run(
            ["cargo", "test", "-p", "ion_project", "--test", "ssh", "--", "--ignored", "--nocapture"],
            env=env, cwd=ROOT,
        )
        if result.returncode == 0:
            leftovers = list(home.glob(".ion/server/*/*/*.upload-*"))
            installed = list(home.glob(".ion/server/*/*/ion-server"))
            assert len(installed) == 1 and not leftovers, f"Unexpected server cache: {installed} {leftovers}"
            print("Server installed once, reused on later connections")
    finally:
        listener.close()
        for transport in transports:
            transport.close()
        for process in shells:
            try:
                process.wait(5)
            except subprocess.TimeoutExpired:
                kill_tree(process)
    raise SystemExit(result.returncode)
