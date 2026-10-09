# Remote development over SSH

Open a folder on a Linux server; files, search, Git and terminals all run
there.

## Connecting

Choose **Connect to SSH…** from the Ion menu, the start page or the command
palette (**Remote: Connect to SSH…**).

- **Host** takes a host name, an IP address, `user@host`, `host:port` or a
  name from your `~/.ssh/config`. Under it, Ion shows who it will connect as.
- **Saved hosts** lists your recent connections and the hosts in your SSH
  config. Click one (or pick it with Up/Down and press Enter) to connect.
- **Username** is optional: it defaults to the config's `User`, otherwise
  your Windows user name, shown as the placeholder.
- **Port and private key** are under the disclosure arrow. Both are optional.
- **Tab** and **Shift+Tab** move between fields; **Enter** connects.

### Logging in

Ion runs the system OpenSSH client (`ssh`), so your agent, keys and config work
as they do in a terminal. On Windows this needs the **OpenSSH Client** optional
feature (Settings > System > Optional features); Ion uses
`C:\Windows\System32\OpenSSH\ssh.exe`, which talks to the Windows ssh-agent
service. When `ssh` asks for something (a password, a key passphrase or a
one-time code), Ion receives the question through `SSH_ASKPASS` (Ion itself is
the helper, over a loopback socket guarded by a random token) and shows it in
the dialog. A password typed in the form is used first, and also tried as the
passphrase for an encrypted key.

You log in **once per connection**. Every terminal, Git command, search and the
file watcher share that one SSH connection, so nothing asks again. Answers are
kept in memory only for the life of the connection, and are never written to
disk.

### Host keys

