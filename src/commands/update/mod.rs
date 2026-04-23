//! `crap-cms update` — nvm-style version manager for the crap-cms binary.
//!
//! Subcommands:
//! - `check` — compare current version to latest released version.
//! - `list` — list available remote tags + mark installed versions.
//! - `install <version>` — download + verify + stage a version in the store.
//! - `use <version>` — switch the `current` symlink to an installed version.
//! - `uninstall <version>` — remove a version from the store.
//! - `where` — print the path of the currently active binary (resolving `current`).
//! - (no subcommand) — shortcut for install-latest + use-latest.

pub mod cache;
pub mod checksum;
pub mod github;
pub mod platform;
pub mod store;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{CommandFactory, Subcommand};
use clap_complete::Shell;
use semver::Version;
use std::path::PathBuf;

use crate::cli;

mod completions;

#[derive(Subcommand, Debug)]
pub enum UpdateCmd {
    /// Check GitHub for a newer release. Exit code 0 if up-to-date, 1 if newer is available.
    Check,

    /// List available release tags, marking installed versions and the active one.
    List,

    /// Download and stage a specific version in the local store.
    Install {
        /// Version tag (e.g., `v0.1.0-alpha.5`). Prefix with `v` accepted or bare.
        version: String,
        /// Re-download even if this version is already installed.
        #[arg(long)]
        reinstall: bool,
    },

    /// Switch the `current` symlink to the given (already installed) version.
    Use {
        /// Version tag to activate.
        version: String,
    },

    /// Remove an installed version from the store.
    Uninstall {
        /// Version tag to remove.
        version: String,
    },

    /// Print the path of the currently active binary.
    Where,

    /// Generate shell completions (to stdout) or remove installed completion files.
    Completions {
        /// Shell to target (bash, zsh, fish, elvish, powershell). Omit with
        /// `--uninstall` to remove files for every supported shell.
        shell: Option<Shell>,

        /// Remove the installed completion file(s) instead of printing.
        #[arg(long)]
        uninstall: bool,
    },
}

/// Execute an `UpdateCmd`. `None` means "install latest + use latest".
pub fn run<C: CommandFactory>(cmd: Option<UpdateCmd>, yes: bool, force: bool) -> Result<()> {
    match cmd {
        Some(UpdateCmd::Check) => run_check(),
        Some(UpdateCmd::List) => run_list(),
        Some(UpdateCmd::Install { version, reinstall }) => {
            refuse_on_windows("install")?;
            run_install(&version, reinstall, force)
        }
        Some(UpdateCmd::Use { version }) => {
            refuse_on_windows("use")?;
            run_use::<C>(&version, force)
        }
        Some(UpdateCmd::Uninstall { version }) => run_uninstall(&version),
        Some(UpdateCmd::Where) => run_where(),
        Some(UpdateCmd::Completions { shell, uninstall }) => run_completions::<C>(shell, uninstall),
        None => {
            refuse_on_windows("update")?;
            run_update_latest::<C>(yes, force)
        }
    }
}

/// Windows self-update relies on symlinks under `~/.local/share/crap-cms/` and
/// a Unix-style PATH layout. Symlink creation on Windows requires Developer
/// Mode or admin privileges, which most users don't have enabled. Until we
/// build a Windows-native version store (MSIX or a copy-fallback shim) we
/// refuse the write paths cleanly and tell the user where to get updates.
/// Read-only subcommands (`check`, `list`, `where`) still work on Windows.
fn refuse_on_windows(verb: &str) -> Result<()> {
    if cfg!(windows) {
        bail!(
            "`crap-cms update {verb}` is not supported on Windows yet.\n\
             Please download the latest `crap-cms-windows-x86_64.exe` manually \
             from https://github.com/dkluhzeb/crap-cms/releases/latest"
        );
    }
    Ok(())
}

/// Print shell completions to stdout, or remove installed completion files
/// when `--uninstall` is passed.
fn run_completions<C: CommandFactory>(shell: Option<Shell>, uninstall: bool) -> Result<()> {
    if uninstall {
        match shell {
            Some(s) => {
                completions::uninstall_completions_for(s);
            }
            None => completions::uninstall_all_completions(),
        }
        return Ok(());
    }

    let Some(shell) = shell else {
        bail!(
            "missing <SHELL> argument.\n\
             Usage: `crap-cms update completions <SHELL>` to print completions, \
             or `crap-cms update completions --uninstall` to remove installed files."
        );
    };

    completions::print_completions::<C>(shell);
    Ok(())
}

