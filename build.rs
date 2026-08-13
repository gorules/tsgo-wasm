use std::io::Read;
use std::path::PathBuf;
use std::{env, fs};

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn main() {
    println!("cargo:rerun-if-changed=artifacts/tsgo.rev");
    println!("cargo:rerun-if-changed=artifacts/tsgo.sha256");
    println!("cargo:rerun-if-changed=artifacts/tsgo.wasm.zst");
    println!("cargo:rerun-if-env-changed=TSGO_WASM_FILE");

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("tsgo.wasm.zst");
    let sha = fs::read_to_string(root.join("artifacts/tsgo.sha256"))
        .unwrap()
        .trim()
        .to_string();

    if let Ok(existing) = fs::read(&out) {
        if sha256_hex(&existing) == sha {
            return;
        }
    }

    if let Ok(path) = env::var("TSGO_WASM_FILE") {
        fs::copy(&path, &out).unwrap();
        return;
    }

    let local = root.join("artifacts/tsgo.wasm.zst");
    if let Ok(bytes) = fs::read(&local) {
        if sha256_hex(&bytes) == sha {
            fs::write(&out, bytes).unwrap();
            return;
        }
    }

    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let url =
        format!("https://github.com/gorules/tsgo-wasm/releases/download/v{version}/tsgo.wasm.zst");
    let mut bytes = Vec::new();
    ureq::get(&url)
        .call()
        .unwrap_or_else(|e| panic!("failed to download {url}: {e}"))
        .into_reader()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(sha256_hex(&bytes), sha, "checksum mismatch for {url}");
    fs::write(&out, bytes).unwrap();
}
