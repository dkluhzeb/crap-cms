//! SHA256 verification against a release's `SHA256SUMS` manifest.

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

/// Parse a `SHA256SUMS` manifest and find the line matching `asset_name`.
///
/// Manifest lines look like `<hex>  <filename>` (double-space). Filenames may
/// contain subdirectories depending on how the workflow was run — we compare
/// against the basename to be robust.
pub fn expected_hex_for(manifest: &str, asset_name: &str) -> Option<String> {
    for line in manifest.lines() {
        let mut parts = line.split_whitespace();
        let Some(hex) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        let basename = Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(name);
        if basename == asset_name {
            return Some(hex.to_string());
        }
    }
    None
}

/// Hex-encoded SHA256 of a file.
pub fn file_hex(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("opening {} for checksum", path.display()))?;

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).context("reading file for checksum")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hex_encode(&hasher.finalize()))
}

/// Verify `downloaded` matches `expected_hex`. Error on mismatch; succeed on match.
pub fn verify(downloaded: &Path, expected_hex: &str) -> Result<()> {
    let actual = file_hex(downloaded)?;
    if !actual.eq_ignore_ascii_case(expected_hex) {
        bail!(
            "checksum mismatch: expected {expected_hex}, got {actual} (file: {})",
            downloaded.display()
        );
    }
    Ok(())
}

/// Given a manifest + asset name, verify the file.
pub fn verify_against_manifest(downloaded: &Path, manifest: &str, asset_name: &str) -> Result<()> {
    let expected = expected_hex_for(manifest, asset_name)
        .ok_or_else(|| anyhow!("no SHA256 entry for {asset_name} in SHA256SUMS manifest"))?;
    verify(downloaded, &expected)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    const SAMPLE_MANIFEST: &str = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  crap-cms-linux-x86_64
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  crap-cms-linux-aarch64
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  crap-cms-windows-x86_64.exe
dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd  example.tar.gz
";

    #[test]
    fn finds_hex_by_basename() {
        let hex = expected_hex_for(SAMPLE_MANIFEST, "crap-cms-linux-x86_64").unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.starts_with("aaaa"));
    }

    #[test]
    fn finds_hex_when_path_has_directory() {
        // The release workflow sometimes writes `dir/file` — we must still match.
        let manifest = "aaaabbbb  dist/crap-cms-linux-x86_64";
        let hex = expected_hex_for(manifest, "crap-cms-linux-x86_64").unwrap();
        assert_eq!(hex, "aaaabbbb");
    }

    #[test]
    fn returns_none_when_asset_missing() {
        assert!(expected_hex_for(SAMPLE_MANIFEST, "nonexistent.bin").is_none());
    }

    #[test]
    fn verify_accepts_matching_hash() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"hello world").unwrap();
        // sha256("hello world") is a known constant.
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        verify(tmp.path(), expected).unwrap();
    }

    #[test]
    fn verify_rejects_mismatched_hash() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"hello world").unwrap();
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        let err = verify(tmp.path(), wrong).unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"));
    }

    #[test]
    fn verify_against_manifest_end_to_end() {
        // Build a manifest whose hash matches the file we write.
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"crap").unwrap();
        let hex = file_hex(tmp.path()).unwrap();

        let name = tmp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let manifest = format!("{hex}  {name}\n");

        verify_against_manifest(tmp.path(), &manifest, &name).unwrap();
    }

    #[test]
    fn verify_against_manifest_errors_when_asset_absent() {
        let err = verify_against_manifest(Path::new("/dev/null"), SAMPLE_MANIFEST, "absent.bin")
            .unwrap_err();
        assert!(format!("{err:#}").contains("no SHA256 entry"));
    }
}