/// Compare current crate version to the latest release tag.
fn run_check() -> Result<()> {
    let current = current_version();
    let latest = github::latest_tag(github::DEFAULT_REPO)?;
    let now = Utc::now();

    // Write the cache for the startup nudge regardless of the comparison result.
    if let Some(path) = cache::default_path() {
        let _ = cache::write_at(
            &path,
            &cache::UpdateCache {
                checked_at: now,
                latest: latest.clone(),
            },
        );
    }

    if is_newer(&latest, &current) {
        cli::info(&format!(
            "Newer release available: {latest} (current: {current})"
        ));
        cli::hint("Run `crap-cms update` to install and switch.");
        std::process::exit(1);
    }

    cli::success(&format!("Up to date ({current})."));
    Ok(())
}

/// Print all remote release tags, marking installed and active.
fn run_list() -> Result<()> {
    let releases = github::list_releases(github::DEFAULT_REPO)?;
    let store = store::Store::default_for_user().ok();
    let installed = store
        .as_ref()
        .and_then(|s| s.installed().ok())
        .unwrap_or_default();
    let active = store.as_ref().and_then(|s| s.active_version());

    for release in releases {
        let is_installed = installed.contains(&release.tag_name);
        let is_active = active.as_deref() == Some(&release.tag_name);
        let marker = match (is_active, is_installed) {
            (true, _) => "*",
            (false, true) => " ",
            _ => " ",
        };
        let suffix = match (is_active, is_installed, release.prerelease) {
            (true, _, true) => " (active, prerelease)",
            (true, _, false) => " (active)",
            (false, true, true) => " (installed, prerelease)",
            (false, true, false) => " (installed)",
            (_, _, true) => " (prerelease)",
            _ => "",
        };
        println!("{marker} {}{suffix}", release.tag_name);
    }
    Ok(())
}

/// Download + verify + install a specific version.
fn run_install(version: &str, reinstall: bool, force: bool) -> Result<()> {
    let version = normalize_tag(version);
    let store = store::Store::default_for_user()?;

    // Guard: running binary is outside the store → refuse unless --force.
    ensure_self_managed(&store, force)?;

    if !reinstall && store.installed()?.contains(&version) {
        cli::info(&format!(
            "{version} is already installed. Use `--reinstall` to redownload, or `crap-cms update use {version}` to activate it."
        ));
        return Ok(());
    }

    // Verify the tag exists in the remote release list before we hit any
    // download URL — gives the user a helpful "did you mean…" instead of a
    // raw HTTP 404 when they typo'd the version.
    let releases = github::list_releases(github::DEFAULT_REPO)?;
    if !releases.iter().any(|r| r.tag_name == version) {
        let mut msg = format!("version {version} is not a published release.");
        let tags: Vec<String> = releases
            .iter()
            .take(10)
            .map(|r| r.tag_name.clone())
            .collect();
        if !tags.is_empty() {
            msg.push_str("\n\nAvailable versions:\n  ");
            msg.push_str(&tags.join("\n  "));
        }
        msg.push_str("\n\nRun `crap-cms update list` to see the full list.");
        bail!(msg);
    }

    let asset = platform::asset_name()?;
    let tmp_dir = ScratchDir::new()?;
    let tmp_bin = tmp_dir.path().join(&asset);

    cli::info(&format!("Downloading {version}/{asset}..."));
    github::download_asset(github::DEFAULT_REPO, &version, &asset, &tmp_bin)?;

    cli::info("Verifying SHA256...");
    let sums = github::fetch_sha256sums(github::DEFAULT_REPO, &version)?;
    checksum::verify_against_manifest(&tmp_bin, &sums, &asset)?;

    let installed_path = store.install_binary(&version, &tmp_bin)?;
    cli::success(&format!(
        "Installed {version} at {}",
        installed_path.display()
    ));

    // Help the user discover the next step. `install` stages only; the user
    // has to explicitly `use` a version to activate it (rustup-style).
    match store.active_version() {
        Some(active) if active == version => {
            // Already active (e.g., `--reinstall` of the current version) —
            // no next step needed.
        }
        Some(active) => {
            cli::hint(&format!(
                "Active version is still {active}. Run `crap-cms update use {version}` to switch."
            ));
        }
        None => {
            cli::hint(&format!(
                "No version is active yet. Run `crap-cms update use {version}` to activate it."
            ));
        }
    }
    Ok(())
}