On the first connection to a host (or a jump host), the dialog shows its
SHA-256 fingerprint. Check it against one from a trusted source, then choose
**Trust and Connect**. `ssh` adds the key to your `known_hosts` (or the
config's `UserKnownHostsFile`), as usual. A host whose key changed is
refused, with an explanation.

### SSH config

`ssh` reads `~/.ssh/config` itself, so everything it supports applies,
including `ProxyJump`, `ProxyCommand`, `Match` and `Include`. Ion reads the
file only to list host aliases and preview the user and port in the dialog.
The jump host's login questions and host key are handled like the target's.

## Choosing the folder

After logging in, **Open a folder** starts in your home folder and lists its
folders right away. Recent folders on that host come first.

- Type a path (`~/app`, `/srv/app`, or relative to home); suggestions follow.
- **Tab** completes the highlighted folder, **Up/Down** choose, a double-click
  opens one, **Enter** opens the path in the field.
- **Disconnect** closes the connection without opening anything.

The project opens in its own window, with **SSH: user@host** in the status
bar and the folder name and host in the title. Recent remote projects appear
on the start page and in the project menu; picking one reconnects and opens it
directly.

In a remote window:

- **Open Folder** (Ctrl+O) browses the same server, in the same window.
- The project menu lists recent folders on that host.
- **Open settings.json** opens your local settings file in the same window.
- **Remote: Disconnect SSH** closes the window (asking about unsaved changes)
  and ends the connection.

## Files, Git and terminals

The file tree, Go to File, Find in Files, saves, new files and folders,
rename, copy, move, delete and Git all run on the server. Remote saves write a
temporary file next to the original and replace it atomically once the upload
succeeds, keeping its permissions and following symlinks. Remote deletion is
permanent and asks first; there's no Recycle Bin on the server.

Git runs the server's `git`, with its error output kept apart from its results
and no time limit, so commit hooks can take as long as they need.

Terminals start the server's login shell in the project folder, in a pty
created by `ion-server` on the shared connection. Builds, agents and other commands run on the server. The local
terminal shell setting applies only to local projects.

Live updates use the server's file system events (Linux inotify), with no
polling. **File: Refresh Project** reloads the tree, file index, Git status
and open files if watching isn't available. Unsaved changes are never
overwritten without asking.

## When the connection drops

`ssh` sends keepalives every 15 seconds and ends after four unanswered ones,
so Ion notices a dead link within about a minute. It then
reconnects on its own, using the login answers it already holds:

- The status bar shows **Reconnecting to user@host…**, then **SSH: user@host**
  again once it's back. If it gives up (after about two and a half minutes),
  it shows **Disconnected · Reconnect**; click it to try again.
- If the server needs a fresh one-time code, the window asks for it.
- Open files stay open, with unsaved changes kept. After reconnecting, Ion
  restarts live updates and refreshes the tree, Git status and open files.
- Terminals show **Press Enter to start a new shell**: the old shell ended
  with the connection.

## How it works

On connecting, Ion checks the server's architecture (Linux x86-64 or ARM64)
and installs a small static `ion-server` binary in
`~/.ion/server/<version>/<architecture>-<checksum>/`, over the SSH connection
itself. Its SHA-256 checksum is verified before and after the upload, and the
cached copy is reused while it's intact and reports Ion's version and
protocol. Otherwise the matching server is installed and the other versions
under `~/.ion/server` are removed. Binaries bundled with Ion are used
first; builds without them download the matching release (only when an
install is needed) on the **local** machine, so the server needs no internet
access, Python or root. Bump the workspace version whenever the protocol
changes: releases are looked up by version. The server
doesn't listen on a port or install a service: it runs only while Ion is
connected, behind the host's SSH daemon.

The connection is one `ssh` process: its stdio first carries a short shell
bootstrap (check the architecture, install and verify the server), then becomes
`ion-server --stdio`. File requests, searches, the watcher, Git and terminals
are multiplexed over it as tagged frames (protocol v2). A reader thread
dispatches responses and stream output; a writer thread sends queued frames; a
ticker thread sleeps until a terminal needs a deferred flush. Requests run
concurrently on the server, so a search never holds up reads and saves, and a
new search cancels the previous one. Only the
requested file contents, paths and bounded search previews cross the network.
Language servers aren't hosted remotely yet.

## Building and testing

Remote projects run the system OpenSSH client, so there is nothing extra to build.
On Windows the client is an optional feature (Settings > System > Optional
features > OpenSSH Client).

Build Linux assets before packaging Ion:

```powershell
python -m pip install cargo-zigbuild==0.23.4 ziglang==0.16.0
./scripts/build-server.ps1
cargo dev
```

The packaging script verifies that both files are static ELF binaries for the
correct architecture and generates `target/server-dist/manifest.json` and
`SHA256SUMS`. `ion_remote` embeds these files at build time. Set
`ION_SERVER_ASSETS` to a packaged directory to override the assets for development
or offline distribution. Without assets Ion obtains its versioned public release.
Remote users never need the developer build tools.

`cargo t` covers protocol framing, version compatibility, checksum validation,
atomic save behavior and the existing tests. Build `ion_server`, then run
`python scripts/test-server.py target/debug/ion-server.exe` on Windows (omit
`.exe` on Linux) to exercise the actual server's persistent RPC and OS watcher.

For the SSH round trip, install Paramiko and run `python scripts/test-ssh.py`.
It drives the real system `ssh` against a loopback Paramiko server with
disposable host and Ed25519 keys, and tests host-key trust, password, key and
one-time-code logins (through the askpass helper, built as
`crates/remote/examples/askpass.rs`), `ProxyJump`, server upload and cache
reuse, file operations, search, remote Git, the watcher and reconnecting after
a dropped link; on Linux it also covers terminals. The remote shell is Git Bash
(`/bin/sh` on Linux). On Windows it runs the native server through
`scripts/test-ssh-proxy.py`, which translates MSYS paths, and skips terminals
because the server's pty support is Unix-only. The Linux release workflow executes each static binary
and tests RPC and inotify on its own native architecture before publishing.
