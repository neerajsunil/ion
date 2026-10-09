use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=ION_SERVER_ASSETS");
    let dir = env::var_os("ION_SERVER_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
                .join("../../target/server-dist")
        });
    let manifest = dir.join("manifest.json");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let mut generated = String::from("const BUNDLED: &[(&str, &str, &[u8])] = &[\n");
    if let Ok(bytes) = fs::read(&manifest) {
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("Invalid server manifest");
        // Stale assets (an older version or protocol) would install a server the
        // client then refuses; leave them out and fall back to the release download.
        let current = value["version"].as_str() == Some(remote_protocol::VERSION)
            && value["protocol"].as_u64() == Some(u64::from(remote_protocol::PROTOCOL));
        if !current {
            println!(
                "cargo:warning=Ignoring stale Ion server assets in {} (version {}, protocol {}; expected {}, {}). Run scripts/build-server.ps1 to rebuild them.",
                dir.display(),
                value["version"],
                value["protocol"],
                remote_protocol::VERSION,
                remote_protocol::PROTOCOL
            );
        }
        let artifacts = value["artifacts"].as_array().expect("Missing artifacts");
        for artifact in artifacts.iter().filter(|_| current) {
            let file = dir
                .join(artifact["file"].as_str().unwrap())
                .canonicalize()
                .expect("Missing server binary");
            println!("cargo:rerun-if-changed={}", file.display());
            generated.push_str(&format!(
                "({:?}, {:?}, include_bytes!({:?})),\n",
                artifact["target"].as_str().unwrap(),
                artifact["sha256"].as_str().unwrap(),
                file.to_str().unwrap()
            ));
        }
    }
    generated.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("server_assets.rs"),
        generated,
    )
    .unwrap();
}