/// Tiny scoped tempdir — we don't depend on the `tempfile` crate at runtime.
/// Cleans up on drop (best-effort).
struct ScratchDir {
    path: std::path::PathBuf,
}

impl ScratchDir {
    fn new() -> Result<Self> {
        let base = std::env::temp_dir();
        let suffix = nanoid::nanoid!(12);
        let dir = base.join(format!("crap-cms-update-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { path: dir })
    }
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Switch the `current` symlink.
fn run_use<C: CommandFactory>(version: &str, force: bool) -> Result<()> {
    let version = normalize_tag(version);
    let store = store::Store::default_for_user()?;
    ensure_self_managed(&store, force)?;
    store.switch_to(&version)?;
    cli::success(&format!("Switched to {version}."));

    completions::install_completions::<C>();
    warn_if_path_misaligned(&store);
    Ok(())
}

/// Remove a version. Also removes installed shell completions if no
/// versions remain — they follow the tool, not any individual version.
fn run_uninstall(version: &str) -> Result<()> {
    let version = normalize_tag(version);
    let store = store::Store::default_for_user()?;
    store.uninstall(&version)?;
    cli::success(&format!("Removed {version}."));

    if store.installed().map(|v| v.is_empty()).unwrap_or(false) {
        completions::uninstall_all_completions();
    }

    Ok(())
}

/// Print the active binary's resolved path.
fn run_where() -> Result<()> {
    let store = store::Store::default_for_user()?;
    let link = store.current_link();
    if !link.exists() {
        bail!(
            "no active version — the `current` symlink does not exist at {}",
            link.display()
        );
    }
    let resolved =
        std::fs::read_link(&link).with_context(|| format!("reading {}", link.display()))?;
    println!("{}", resolved.display());
    Ok(())
}

/// `crap-cms update` (no args): install latest + switch to it.
fn run_update_latest<C: CommandFactory>(yes: bool, force: bool) -> Result<()> {
    let latest = github::latest_tag(github::DEFAULT_REPO)?;
    let current = current_version();

    if !is_newer(&latest, &current) {
        cli::success(&format!("Already on the latest release ({current})."));
        return Ok(());
    }

    cli::info(&format!("Current: {current}"));
    cli::info(&format!("Latest:  {latest}"));

    if !yes && !confirm(&format!("Install {latest} and switch to it?"))? {
        cli::warning("Aborted.");
        return Ok(());
    }

    run_install(&latest, false, force)?;
    run_use::<C>(&latest, force)?;
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn current_version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Accept both `v0.1.0-alpha.5` and `0.1.0-alpha.5` on input; emit with `v`.
fn normalize_tag(input: &str) -> String {
    if input.starts_with('v') {
        input.to_string()
    } else {
        format!("v{input}")
    }
}

/// Parse a `vX.Y.Z-…` tag into a `semver::Version` (strips the leading `v`).
fn parse_tag(tag: &str) -> Option<Version> {
    let trimmed = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(trimmed).ok()
}

/// Is `candidate` a newer release than `current`?
fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_tag(candidate), parse_tag(current)) {
        (Some(c), Some(n)) => c > n,
        _ => false, // conservative: if we can't parse, don't claim a newer one
    }
}

/// Refuse self-update when the running binary lives outside the user's store
/// (distro-managed install paths like `/usr/bin`, `/opt/...`, `/nix/...`).
/// `--force` bypasses.
fn ensure_self_managed(store: &store::Store, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let Ok(current_exe) = std::env::current_exe() else {
        return Ok(()); // can't figure out the path → don't block the user
    };
    // Resolve symlinks (e.g. our own `current` shim) before checking.
    let resolved = current_exe.canonicalize().unwrap_or(current_exe);

    if store.owns_path(&resolved) {
        return Ok(());
    }

    // Only refuse for paths that smell distro-managed. User-placed binaries
    // under arbitrary paths are allowed through (the install still lands in
    // the store, not over the running binary's location).
    if looks_distro_managed(&resolved) {
        bail!(
            "this binary is at {} — looks like a package-manager install.\n\
             Update via your package manager, or pass `--force` to install into the crap-cms store anyway.",
            resolved.display()
        );
    }
    Ok(())
}

fn looks_distro_managed(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/usr/")
        || s.starts_with("/opt/")
        || s.starts_with("/nix/")
        || s.starts_with("/bin/")
        || s.starts_with("/sbin/")
}

/// Resolve the first `crap-cms` executable on `$PATH`, matching what the user's
/// shell would pick when they type `crap-cms`.
fn resolve_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&candidate)
                && meta.permissions().mode() & 0o111 != 0
            {
                return Some(candidate);
            }
        }
        #[cfg(not(unix))]
        {
            return Some(candidate);
        }
    }
    None
}

