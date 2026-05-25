//! Workspace task runner. Invoked via the `cargo xtask` alias.
//!
//! Each subcommand lives in its own module. Main is intentionally
//! thin: CLI parsing + dispatch only. No business logic.
//!
//! Subcommands:
//! - `gen-lua-types [--check]` — regenerates `types/crap.lua` from
//!   Rust source ([`gen_lua_types`]).
//! - `gen-template-doc [--check]` — regenerates
//!   `docs/src/admin-ui/reference/template-context.md` from the typed
//!   admin page contexts ([`gen_template_doc`]).
//!
//! Standard `cargo-xtask` pattern — keeps build-tool logic out of
//! `build.rs` (where it would re-run on every compile) and out of
//! shell scripts (where editor support and type safety go missing).
//!
//! ```bash
//! cargo xtask gen-lua-types              # regenerate types/crap.lua
//! cargo xtask gen-lua-types --check      # CI gate: fail if out of sync
//! cargo xtask gen-template-doc           # regenerate template-context.md
//! cargo xtask gen-template-doc --check   # CI gate: fail if out of sync
//! ```

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod drift;
mod gen_lua_types;
mod gen_template_doc;

#[derive(Parser)]
#[command(name = "xtask", about = "crap-cms workspace task runner")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Regenerate the static `types/crap.lua` Lua type definition file
    /// from Rust source. With `--check`, exits non-zero (and prints a
    /// diff) when the on-disk file diverges from what would be generated.
    GenLuaTypes {
        /// Verify the on-disk file matches; do not write. Use in CI.
        #[arg(long)]
        check: bool,
    },

    /// Regenerate `docs/src/admin-ui/reference/template-context.md`
    /// from the typed page-context structs. With `--check`, exits
    /// non-zero (and prints a diff) when the on-disk file diverges.
    GenTemplateDoc {
        /// Verify the on-disk file matches; do not write. Use in CI.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    let result = match Cli::parse().cmd {
        Cmd::GenLuaTypes { check } => gen_lua_types::run(check),
        Cmd::GenTemplateDoc { check } => gen_template_doc::run(check),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
