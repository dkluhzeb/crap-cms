//! `UpdateCmd` clap subcommand definition + `run` dispatcher.

use anyhow::{Result, bail};
use clap::{CommandFactory, Subcommand};
use clap_complete::Shell;

use super::{
    check::run_check, completions, install::run_install, install_latest::run_update_latest,
    list::run_list, uninstall::run_uninstall, use_action::run_use, where_action::run_where,
};

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
            run_use::<C>(&version, yes, force)
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
