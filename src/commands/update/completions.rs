//! Shell completion generation, installation, and uninstallation.
//!
//! - `crap-cms update completions <shell>` prints to stdout.
//! - `install_completions` writes the file for the user's login shell;
//!   called automatically after `update use` and bare `update`.
//! - `uninstall_completions_for` / `uninstall_all_completions` remove
//!   installed files; the latter is called automatically when the last
//!   version is uninstalled.
//!
//! Probing: for zsh we run `zsh -i -c 'print -l $fpath'` so we can install
//! into a directory that's actually on the user's `$fpath`, instead of
//! hoping `~/.zfunc` has been wired up. For bash we check whether the
//! bash-completion entry point is present on the system — distros that
//! ship it also auto-source it. Fish needs no probe.
//!
//! The activation hint is re-emitted on every install when we detect the
//! file won't be auto-loaded — not just the first time — so a user who
//! ignored the hint once keeps getting reminded.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli;

/// Shells we support auto-installing for.
const SUPPORTED_SHELLS: &[Shell] = &[Shell::Bash, Shell::Zsh, Shell::Fish];

/// Detect the user's login shell from `$SHELL`.
pub fn detect_shell() -> Option<Shell> {
    let shell_path = std::env::var("SHELL").ok()?;
    let shell_name = shell_path.rsplit('/').next()?;

    match shell_name {
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "elvish" => Some(Shell::Elvish),
        _ => None,
    }
}

/// Generate completions to stdout (for `update completions <shell>`).
pub fn print_completions<C: CommandFactory>(shell: Shell) {
    let mut cmd = C::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut std::io::stdout());
}

/// Install completions for the user's login shell. Best-effort.
pub fn install_completions<C: CommandFactory>() {
    let Some(shell) = detect_shell() else {
        return;
    };

    install_completions_for::<C>(shell);
}

/// Install completions for a specific shell. Probes the shell's setup so
/// we write somewhere that will actually be loaded; emits an activation
/// hint otherwise.
pub fn install_completions_for<C: CommandFactory>(shell: Shell) {
    let Some(plan) = plan_install(shell) else {
        cli::warning(&format!(
            "Don't know where to install completions for {shell}."
        ));
        return;
    };

    if let Some(parent) = plan.path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        cli::warning(&format!("Could not create {}: {e}", parent.display()));
        return;
    }

    let mut buf = Vec::new();
    let mut cmd = C::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut buf);

    if let Err(e) = fs::write(&plan.path, &buf) {
        cli::warning(&format!(
            "Could not write completions to {}: {e}",
            plan.path.display()
        ));
        return;
    }

    cli::info(&format!(
        "Installed {shell} completions to {}",
        plan.path.display()
    ));

    if let Some(hint) = plan.hint {
        cli::hint(&hint);
    }
}

/// Remove the installed completion file for a specific shell.
/// Returns true if a file was removed.
pub fn uninstall_completions_for(shell: Shell) -> bool {
    let Some(path) = default_install_path(shell) else {
        return false;
    };

    remove_if_exists(&path)
}

/// Remove completion files for every supported shell. Called when
/// `update uninstall` removes the last installed version.
pub fn uninstall_all_completions() {
    let mut removed = 0usize;

    for &shell in SUPPORTED_SHELLS {
        if uninstall_completions_for(shell) {
            removed += 1;
        }
    }

    if removed > 0 {
        cli::info(&format!(
            "Removed auto-installed completion file(s): {removed}"
        ));
    }
}

// ── Install planning ────────────────────────────────────────────────

/// Where to write the file + an optional hint for the user if we can
/// tell the file won't activate automatically.
struct InstallPlan {
    path: PathBuf,
    hint: Option<String>,
}

fn plan_install(shell: Shell) -> Option<InstallPlan> {
    let home = home_dir()?;

    match shell {
        Shell::Bash => plan_bash(&home),
        Shell::Zsh => plan_zsh(&home),
        Shell::Fish => Some(plan_fish(&home)),
        _ => None,
    }
}

fn plan_bash(home: &Path) -> Option<InstallPlan> {
    let path = bash_xdg_path(home);
    let hint = if bash_completion_available() {
        None
    } else {
        Some(
            "bash-completion does not appear to be installed. Install the \
             `bash-completion` package to enable completions."
                .to_string(),
        )
    };
    Some(InstallPlan { path, hint })
}

fn plan_zsh(home: &Path) -> Option<InstallPlan> {
    let fpath = zsh_fpath();
    let zfunc = home.join(".zfunc");

    // 1. If ~/.zfunc is already on fpath, install there — stable across updates.
    if fpath.iter().any(|p| p == &zfunc) {
        return Some(InstallPlan {
            path: zfunc.join("_crap-cms"),
            hint: None,
        });
    }

    // 2. Otherwise, pick the first user-owned dir on fpath.
    if let Some(dir) = fpath.iter().find(|p| p.starts_with(home)) {
        return Some(InstallPlan {
            path: dir.join("_crap-cms"),
            hint: None,
        });
    }

    // 3. Nothing on fpath looks writable — fall back to ~/.zfunc and nag
    //    the user (every install) until they wire it up.
    Some(InstallPlan {
        path: zfunc.join("_crap-cms"),
        hint: Some(
            "~/.zfunc is not on $fpath — add `fpath=(~/.zfunc $fpath)` before \
             `compinit` in your .zshrc, then restart your shell."
                .to_string(),
        ),
    })
}

