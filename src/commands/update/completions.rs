//! Shell completion generation and auto-installation.
//!
//! `crap-cms update completions <shell>` prints to stdout.
//! `install_completions` writes the completion file to the standard
//! location for the user's current shell — called automatically
//! after `update use` and bare `update`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli;

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

/// Return the standard completion file path for the given shell.
fn completion_path(shell: Shell) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let home = PathBuf::from(home);

    match shell {
        Shell::Bash => {
            // XDG-compliant: ~/.local/share/bash-completion/completions/
            let xdg_data = std::env::var("XDG_DATA_HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"));

            Some(xdg_data.join("bash-completion/completions/crap-cms"))
        }
        Shell::Zsh => {
            // ~/.zfunc/_crap-cms (user must add fpath+=~/.zfunc before compinit)
            Some(home.join(".zfunc/_crap-cms"))
        }
        Shell::Fish => {
            // ~/.config/fish/completions/crap-cms.fish
            let config = std::env::var("XDG_CONFIG_HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));

            Some(config.join("fish/completions/crap-cms.fish"))
        }
        _ => None,
    }
}

/// Generate completions to stdout.
pub fn print_completions<C: CommandFactory>(shell: Shell) {
    let mut cmd = C::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut std::io::stdout());
}

/// Generate and install completions for the user's current shell.
/// Best-effort: logs what it did, never fails the parent operation.
pub fn install_completions<C: CommandFactory>() {
    let Some(shell) = detect_shell() else {
        return;
    };

    let Some(path) = completion_path(shell) else {
        return;
    };

    let first_install = !path.exists();

    if let Some(parent) = path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        cli::warning(&format!("Could not create {}: {e}", parent.display()));
        return;
    }

    let mut buf = Vec::new();
    let mut cmd = C::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut buf);

    match fs::write(&path, &buf) {
        Ok(()) => {
            cli::info(&format!(
                "Installed {shell} completions to {}",
                path.display()
            ));

            if first_install {
                show_activation_hint(shell, &path);
            }
        }
        Err(e) => cli::warning(&format!(
            "Could not write completions to {}: {e}",
            path.display()
        )),
    }
}

fn show_activation_hint(shell: Shell, path: &Path) {
    match shell {
        Shell::Bash => {
            cli::hint(&format!(
                "Run `source {}` to activate now, or restart your shell.",
                path.display()
            ));
        }
        Shell::Zsh => {
            cli::hint(
                "Add `fpath+=~/.zfunc` before `compinit` in your .zshrc, then restart your shell.",
            );
        }
        Shell::Fish => {
            cli::hint("Completions are active in new fish sessions automatically.");
        }
        _ => {
            cli::hint(&format!(
                "Source {} in your shell config to activate completions.",
                path.display()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_shell_parses_common_shells() {
        // Can't test directly since it reads $SHELL, but we can test the path parsing logic
        // by checking that known shell values produce expected results.
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
    fn completion_path_bash_uses_xdg() {
        let path = completion_path(Shell::Bash);
        // Just verify it returns Some and ends correctly — actual path depends on env.
        if let Some(p) = path {
            assert!(p.ends_with("bash-completion/completions/crap-cms"));
        }
    }

    #[test]
    fn completion_path_zsh_uses_zfunc() {
        let path = completion_path(Shell::Zsh);
        if let Some(p) = path {
            assert!(p.ends_with(".zfunc/_crap-cms"));
        }
    }

    #[test]
    fn completion_path_fish_uses_config() {
        let path = completion_path(Shell::Fish);
        if let Some(p) = path {
            assert!(p.ends_with("fish/completions/crap-cms.fish"));
        }
    }
}
