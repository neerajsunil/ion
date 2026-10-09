"""Windows-only helper for scripts/test-ssh.py: runs the native ion-server.exe
behind the fixture's Git Bash "remote".

The fake remote host speaks MSYS paths (/c/Users/...) in its shell, but the
native server needs Windows paths (C:/Users/...). This proxy sits between the
SSH channel and the server, rewriting path strings in JSON messages and in the
server's output data. Linux needs none of this.

Usage: test-ssh-proxy.py <native-server> --stdio
"""
import json
import re
import struct
import subprocess
import sys
import threading

MSYS = re.compile(r"(?<![\w/])/([a-zA-Z])/")
DRIVE = re.compile(r"(?://\?/)?([A-Za-z]):[/\\]")


def to_windows(value):
    if isinstance(value, dict):
        return {key: to_windows(item) for key, item in value.items()}
    if isinstance(value, list):
        return [to_windows(item) for item in value]
    if isinstance(value, str):
        return MSYS.sub(lambda m: m.group(1).upper() + ":/", value)
    return value


def to_msys(value):
    if isinstance(value, dict):
        return {key: to_msys(item) for key, item in value.items()}
    if isinstance(value, list):
        return [to_msys(item) for item in value]
    if isinstance(value, str) and DRIVE.search(value):
        value = value.replace("\\", "/")
        return DRIVE.sub(lambda m: "/" + m.group(1).lower() + "/", value)
    return value


def read_exact(stream, count):
    data = bytearray()
    while len(data) < count:
        chunk = stream.read(count - len(data))
        if not chunk:
            return None
        data.extend(chunk)
    return bytes(data)


def frames(source, sink, message, data):
    while True:
        header = read_exact(source, 4)
        if header is None:
            break
        body = read_exact(source, struct.unpack(">I", header)[0])
        if body is None:
            break
        if body[0] == 0:
            body = b"\0" + json.dumps(message(json.loads(body[1:])), ensure_ascii=False).encode()
        elif data is not None:
            body = body[:9] + data(body[9:])
        sink.write(struct.pack(">I", len(body)) + body)
        sink.flush()


def output_data(chunk):
    text = chunk.decode("utf-8", "surrogateescape")
    return to_msys(text).encode("utf-8", "surrogateescape")


def main():
    process = subprocess.Popen(sys.argv[1:], stdin=subprocess.PIPE, stdout=subprocess.PIPE)

    def feed():
        try:
            frames(sys.stdin.buffer, process.stdin, to_windows, None)
        finally:
            process.stdin.close()

    threading.Thread(target=feed, daemon=True).start()
    frames(process.stdout, sys.stdout.buffer, to_msys, output_data)
    sys.exit(process.wait())


main()
