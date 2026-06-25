//! `make route` -- generate a custom HTTP route handler at
//! `<config_dir>/routes/<name>.lua` and print the matching
//! `crap.routes.register(...)` snippet for `init.lua`.

use std::fmt::Write as _;
use std::{fs, path::Path};

use anyhow::{Context as _, Result};

use crate::cli;
use crate::scaffold::guards::refuse_file_overwrite;
use crate::scaffold::{paths, validate_slug};

/// Options for `make route`.
pub struct MakeRouteOptions<'a> {
    pub config_dir: &'a Path,
    /// Route file name -> `routes/<name>.lua` and the `routes.<name>` ref.
    pub name: &'a str,
    /// HTTP method (defaults to `GET` when omitted).
    pub method: Option<&'a str>,
    /// URL path (defaults to `/<name>` when omitted).
    pub path: Option<&'a str>,
    pub force: bool,
}

/// Scaffold a custom route handler and print the registration snippet.
///
/// # Errors
/// Returns an error on an invalid name, a directory-create failure, an
/// overwrite refusal, or a write failure.
pub fn make_route(opts: &MakeRouteOptions) -> Result<()> {
    validate_slug(opts.name)?;

    let routes_dir = paths::routes_dir(opts.config_dir);
    fs::create_dir_all(&routes_dir).context("Failed to create routes/ directory")?;

    let file_path = routes_dir.join(format!("{}.lua", opts.name));
    refuse_file_overwrite(&file_path, opts.force)?;

    fs::write(&file_path, render_route_lua())
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    let handler_ref = format!("routes.{}", opts.name);
    cli::success(&format!("Created {}", file_path.display()));
    cli::kv("Handler ref", &handler_ref);
    print_register_hint(opts, &handler_ref);

    Ok(())
}

/// The starter handler body. Wrapped in `crap.any.route_handler` so the editor
/// types `ctx` as `crap.RouteContext` (identity pass-through at runtime).
fn render_route_lua() -> &'static str {
    "-- Custom HTTP route handler. Register it in init.lua (see the printed\n\
     -- snippet). Runs in pool-mode like a job handler: per-op CRUD autocommits,\n\
     -- and `crap.transaction(fn)` wraps a block in one transaction.\n\
     return crap.any.route_handler(function(ctx)\n\
     \treturn {\n\
     \t\tjson = { ok = true, method = ctx.method },\n\
     \t}\n\
     end)\n"
}

/// Print the `crap.routes.register(...)` snippet to paste into `init.lua`.
fn print_register_hint(opts: &MakeRouteOptions, handler_ref: &str) {
    let method = opts.method.unwrap_or("GET").to_ascii_uppercase();
    let path = opts
        .path
        .map_or_else(|| format!("/{}", opts.name), str::to_string);

    let mut snippet = String::new();
    snippet.push_str("\nAdd this to your init.lua to mount the route:\n\n");
    let _ = writeln!(snippet, "  crap.routes.register({{");
    let _ = writeln!(snippet, "    path = \"{path}\",");
    let _ = writeln!(snippet, "    method = \"{method}\",");
    let _ = writeln!(snippet, "    handler = \"{handler_ref}\",");
    let _ = writeln!(snippet, "  }})");
    cli::hint(&snippet);
}
