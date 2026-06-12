//! Minimal GitHub releases API client (blocking reqwest).
//!
//! Only the pieces we need: list releases, fetch a single asset, fetch
//! `SHA256SUMS`. We don't authenticate — these are public endpoints. Rate
//! limiting is unlikely to bite for `crap-cms update check` at 24-hour cadence.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    fs::File,
    io::{Read as _, Write},
    path::Path,
    time::Duration,
};

pub const DEFAULT_REPO: &str = "dkluhzeb/crap-cms";

/// Build a reqwest blocking client with a User-Agent GitHub requires.
fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("crap-cms/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("building HTTP client")
}

/// Single release as returned by the GitHub API.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
}

/// Fetch all published releases. Includes pre-releases so our alpha tags
/// show up. Drafts are filtered out (they're not publicly downloadable).
pub(super) fn list_releases(repo: &str) -> Result<Vec<Release>> {
    let url = format!("https://api.github.com/repos/{repo}/releases");
    let resp = client()?
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("GitHub API returned HTTP {} for {}", resp.status(), url);
    }
    let releases: Vec<Release> = resp.json().context("parsing releases JSON")?;
    Ok(releases.into_iter().filter(|r| !r.draft).collect())
}

/// Return the latest release tag — first non-draft release in the API's
/// order (which matches what `install.sh` already does).
pub(super) fn latest_tag(repo: &str) -> Result<String> {
    let releases = list_releases(repo)?;
    releases
        .into_iter()
        .next()
        .map(|r| r.tag_name)
        .context("no releases published yet")
}

/// Download a specific asset from a specific release tag into `dest`.
pub(super) fn download_asset(repo: &str, tag: &str, asset: &str, dest: &Path) -> Result<()> {
    let url = format!("https://github.com/{repo}/releases/download/{tag}/{asset}");
    let mut resp = client()?
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("download failed: HTTP {} for {}", resp.status(), url);
    }
    let mut file = File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    std::io::copy(&mut resp, &mut file).with_context(|| format!("writing {}", dest.display()))?;
    file.flush().ok();
    Ok(())
}

/// Hard cap for the `SHA256SUMS` manifest — a handful of hash lines. Anything
/// bigger is not our manifest; refuse instead of buffering it.
const MAX_SHA256SUMS_BYTES: u64 = 64 * 1024;

/// Fetch `SHA256SUMS` as text.
pub(super) fn fetch_sha256sums(repo: &str, tag: &str) -> Result<String> {
    let url = format!("https://github.com/{repo}/releases/download/{tag}/SHA256SUMS");
    let resp = client()?
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("no SHA256SUMS published for {tag} (HTTP {})", resp.status());
    }

    let mut body = String::new();
    resp.take(MAX_SHA256SUMS_BYTES)
        .read_to_string(&mut body)
        .context("reading SHA256SUMS body")?;
    if body.len() as u64 == MAX_SHA256SUMS_BYTES {
        bail!("SHA256SUMS is unexpectedly large (> {MAX_SHA256SUMS_BYTES} bytes) — refusing");
    }

    Ok(body)
}
