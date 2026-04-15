//! Platform detection for matching release asset names.
//!
//! Mirrors the naming produced by `.github/workflows/release.yml`:
//! `crap-cms-linux-x86_64`, `crap-cms-linux-aarch64`, `crap-cms-windows-x86_64.exe`.
//! macOS is deliberately unsupported — the release workflow does not build a
//! macOS artifact, so claiming support here would only produce 404s at install
//! time.

use anyhow::{Result, bail};

/// Return the release-artifact filename for the host platform.
///
/// Errors on platforms with no published artifact so callers surface a clear
/// message instead of downloading a missing URL.
pub fn asset_name() -> Result<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    asset_name_for(os, arch)
}

/// Pure function for unit-testing the mapping without touching `std::env`.
pub fn asset_name_for(os: &str, arch: &str) -> Result<String> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("crap-cms-linux-x86_64".to_string()),
        ("linux", "aarch64") => Ok("crap-cms-linux-aarch64".to_string()),
        ("windows", "x86_64") => Ok("crap-cms-windows-x86_64.exe".to_string()),
        ("macos", _) => {
            bail!("macOS has no release artifact yet — build from source or install via Docker.")
        }
        (o, a) => bail!("Unsupported platform: {o}/{a}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_x86_64_maps_to_musl_artifact() {
        assert_eq!(
            asset_name_for("linux", "x86_64").unwrap(),
            "crap-cms-linux-x86_64"
        );
    }

    #[test]
    fn linux_aarch64_maps() {
        assert_eq!(
            asset_name_for("linux", "aarch64").unwrap(),
            "crap-cms-linux-aarch64"
        );
    }

    #[test]
    fn windows_x86_64_maps_to_exe() {
        assert_eq!(
            asset_name_for("windows", "x86_64").unwrap(),
            "crap-cms-windows-x86_64.exe"
        );
    }

    #[test]
    fn macos_is_explicitly_rejected() {
        let err = asset_name_for("macos", "x86_64").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("macOS"), "unexpected: {msg}");
    }

    #[test]
    fn unknown_platform_is_rejected() {
        let err = asset_name_for("freebsd", "riscv64").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Unsupported"), "unexpected: {msg}");
    }
}
