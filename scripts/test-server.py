"""Exercise the real server process: multiplexed RPC, shell and pty streams, atomic saves, search and OS events."""
import json
import pathlib
import shutil
import struct
import subprocess
import sys
import tempfile

binary = pathlib.Path(sys.argv[1]).resolve()

class Connection:
    """One `--stdio` connection; frames are length + tag + payload (0 JSON, 1 stdout, 2 stderr)."""
    def __init__(self, process):
        self.process = process
        self.next_id = 0
        self.pending = []

    def send(self, message):
        body = json.dumps(message).encode()
        self.process.stdin.write(struct.pack(">IB", len(body) + 1, 0) + body)
        self.process.stdin.flush()

    def send_data(self, stream, data):
        self.process.stdin.write(struct.pack(">IBQ", len(data) + 9, 1, stream) + data)
        self.process.stdin.flush()

    def read(self):
        """Returns ("message", dict) or ("stdout"/"stderr", stream, bytes)."""
        header = self.process.stdout.read(4)
        assert len(header) == 4, "Server closed unexpectedly"
        size = struct.unpack(">I", header)[0]
        body = self.process.stdout.read(size)
        assert len(body) == size
        tag = body[0]
        if tag == 0:
            return ("message", json.loads(body[1:]))
        assert tag in (1, 2), f"unknown tag {tag}"
        return (("stdout", "stderr")[tag - 1], struct.unpack(">Q", body[1:9])[0], body[9:])

    def request(self, method, **fields):
        self.next_id += 1
        self.send(dict(type="request", id=self.next_id, operation=dict(method=method, **fields)))
        kind, response = self.read()
        assert kind == "message" and response["type"] == "response" and response["id"] == self.next_id, response
        assert "Ok" in response["result"], response
        return response["result"]["Ok"]

    def collect(self, stream, until):
        """Reads frames for `stream` until `until(output)` or its Exit; returns (stdout, stderr, exit)."""
        out, err = b"", b""
        while True:
            frame = self.read()
            if frame[0] == "stdout" and frame[1] == stream:
                out += frame[2]
            elif frame[0] == "stderr" and frame[1] == stream:
                err += frame[2]
            elif frame[0] == "message" and frame[1].get("stream") == stream:
                if frame[1]["type"] == "exit":
                    return out, err, frame[1]
                if until(frame[1]):
                    return out, err, frame[1]

with tempfile.TemporaryDirectory(prefix="ion-server-test-") as temp:
    root = pathlib.Path(temp)
    with subprocess.Popen([binary, "--stdio"], stdin=subprocess.PIPE, stdout=subprocess.PIPE) as process:
        rpc = Connection(process)
        kind, hello = rpc.read()
        assert kind == "message" and hello["protocol"] == 2, hello
        file = root / "quote ' é.txt"
        file.write_text("old")
        rpc.request("save_text", path=str(file), text="héllo\nneedle\n", has_bom=True)
        assert file.read_bytes().startswith(b"\xef\xbb\xbf")
        assert rpc.request("read_text", path=str(file))["text"] == "héllo\nneedle\n"
        assert rpc.request("read_dir", path=str(root)) == [dict(name=file.name, is_dir=False)]
        assert rpc.request("index", root=str(root)) == [file.name]
        matches = rpc.request("search", root=str(root), query="needle", case_sensitive=True)
        assert matches["matches"][0]["line"] == 1
        assert matches["matches"][0]["preview"] == "needle"

        if shutil.which("sh"):
            spec = lambda command, cwd=None: dict(type="exec", command=command, cwd=cwd, pty=None, merge_stderr=False)
            rpc.send(dict(type="open", stream=1, spec=spec("echo hi; echo err >&2; exit 3")))
            out, err, exit = rpc.collect(1, lambda message: False)
            assert out.strip() == b"hi" and err.strip() == b"err", (out, err)
            assert exit["code"] == 3 and exit["error"] is None, exit
            rpc.send(dict(type="open", stream=2, spec=spec("cat")))
            rpc.send_data(2, b"round trip\n")
            rpc.send(dict(type="close_input", stream=2))
            out, err, exit = rpc.collect(2, lambda message: False)
            assert out == b"round trip\n" and exit["code"] == 0, (out, exit)
            rpc.send(dict(type="open", stream=3, spec=spec("pwd; echo err >&2", cwd=str(root))))
            out, err, exit = rpc.collect(3, lambda message: False)
            assert out.strip() and err.strip() == b"err", (out, err)
            rpc.send(dict(type="open", stream=4, spec=spec("sleep 60")))
            rpc.send(dict(type="close", stream=4))
            out, err, exit = rpc.collect(4, lambda message: False)
            assert exit["code"] != 0, exit
            if sys.platform != "win32":
                size = dict(cols=100, rows=30, width=0, height=0)
                rpc.send(dict(type="open", stream=5, spec=dict(type="exec", command="echo $TERM; stty size; read line; echo got:$line", cwd=None, pty=size, merge_stderr=False)))
                rpc.send_data(5, b"ping\n")
                out, err, exit = rpc.collect(5, lambda message: False)
                assert b"xterm-256color" in out and b"30 100" in out and b"got:ping" in out, out
                assert exit["code"] == 0, exit
        rpc.send(dict(type="open", stream=6, spec=dict(type="watch", root=str(root))))
        kind, event = rpc.read()
        assert event["type"] == "watch" and event["stream"] == 6 and event["event"]["ready"], event
        file.write_text("external change")
        while True:
            kind, event = rpc.read()
            if kind != "message":
                continue
            assert event["type"] == "watch" and event["event"]["error"] is None, event
            if any(path.endswith(file.name) for path in event["event"]["paths"]):
                break
        rpc.send(dict(type="close", stream=6))
        process.stdin.close()
        assert process.wait(timeout=10) == 0
print("Multiplexed RPC, shell streams, native search, atomic save and filesystem events passed")
