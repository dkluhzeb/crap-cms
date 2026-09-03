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
//! - `gen-proto [--check]` — regenerates the CRUD request message
//!   bodies in `proto/content.proto` from the pinned wire spec
//!   ([`gen_proto`]).
//! - `gen-wire-doc [--check]` — regenerates
//!   `docs/src/reference/operation-options.md` from the wire model
//!   ([`gen_wire_doc`]).
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
//! cargo xtask gen-proto                  # regenerate proto CRUD messages
//! cargo xtask gen-proto --check          # CI gate: fail if out of sync
//! cargo xtask gen-wire-doc               # regenerate operation-options.md
//! cargo xtask gen-wire-doc --check       # CI gate: fail if out of sync
//! ```

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod drift;
mod gen_doc_tables;
mod gen_lua_types;
mod gen_proto;
mod gen_template_doc;
mod gen_wire_doc;

#[derive(Parser)]
#[command(name = "xtask", about = "crap-cms workspace task runner")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

// The shared `Gen` prefix is semantic: each variant mirrors its `gen-*`
// CLI subcommand name, which clap derives from the variant.
#[allow(clippy::enum_variant_names)]
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

    /// Regenerate the generated doc tables (slots guide region,
    /// css-variables reference, …). With `--check`, exits non-zero when
    /// any target diverges from its Rust/CSS source of truth.
    GenDocTables {
        /// Verify the on-disk docs match; do not write. Use in CI.
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

    /// Regenerate the CRUD request message bodies in
    /// `proto/content.proto` from the pinned wire spec. With `--check`,
    /// exits non-zero (and prints a diff) when the on-disk file diverges.
    GenProto {
        /// Verify the on-disk file matches; do not write. Use in CI.
        #[arg(long)]
        check: bool,
    },

    /// Regenerate `docs/src/reference/operation-options.md` from the
    /// single-source wire model. With `--check`, exits non-zero (and
    /// prints a diff) when the on-disk file diverges.
    GenWireDoc {
        /// Verify the on-disk file matches; do not write. Use in CI.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    let result = match Cli::parse().cmd {
        Cmd::GenLuaTypes { check } => gen_lua_types::run(check),
        Cmd::GenTemplateDoc { check } => gen_template_doc::run(check),
        Cmd::GenProto { check } => gen_proto::run(check),
        Cmd::GenWireDoc { check } => gen_wire_doc::run(check),
        Cmd::GenDocTables { check } => gen_doc_tables::run(check),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