/// After `update use` flips the internal `current` symlink, the user's shell
/// may still resolve `crap-cms` to an older binary elsewhere on `$PATH` —
/// e.g. a `/usr/local/bin/crap-cms` from a manual install, or no shim at all.
/// Surface this as an explicit warning so "Switched to X" never misleads.
fn warn_if_path_misaligned(store: &store::Store) {
    let Some(active) = store.active_version() else {
        return;
    };
    let expected = store.version_path(&active);
    let expected_canonical = expected.canonicalize().unwrap_or(expected.clone());

    let Some(on_path) = resolve_on_path("crap-cms") else {
        cli::warning("`crap-cms` is not on your PATH.");
        cli::hint(&format!(
            "Link the shim:  ln -sfn {} ~/.local/bin/crap-cms",
            store.current_link().display()
        ));
        cli::hint("Then make sure `~/.local/bin` is on your PATH.");
        return;
    };

    let on_path_canonical = on_path.canonicalize().unwrap_or(on_path.clone());
    if on_path_canonical == expected_canonical {
        return; // all wired up — the shell will pick up the active version.
    }

    cli::warning(&format!(
        "`crap-cms` on PATH resolves to {} — not the version you just activated.",
        on_path.display()
    ));
    cli::hint(&format!(
        "Point your shim at the store:  ln -sfn {} ~/.local/bin/crap-cms",
        store.current_link().display()
    ));
    cli::hint("(Or remove the conflicting binary and re-run `scripts/install.sh`.)");
}

fn confirm(prompt: &str) -> Result<bool> {
    use dialoguer::Confirm;
    Ok(Confirm::with_theme(&crate::cli::crap_theme())
        .with_prompt(prompt)
        .default(true)
        .interact()?)
}

/// Resolve the path the startup nudge should write the update cache to.
/// Exposed for tests.
pub fn cache_path() -> Option<PathBuf> {
    cache::default_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tag_adds_v_prefix() {
        assert_eq!(normalize_tag("0.1.0-alpha.5"), "v0.1.0-alpha.5");
        assert_eq!(normalize_tag("v0.1.0-alpha.5"), "v0.1.0-alpha.5");
    }

    #[test]
    fn is_newer_prerelease_order() {
        assert!(is_newer("v0.1.0-alpha.5", "v0.1.0-alpha.4"));
        assert!(!is_newer("v0.1.0-alpha.4", "v0.1.0-alpha.5"));
    }

    #[test]
    fn is_newer_stable_over_prerelease() {
        // semver: 1.0.0 > 1.0.0-alpha.5 (prereleases rank below)
        assert!(is_newer("v1.0.0", "v1.0.0-alpha.5"));
    }

    #[test]
    fn is_newer_same_version_is_false() {
        assert!(!is_newer("v0.1.0-alpha.5", "v0.1.0-alpha.5"));
    }

    #[test]
    fn is_newer_unparseable_is_false() {
        // Don't claim updates on junk input.
        assert!(!is_newer("nightly", "v0.1.0-alpha.5"));
        assert!(!is_newer("v0.1.0-alpha.5", "nightly"));
    }

    #[test]
    fn looks_distro_managed_recognises_system_paths() {
        assert!(looks_distro_managed(std::path::Path::new(
            "/usr/bin/crap-cms"
        )));
        assert!(looks_distro_managed(std::path::Path::new(
            "/opt/crap-cms/bin/crap-cms"
        )));
        assert!(looks_distro_managed(std::path::Path::new(
            "/nix/store/abc/bin/crap-cms"
        )));
    }

    #[test]
    fn looks_distro_managed_ignores_home_paths() {
        assert!(!looks_distro_managed(std::path::Path::new(
            "/home/someone/.local/bin/crap-cms"
        )));
        assert!(!looks_distro_managed(std::path::Path::new(
            "/tmp/my-install/crap-cms"
        )));
    }
}
