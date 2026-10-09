"""Package static Linux servers and their versioned checksum manifest."""
import argparse
import hashlib
import json
import pathlib
import re
import shutil
import struct
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument("--output", type=pathlib.Path, default=root / "target/server-dist")
parser.add_argument("--input", type=pathlib.Path, default=root / "target")
args = parser.parse_args()
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
protocol = int(re.search(r"pub const PROTOCOL: u32 = (\d+);", (root / "crates/remote_protocol/src/lib.rs").read_text())[1])
args.output.mkdir(parents=True, exist_ok=True)
artifacts = []
for target, machine in [("x86_64-unknown-linux-musl", 62), ("aarch64-unknown-linux-musl", 183)]:
    source = args.input / target / "release/ion-server"
    data = source.read_bytes()
    if data[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", data, 18)[0] != machine:
        raise ValueError(f"Wrong ELF architecture: {source}")
    offset = struct.unpack_from("<Q", data, 32)[0]
    size, count = struct.unpack_from("<HH", data, 54)
    if any(struct.unpack_from("<I", data, offset + i * size)[0] in (2, 3) for i in range(count)):
        raise ValueError(f"Server must be static, without dynamic libraries or interpreter: {source}")
    name = f"ion-server-{version}-{target}"
    shutil.copyfile(source, args.output / name)
    (args.output / name).chmod(0o755)
    artifacts.append(dict(target=target, file=name, sha256=hashlib.sha256(data).hexdigest()))
    print(f"{target}: {len(data):,} bytes, statically linked")
(args.output / "manifest.json").write_text(json.dumps(dict(version=version, protocol=protocol, artifacts=artifacts), indent=2) + "\n")
(args.output / "SHA256SUMS").write_text("".join(f"{item['sha256']}  {item['file']}\n" for item in artifacts))
