use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::{env, io::Result, path::PathBuf};

fn main() -> Result<()> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("content_descriptor.bin"))
        .compile_protos(&["proto/content.proto"], &["proto"])?;

    // Build hash for cache-busting static assets.
    // Hashes all files under static/ and templates/ so any change produces a new hash.
    let mut hasher = DefaultHasher::new();
    for dir in &["static", "templates"] {
        hash_dir(&mut hasher, dir);
    }
    let hash = format!("{:016x}", hasher.finish());
    println!("cargo:rustc-env=BUILD_HASH={hash}");

    Ok(())
}

fn hash_dir(hasher: &mut DefaultHasher, dir: &str) {
    let mut entries: Vec<_> = walkdir(dir);
    entries.sort();
    for path in entries {
        if let Ok(bytes) = std::fs::read(&path) {
            path.hash(hasher);
            bytes.hash(hasher);
        }
    }
}

fn walkdir(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walkdir(path.to_str().unwrap_or("")));
            } else {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
    out
}