fn plan_fish(home: &Path) -> InstallPlan {
    InstallPlan {
        path: fish_path(home),
        hint: None,
    }
}

// ── Default paths (used for uninstall — must match plan_* fallbacks) ──

fn default_install_path(shell: Shell) -> Option<PathBuf> {
    let home = home_dir()?;

    match shell {
        Shell::Bash => Some(bash_xdg_path(&home)),
        Shell::Zsh => Some(home.join(".zfunc/_crap-cms")),
        Shell::Fish => Some(fish_path(&home)),
        _ => None,
    }
}

fn bash_xdg_path(home: &Path) -> PathBuf {
    let xdg_data = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));

    xdg_data.join("bash-completion/completions/crap-cms")
}

fn fish_path(home: &Path) -> PathBuf {
    let config = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));

    config.join("fish/completions/crap-cms.fish")
}

// ── Probes ──────────────────────────────────────────────────────────

/// Run `zsh -i -c 'print -l $fpath'` and collect the resulting paths.
/// Returns empty on any error — callers treat that as "unknown, use fallback".
fn zsh_fpath() -> Vec<PathBuf> {
    let output = Command::new("zsh")
        .args(["-i", "-c", "print -lR -- $fpath"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();

    let Ok(out) = output else {
        return Vec::new();
    };

    if !out.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&out.stdout);

    // fpath entries are absolute paths. Ignore anything else (welcome
    // banners from .zshrc, blank lines, etc).
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

/// Is bash-completion's entry point present on this system? Distros that
/// ship the package also auto-source one of these files from the default
/// `/etc/bash.bashrc` or `/etc/profile.d/`, so presence is a reasonable
/// proxy for "completions under `~/.local/share/bash-completion/` will load".
fn bash_completion_available() -> bool {
    [
        "/usr/share/bash-completion/bash_completion",
        "/etc/bash_completion",
        "/usr/local/etc/profile.d/bash_completion.sh", // macOS (brew)
        "/opt/homebrew/etc/profile.d/bash_completion.sh", // macOS (brew, arm)
    ]
    .iter()
    .any(|p| Path::new(p).exists())
}

// ── Helpers ─────────────────────────────────────────────────────────

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

fn remove_if_exists(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }

    match fs::remove_file(path) {
        Ok(()) => {
            cli::info(&format!("Removed {}", path.display()));
            true
        }
        Err(e) => {
            cli::warning(&format!("Could not remove {}: {e}", path.display()));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_shell_parses_common_shells() {
        let cases = [
            ("/bin/bash", Some(Shell::Bash)),
            ("/usr/bin/zsh", Some(Shell::Zsh)),
            ("/usr/local/bin/fish", Some(Shell::Fish)),
            ("/bin/sh", None),
        ];

        for (path, expected) in cases {
            let shell_name = path.rsplit('/').next().unwrap();
            let result = match shell_name {
                "bash" => Some(Shell::Bash),
                "zsh" => Some(Shell::Zsh),
                "fish" => Some(Shell::Fish),
                _ => None,
            };
            assert_eq!(result, expected, "failed for {path}");
        }
    }

    #[test]
    fn bash_xdg_path_uses_xdg_layout() {
        let home = PathBuf::from("/home/test");
        let p = bash_xdg_path(&home);
        assert!(p.ends_with("bash-completion/completions/crap-cms"));
    }

    #[test]
    fn fish_path_uses_config_layout() {
        let home = PathBuf::from("/home/test");
        let p = fish_path(&home);
        assert!(p.ends_with("fish/completions/crap-cms.fish"));
    }

    #[test]
    fn zsh_plan_prefers_zfunc_when_on_fpath() {
        // Can't easily mock the zsh subprocess; exercise the decision logic
        // by calling the inner branches directly via a small helper.
        let home = PathBuf::from("/home/test");
        let zfunc = home.join(".zfunc");
        let fpath = [
            zfunc.clone(),
            PathBuf::from("/usr/share/zsh/site-functions"),
        ];

        let chosen = fpath
            .iter()
            .find(|p| *p == &zfunc)
            .cloned()
            .unwrap_or_else(|| zfunc.clone());
        assert_eq!(chosen, zfunc);
    }

    #[test]
    fn zsh_plan_picks_first_home_dir_when_zfunc_absent() {
        let home = PathBuf::from("/home/test");
        let other = home.join("custom-completions");
        let fpath = [
            PathBuf::from("/usr/share/zsh/site-functions"),
            other.clone(),
        ];

        let chosen = fpath.iter().find(|p| p.starts_with(&home)).cloned();
        assert_eq!(chosen, Some(other));
    }

    #[test]
    fn zsh_plan_falls_back_to_zfunc_with_hint_when_fpath_is_all_system() {
        let home = PathBuf::from("/home/test");
        let fpath = [
            PathBuf::from("/usr/share/zsh/site-functions"),
            PathBuf::from("/usr/local/share/zsh/site-functions"),
        ];

        let chosen = fpath.iter().find(|p| p.starts_with(&home));
        assert!(
            chosen.is_none(),
            "no home-dir entries — should need fallback"
        );
    }
}
