"""Package the Linux and macOS (Apple Silicon) servers and their versioned checksum manifest.

Linux servers must be static (musl). macOS servers link only the system
libraries every Mac has. Pass --linux-only to package just the Linux ones
(they're the only ones that can be cross-built on Windows).
"""
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
parser.add_argument("--linux-only", action="store_true")
args = parser.parse_args()
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
protocol = int(re.search(r"pub const PROTOCOL: u32 = (\d+);", (root / "crates/remote_protocol/src/lib.rs").read_text())[1])
args.output.mkdir(parents=True, exist_ok=True)
artifacts = []
# (Rust target, release file suffix, ELF machine or Mach-O CPU type). The
# manifest keys artifacts by target; file names use the same short platform
# names as the app downloads.
LINUX = [
    ("x86_64-unknown-linux-musl", "linux-x64", 62),
    ("aarch64-unknown-linux-musl", "linux-arm64", 183),
]
# CPU_ARCH_ABI64 | ARM.
MACOS = [("aarch64-apple-darwin", "macos-arm64", 0x0100000C)]


def check_elf(data, machine, source):
    if data[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", data, 18)[0] != machine:
        raise ValueError(f"Wrong ELF architecture: {source}")
    offset = struct.unpack_from("<Q", data, 32)[0]
    size, count = struct.unpack_from("<HH", data, 54)
    if any(struct.unpack_from("<I", data, offset + i * size)[0] in (2, 3) for i in range(count)):
        raise ValueError(f"Server must be static, without dynamic libraries or interpreter: {source}")


def check_macho(data, cpu, source):
    if data[:4] != b"\xcf\xfa\xed\xfe" or struct.unpack_from("<I", data, 4)[0] != cpu:
        raise ValueError(f"Wrong Mach-O architecture: {source}")


targets = [(*item, check_elf) for item in LINUX]
if not args.linux_only:
    targets += [(*item, check_macho) for item in MACOS]
for target, platform, machine, check in targets:
    source = args.input / target / "release/ion-server"
    data = source.read_bytes()
    check(data, machine, source)
    name = f"ion-server-{version}-{platform}"
    shutil.copyfile(source, args.output / name)
    (args.output / name).chmod(0o755)
    artifacts.append(dict(target=target, file=name, sha256=hashlib.sha256(data).hexdigest()))
    print(f"{target}: {len(data):,} bytes")
(args.output / "manifest.json").write_text(json.dumps(dict(version=version, protocol=protocol, artifacts=artifacts), indent=2) + "\n")
(args.output / "SHA256SUMS").write_text("".join(f"{item['sha256']}  {item['file']}\n" for item in artifacts))
