//! Minimal GitHub releases API client (blocking reqwest).
//!
//! Only the pieces we need: list releases, fetch a single asset, fetch
//! `SHA256SUMS`. We don't authenticate — these are public endpoints. Rate
//! limiting is unlikely to bite for `crap-cms update check` at 24-hour cadence.
//!
//! `SHA256SUMS` comes from the same release as the binary: it verifies
//! integrity (the download is complete and uncorrupted), not authenticity —
//! releases are not signed.

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
    time::Duration,
};

pub const DEFAULT_REPO: &str = "dkluhzeb/crap-cms";

/// Releases requested per API page (GitHub's maximum).
const RELEASES_PER_PAGE: usize = 100;

/// Upper bound on release pages walked — 10 000 releases; a guard against a
/// misbehaving API, not a limit any real repository reaches.
const MAX_RELEASE_PAGES: usize = 100;

/// Hard cap for a downloaded binary. Release binaries are tens of MiB; a body
/// past this is not our asset and would only fill the disk.
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

/// Build a reqwest blocking client with a User-Agent GitHub requires.
///
/// The blocking client's `timeout` bounds the wait for the response headers
/// and then *each* body read individually — it is an inactivity timeout, not
/// a total one — so a large download on a slow but progressing link is never
/// cut off, while a stalled connection fails after `idle`.
fn client(idle: Duration) -> Result<Client> {
    Client::builder()
        .user_agent(concat!("crap-cms/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(idle)
        .build()
        .context("building HTTP client")
}

/// Client for the small JSON / manifest requests.
fn api_client() -> Result<Client> {
    client(Duration::from_secs(30))
}

/// Client for binary downloads: a longer inactivity window, no total cap.
fn download_client() -> Result<Client> {
    client(Duration::from_mins(2))
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

/// Fetch one page (1-based) of releases, drafts included.
fn fetch_release_page(client: &Client, repo: &str, page: usize) -> Result<Vec<Release>> {
    let url = format!(
        "https://api.github.com/repos/{repo}/releases?per_page={RELEASES_PER_PAGE}&page={page}"
    );
    let resp = client
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("GitHub API returned HTTP {} for {}", resp.status(), url);
    }

    resp.json().context("parsing releases JSON")
}

/// Fetch all published releases, walking every API page. Includes
/// pre-releases so our alpha tags show up. Drafts are filtered out (they're
/// not publicly downloadable).
pub(super) fn list_releases(repo: &str) -> Result<Vec<Release>> {
    let client = api_client()?;
    let mut releases = Vec::new();

    for page in 1..=MAX_RELEASE_PAGES {
        let batch = fetch_release_page(&client, repo, page)?;
        let last = batch.len() < RELEASES_PER_PAGE;

        releases.extend(batch.into_iter().filter(|r| !r.draft));

        if last {
            break;
        }
    }

    Ok(releases)
}

/// Return the latest release tag — first non-draft release in the API's
/// order (which matches what `install.sh` already does). Reads only the
/// first page.
pub(super) fn latest_tag(repo: &str) -> Result<String> {
    let client = api_client()?;

    fetch_release_page(&client, repo, 1)?
        .into_iter()
        .find(|r| !r.draft)
        .map(|r| r.tag_name)
        .context("no releases published yet")
}

/// Copy at most `cap` bytes of `body` into `out`; error when the body is
/// larger.
fn copy_capped(body: impl Read, out: &mut impl Write, cap: u64) -> Result<u64> {
    let written = io::copy(&mut body.take(cap + 1), out).context("writing download")?;
    if written > cap {
        bail!("download is larger than {cap} bytes — refusing");
    }

    out.flush().context("flushing download")?;

    Ok(written)
}

/// Download a specific asset from a specific release tag into `dest`.
pub(super) fn download_asset(repo: &str, tag: &str, asset: &str, dest: &Path) -> Result<()> {
    let url = format!("https://github.com/{repo}/releases/download/{tag}/{asset}");
    let resp = download_client()?
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("download failed: HTTP {} for {}", resp.status(), url);
    }

    let mut file = File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    copy_capped(resp, &mut file, MAX_ASSET_BYTES)
        .with_context(|| format!("downloading {url} to {}", dest.display()))?;

    Ok(())
}

/// Hard cap for the `SHA256SUMS` manifest — a handful of hash lines. Anything
/// bigger is not our manifest; refuse instead of buffering it.
const MAX_SHA256SUMS_BYTES: u64 = 64 * 1024;

/// Fetch `SHA256SUMS` as text.
pub(super) fn fetch_sha256sums(repo: &str, tag: &str) -> Result<String> {
    let url = format!("https://github.com/{repo}/releases/download/{tag}/SHA256SUMS");
    let resp = api_client()?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_capped_accepts_body_at_the_cap() {
        let mut out = Vec::new();
        let n = copy_capped(&b"12345"[..], &mut out, 5).unwrap();
        assert_eq!(n, 5);
        assert_eq!(out, b"12345");
    }

    #[test]
    fn copy_capped_rejects_body_over_the_cap() {
        let mut out = Vec::new();
        let err = copy_capped(&b"123456"[..], &mut out, 5).unwrap_err();
        assert!(format!("{err:#}").contains("larger than 5 bytes"));
    }
}
