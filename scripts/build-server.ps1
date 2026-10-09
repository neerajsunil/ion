$ErrorActionPreference = "Stop"
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
if ($LASTEXITCODE) { throw "Could not install Rust targets" }
foreach ($serverTarget in @("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl")) {
    cargo zigbuild -p ion_server --release --target $serverTarget
    if ($LASTEXITCODE) { throw "Server build failed for $serverTarget" }
}
python scripts/package-server.py --linux-only
if ($LASTEXITCODE) { throw "Server packaging failed" }
