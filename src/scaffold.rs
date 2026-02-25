//! CLI scaffolding commands: init, make collection, make global, make hook, blueprints.
//!
//! Writes plain files to the config directory. No database, no hidden state.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Embedded Lua API type definitions — compiled into the binary.
const LUA_API_TYPES: &str = include_str!("../types/crap.lua");

/// Hook type for the `make hook` command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HookType {
    Collection,
    Field,
    Access,
}

impl HookType {
    /// Parse from string (CLI input).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "collection" => Some(Self::Collection),
            "field" => Some(Self::Field),
            "access" => Some(Self::Access),
            _ => None,
        }
    }

    /// Valid lifecycle positions for this hook type.
    pub fn valid_positions(&self) -> &'static [&'static str] {
        match self {
            Self::Collection => &[
                "before_validate", "before_change", "after_change",
                "before_read", "after_read",
                "before_delete", "after_delete", "before_broadcast",
            ],
            Self::Field => &[
                "before_validate", "before_change", "after_change", "after_read",
            ],
            Self::Access => &["read", "create", "update", "delete"],
        }
    }

    /// Human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Collection => "collection",
            Self::Field => "field",
            Self::Access => "access",
        }
    }
}

/// Options for `make_hook()`. Fully resolved — no prompts.
pub struct MakeHookOptions<'a> {
    pub config_dir: &'a Path,
    pub name: &'a str,
    pub hook_type: HookType,
    pub collection: &'a str,
    pub position: &'a str,
    pub field: Option<&'a str>,
    pub force: bool,
}

/// Scaffold a new config directory with minimum viable structure.
///
/// Creates: crap.toml, init.lua, .luarc.json, .gitignore, and empty directories
/// for collections, globals, hooks, templates, and static.
///
/// Refuses to overwrite if the directory already contains a crap.toml.
pub fn init(dir: Option<PathBuf>) -> Result<()> {
    let target = dir.unwrap_or_else(|| PathBuf::from("./crap-cms"));

    // Refuse to overwrite existing config
    let toml_path = target.join("crap.toml");
    if toml_path.exists() {
        anyhow::bail!(
            "Directory '{}' already contains a crap.toml — refusing to overwrite",
            target.display()
        );
    }

    // Create the directory structure
    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create directory '{}'", target.display()))?;

    for subdir in &["collections", "globals", "hooks", "templates", "static", "migrations", "types"] {
        fs::create_dir_all(target.join(subdir))
            .with_context(|| format!("Failed to create {}/", subdir))?;
    }

    // Write embedded Lua API type definitions
    fs::write(target.join("types/crap.lua"), LUA_API_TYPES)
        .context("Failed to write types/crap.lua")?;

    // crap.toml — sensible defaults with commented-out options
    fs::write(
        &toml_path,
        r#"[server]
admin_port = 3000
grpc_port = 50051
host = "0.0.0.0"

[database]
path = "data/crap.db"

[admin]
dev_mode = true

[auth]
# secret = "your-secret-here"   # omit to auto-generate (tokens won't survive restarts)
# token_expiry = 7200           # seconds, default 2 hours

[live]
# enabled = true                # enable SSE + gRPC Subscribe for live mutation events
# channel_capacity = 1024       # broadcast channel buffer size

# [locale]
# default_locale = "en"         # default locale for content
# locales = ["en", "de"]        # supported locales (empty = disabled)
# fallback = true               # fall back to default locale if field is empty
"#,
    )
    .context("Failed to write crap.toml")?;

    // init.lua — empty entry point with comments
    fs::write(
        target.join("init.lua"),
        r#"-- init.lua — runs once at startup.
-- Register global hooks, set up shared state, or log startup info.

crap.log.info("Crap CMS initializing...")

crap.log.info("init.lua loaded successfully")
"#,
    )
    .context("Failed to write init.lua")?;

    // .luarc.json — LuaLS config pointing to types/
    fs::write(
        target.join(".luarc.json"),
        r#"{
  "workspace.library": ["./types"],
  "runtime.version": "Lua 5.4",
  "diagnostics.globals": ["crap"]
}
"#,
    )
    .context("Failed to write .luarc.json")?;

    // .gitignore — data and uploads (runtime artifacts)
    fs::write(
        target.join(".gitignore"),
        "data/\nuploads/\ntypes/\n",
    )
    .context("Failed to write .gitignore")?;

    let abs = target.canonicalize().unwrap_or_else(|_| target.clone());
    println!("Scaffolded config directory: {}", abs.display());

    Ok(())
}

/// Generate a collection Lua file at `<config_dir>/collections/<slug>.lua`.
///
/// Optionally accepts inline field shorthand (e.g., "title:text:required,body:textarea").
pub fn make_collection(
    config_dir: &Path,
    slug: &str,
    fields_shorthand: Option<&str>,
    no_timestamps: bool,
    auth: bool,
    upload: bool,
    versions: bool,
    force: bool,
) -> Result<()> {
    validate_slug(slug)?;

    let collections_dir = config_dir.join("collections");
    fs::create_dir_all(&collections_dir)
        .context("Failed to create collections/ directory")?;

    let file_path = collections_dir.join(format!("{}.lua", slug));
    if file_path.exists() && !force {
        anyhow::bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let singular_slug = singularize(slug);
    let label_singular = to_title_case(&singular_slug);
    let label_plural = pluralize(&label_singular);
    let timestamps = if no_timestamps { "false" } else { "true" };

    let fields = match fields_shorthand {
        Some(s) => parse_fields_shorthand(s)?,
        None if upload => vec![FieldStub {
            name: "alt".to_string(),
            field_type: "text".to_string(),
            required: false,
        }],
        None => vec![FieldStub {
            name: "title".to_string(),
            field_type: "text".to_string(),
            required: true,
        }],
    };

    let title_field = fields.first().map(|f| f.name.as_str()).unwrap_or("title");

    let mut lua = String::new();
    lua.push_str(&format!("crap.collections.define(\"{}\", {{\n", slug));
    lua.push_str("    labels = {\n");
    lua.push_str(&format!("        singular = \"{}\",\n", label_singular));
    lua.push_str(&format!("        plural = \"{}\",\n", label_plural));
    lua.push_str("    },\n");
    lua.push_str(&format!("    timestamps = {},\n", timestamps));
    if auth {
        lua.push_str("    auth = true,\n");
    }
    if upload {
        lua.push_str("    upload = true,\n");
    }
    if versions {
        lua.push_str("    versions = true,\n");
    }
    lua.push_str("    admin = {\n");
    lua.push_str(&format!("        use_as_title = \"{}\",\n",
        if auth { "email" } else { title_field }));
    lua.push_str("    },\n");
    lua.push_str("    fields = {\n");

    for field in &fields {
        lua.push_str("        {\n");
        lua.push_str(&format!("            name = \"{}\",\n", field.name));
        lua.push_str(&format!("            type = \"{}\",\n", field.field_type));
        if field.required {
            lua.push_str("            required = true,\n");
        }
        lua.push_str("        },\n");
    }

    lua.push_str("    },\n");
    lua.push_str("})\n");

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("Created {}", file_path.display());

    Ok(())
}

/// Generate a global Lua file at `<config_dir>/globals/<slug>.lua`.
pub fn make_global(config_dir: &Path, slug: &str, force: bool) -> Result<()> {
    validate_slug(slug)?;

    let globals_dir = config_dir.join("globals");
    fs::create_dir_all(&globals_dir)
        .context("Failed to create globals/ directory")?;

    let file_path = globals_dir.join(format!("{}.lua", slug));
    if file_path.exists() && !force {
        anyhow::bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let label = to_title_case(slug);

    let lua = format!(
        r#"crap.globals.define("{slug}", {{
    labels = {{
        singular = "{label}",
    }},
    fields = {{
        {{
            name = "title",
            type = "text",
            required = true,
        }},
    }},
}})
"#,
        slug = slug,
        label = label,
    );

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("Created {}", file_path.display());

    Ok(())
}

/// Generate a hook file at `<config_dir>/hooks/<collection>/<name>.lua`.
///
/// Creates a single-function file that returns the function directly (no module table).
/// The template varies by hook type (collection, field, or access).
pub fn make_hook(opts: &MakeHookOptions) -> Result<()> {
    // Validate inputs
    validate_slug(opts.collection)?;
    if opts.name.is_empty() || !opts.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        anyhow::bail!(
            "Invalid hook name '{}' — use alphanumeric characters and underscores only",
            opts.name
        );
    }
    if !opts.hook_type.valid_positions().contains(&opts.position) {
        anyhow::bail!(
            "Invalid position '{}' for {} hook — valid: {}",
            opts.position,
            opts.hook_type.label(),
            opts.hook_type.valid_positions().join(", ")
        );
    }
    if opts.hook_type == HookType::Field && opts.field.is_none() {
        anyhow::bail!("Field hooks require --field to be specified");
    }

    let hooks_dir = opts.config_dir.join("hooks").join(opts.collection);
    fs::create_dir_all(&hooks_dir)
        .context("Failed to create hooks/ subdirectory")?;

    let file_path = hooks_dir.join(format!("{}.lua", opts.name));
    if file_path.exists() && !opts.force {
        anyhow::bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = match opts.hook_type {
        HookType::Collection => format!(
            r#"--- {position} hook for {collection}.
---@param context crap.HookContext
---@return crap.HookContext
return function(context)
    -- TODO: implement
    return context
end
"#,
            position = opts.position,
            collection = opts.collection,
        ),
        HookType::Field => format!(
            r#"--- {position} field hook for {collection}.{field}.
---@param value any
---@param context crap.FieldHookContext
---@return any
return function(value, context)
    -- TODO: implement
    return value
end
"#,
            position = opts.position,
            collection = opts.collection,
            field = opts.field.unwrap_or("?"),
        ),
        HookType::Access => format!(
            r#"--- {position} access control for {collection}.
---@param context crap.AccessContext
---@return boolean | table
return function(context)
    -- TODO: implement
    return true
end
"#,
            position = opts.position,
            collection = opts.collection,
        ),
    };

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    let hook_ref = format!("hooks.{}.{}", opts.collection, opts.name);

    println!("Created {}", file_path.display());
    println!();
    println!("Hook ref: {}", hook_ref);
    println!();

    match opts.hook_type {
        HookType::Collection => {
            println!("Add to your collection definition:");
            println!("  hooks = {{");
            println!("      {} = {{ \"{}\" }},", opts.position, hook_ref);
            println!("  }},");
        }
        HookType::Field => {
            println!("Add to your field definition:");
            println!("  hooks = {{");
            println!("      {} = {{ \"{}\" }},", opts.position, hook_ref);
            println!("  }},");
        }
        HookType::Access => {
            println!("Add to your collection definition:");
            println!("  access = {{");
            println!("      {} = \"{}\",", opts.position, hook_ref);
            println!("  }},");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Proto export
// ---------------------------------------------------------------------------

/// The embedded proto file content — compiled into the binary.
const PROTO_CONTENT: &str = include_str!("../proto/content.proto");

/// Export the embedded `content.proto` file for gRPC client codegen.
///
/// - No `output` → writes to stdout (pipe-friendly).
/// - `output` is a directory → writes `content.proto` into it.
/// - `output` is a file path → writes directly to that file.
pub fn proto_export(output: Option<&Path>) -> Result<()> {
    match output {
        None => {
            // Write to stdout
            std::io::stdout().write_all(PROTO_CONTENT.as_bytes())
                .context("Failed to write proto to stdout")?;
        }
        Some(path) => {
            let target = if path.is_dir() || path.to_string_lossy().ends_with('/') {
                fs::create_dir_all(path)
                    .with_context(|| format!("Failed to create directory '{}'", path.display()))?;
                path.join("content.proto")
            } else {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create directory '{}'", parent.display()))?;
                }
                path.to_path_buf()
            };
            fs::write(&target, PROTO_CONTENT)
                .with_context(|| format!("Failed to write {}", target.display()))?;
            println!("Wrote {}", target.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Templates & static file extraction
// ---------------------------------------------------------------------------

use include_dir::{include_dir, Dir};

/// Embedded default templates — compiled into the binary.
static EMBEDDED_TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");
/// Embedded default static files — compiled into the binary.
static EMBEDDED_STATIC: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");

/// Recursively collect all files from an `include_dir::Dir`, returning `(relative_path, content)`.
/// Paths are relative to the root Dir.
fn collect_embedded_files_flat<'a>(dir: &'a Dir<'a>) -> Vec<(String, &'a [u8])> {
    let mut out = Vec::new();
    for file in dir.files() {
        out.push((file.path().to_string_lossy().to_string(), file.contents()));
    }
    for sub in dir.dirs() {
        out.extend(collect_embedded_files_flat(sub));
    }
    out
}

/// Format a file size as human-readable (e.g., "1.2 KB", "92.0 KB").
fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// List embedded templates and/or static files.
///
/// `type_filter`: None = both, Some("templates") = templates only, Some("static") = static only.
pub fn templates_list(type_filter: Option<&str>) -> Result<()> {
    // Validate filter
    if let Some(f) = type_filter {
        if f != "templates" && f != "static" {
            anyhow::bail!("Invalid --type '{}' — valid: templates, static", f);
        }
    }

    let show_templates = type_filter.is_none() || type_filter == Some("templates");
    let show_static = type_filter.is_none() || type_filter == Some("static");

    if show_templates {
        let files = collect_embedded_files_flat(&EMBEDDED_TEMPLATES);
        println!("Templates ({} files):", files.len());
        print_file_tree(&files);
        if show_static {
            println!();
        }
    }

    if show_static {
        let files = collect_embedded_files_flat(&EMBEDDED_STATIC);
        println!("Static files ({} files):", files.len());
        print_file_tree(&files);
    }

    Ok(())
}

/// Print files grouped by directory in a tree-like format.
fn print_file_tree(files: &[(String, &[u8])]) {
    use std::collections::BTreeMap;

    // Group files by directory
    let mut dirs: BTreeMap<String, Vec<(&str, usize)>> = BTreeMap::new();
    for (path, content) in files {
        let (dir, name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path.as_str()),
        };
        dirs.entry(dir.to_string()).or_default().push((name, content.len()));
    }

    for (dir, entries) in &dirs {
        if !dir.is_empty() {
            println!("  {}/", dir);
        }
        for (name, size) in entries {
            let indent = if dir.is_empty() { "  " } else { "    " };
            println!("{}{:<40} {}", indent, name, format_size(*size));
        }
    }
}

/// Extract embedded templates/static files into a config directory.
///
/// `paths`: specific files to extract (e.g., "layout/base.hbs", "styles.css").
/// `all`: extract all files.
/// `type_filter`: None = both, Some("templates") = templates only, Some("static") = static only.
/// `force`: overwrite existing files.
pub fn templates_extract(
    config_dir: &Path,
    paths: &[String],
    all: bool,
    type_filter: Option<&str>,
    force: bool,
) -> Result<()> {
    // Validate filter
    if let Some(f) = type_filter {
        if f != "templates" && f != "static" {
            anyhow::bail!("Invalid --type '{}' — valid: templates, static", f);
        }
    }

    if !all && paths.is_empty() {
        anyhow::bail!("Specify file paths to extract, or use --all to extract everything");
    }

    let want_templates = type_filter.is_none() || type_filter == Some("templates");
    let want_static = type_filter.is_none() || type_filter == Some("static");

    if all {
        let mut count = 0usize;

        if want_templates {
            let files = collect_embedded_files_flat(&EMBEDDED_TEMPLATES);
            for (path, content) in &files {
                let dest = config_dir.join("templates").join(path);
                if dest.exists() && !force {
                    println!("  Skipped: templates/{} (exists, use --force)", path);
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dest, content)?;
                count += 1;
            }
            if want_templates && !want_static {
                println!("Extracted {} template file(s) to {}/templates/", count, config_dir.display());
                return Ok(());
            }
        }

        if want_static {
            let tpl_count = count;
            let files = collect_embedded_files_flat(&EMBEDDED_STATIC);
            for (path, content) in &files {
                let dest = config_dir.join("static").join(path);
                if dest.exists() && !force {
                    println!("  Skipped: static/{} (exists, use --force)", path);
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dest, content)?;
                count += 1;
            }
            if !want_templates {
                println!("Extracted {} static file(s) to {}/static/", count, config_dir.display());
                return Ok(());
            }
            println!("Extracted {} file(s) ({} templates, {} static) to {}/",
                count, tpl_count, count - tpl_count, config_dir.display());
        }

        return Ok(());
    }

    // Extract specific paths
    let mut extracted = 0usize;
    for path in paths {
        // Try templates first, then static
        let found = if want_templates {
            EMBEDDED_TEMPLATES.get_file(path).map(|f| ("templates", f))
        } else {
            None
        };
        let found = found.or_else(|| {
            if want_static {
                EMBEDDED_STATIC.get_file(path).map(|f| ("static", f))
            } else {
                None
            }
        });

        match found {
            Some((kind, file)) => {
                let dest = config_dir.join(kind).join(path);
                if dest.exists() && !force {
                    println!("  Skipped: {}/{} (exists, use --force)", kind, path);
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dest, file.contents())?;
                println!("  \u{2713} {}/{}", kind, path);
                extracted += 1;
            }
            None => {
                println!("  Not found: {}", path);
            }
        }
    }

    if extracted > 0 {
        println!("Extracted {} file(s) to {}/", extracted, config_dir.display());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration scaffolding
// ---------------------------------------------------------------------------

/// Create a new migration file at `<config_dir>/migrations/YYYYMMDDHHMMSS_name.lua`.
pub fn make_migration(config_dir: &Path, name: &str) -> Result<()> {
    let migrations_dir = config_dir.join("migrations");
    fs::create_dir_all(&migrations_dir)
        .context("Failed to create migrations/ directory")?;

    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    let filename = format!("{}_{}.lua", timestamp, name);
    let file_path = migrations_dir.join(&filename);

    let lua = format!(
        r#"local M = {{}}

function M.up()
    -- TODO: implement migration
    -- crap.* API available (find, create, update, delete)
end

function M.down()
    -- TODO: implement rollback (best-effort)
end

return M
"#,
    );

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("Created {}", file_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Blueprints
// ---------------------------------------------------------------------------

/// Resolve the global blueprints directory.
///
/// - Linux: `~/.config/crap-cms/blueprints/`
/// - macOS: `~/Library/Application Support/crap-cms/blueprints/`
/// - Windows: `C:\Users\<user>\AppData\Roaming\crap-cms\blueprints\`
fn blueprints_dir() -> Result<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory for your platform"))?;
    Ok(base.join("crap-cms").join("blueprints"))
}

/// Files and directories to skip when saving a blueprint (runtime artifacts).
const BLUEPRINT_SKIP: &[&str] = &["data", "uploads", "types"];

/// Save a config directory as a named blueprint.
///
/// Copies everything except runtime artifacts (`data/`, `uploads/`, `types/`)
/// to `~/.config/crap-cms/blueprints/<name>/`.
pub fn blueprint_save(config_dir: &Path, name: &str, force: bool) -> Result<()> {
    validate_blueprint_name(name)?;

    // Verify it's actually a config directory
    if !config_dir.join("crap.toml").exists() {
        anyhow::bail!(
            "Directory '{}' does not contain a crap.toml — not a valid config directory",
            config_dir.display()
        );
    }

    let bp_dir = blueprints_dir()?;
    let target = bp_dir.join(name);

    if target.exists() && !force {
        anyhow::bail!(
            "Blueprint '{}' already exists — use --force to overwrite",
            name
        );
    }

    // Clean target if overwriting
    if target.exists() {
        fs::remove_dir_all(&target)
            .with_context(|| format!("Failed to remove existing blueprint '{}'", name))?;
    }

    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create blueprint directory '{}'", target.display()))?;

    copy_dir_recursive(config_dir, &target, BLUEPRINT_SKIP)
        .with_context(|| format!("Failed to copy config to blueprint '{}'", name))?;

    println!("Saved blueprint '{}' from {}", name, config_dir.display());
    println!("  Location: {}", target.display());

    Ok(())
}

/// Create a new project from a saved blueprint.
///
/// Copies the blueprint to `dir` (or `./crap-cms/` if omitted).
pub fn blueprint_use(name: &str, dir: Option<PathBuf>) -> Result<()> {
    validate_blueprint_name(name)?;

    let bp_dir = blueprints_dir()?;
    let source = bp_dir.join(name);

    if !source.exists() {
        let available = list_blueprint_names()?;
        if available.is_empty() {
            anyhow::bail!("Blueprint '{}' not found. No blueprints saved yet.\nSave one with: crap-cms blueprint save <dir> <name>", name);
        } else {
            anyhow::bail!(
                "Blueprint '{}' not found. Available blueprints: {}",
                name,
                available.join(", ")
            );
        }
    }

    let target = dir.unwrap_or_else(|| PathBuf::from("./crap-cms"));

    // Refuse to overwrite existing config
    if target.join("crap.toml").exists() {
        anyhow::bail!(
            "Directory '{}' already contains a crap.toml — refusing to overwrite",
            target.display()
        );
    }

    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create directory '{}'", target.display()))?;

    copy_dir_recursive(&source, &target, &[])
        .with_context(|| format!("Failed to copy blueprint '{}' to '{}'", name, target.display()))?;

    let abs = target.canonicalize().unwrap_or_else(|_| target.clone());
    println!("Created project from blueprint '{}': {}", name, abs.display());
    println!();
    println!("Start the server: crap-cms serve {}", target.display());

    Ok(())
}

/// List all saved blueprints.
pub fn blueprint_list() -> Result<()> {
    let bp_dir = blueprints_dir()?;

    if !bp_dir.exists() {
        println!("No blueprints saved yet.");
        println!("Save one with: crap-cms blueprint save <dir> <name>");
        return Ok(());
    }

    let names = list_blueprint_names()?;
    if names.is_empty() {
        println!("No blueprints saved yet.");
        println!("Save one with: crap-cms blueprint save <dir> <name>");
        return Ok(());
    }

    println!("Saved blueprints:");
    for name in &names {
        let bp_path = bp_dir.join(name);
        // Count collections and globals for a quick summary
        let collections = count_lua_files(&bp_path.join("collections"));
        let globals = count_lua_files(&bp_path.join("globals"));
        println!("  {} ({} collection(s), {} global(s))", name, collections, globals);
    }
    println!();
    println!("Use with: crap-cms blueprint use <name> [dir]");

    Ok(())
}

/// Remove a saved blueprint.
pub fn blueprint_remove(name: &str) -> Result<()> {
    validate_blueprint_name(name)?;

    let bp_dir = blueprints_dir()?;
    let target = bp_dir.join(name);

    if !target.exists() {
        anyhow::bail!("Blueprint '{}' not found", name);
    }

    fs::remove_dir_all(&target)
        .with_context(|| format!("Failed to remove blueprint '{}'", name))?;

    println!("Removed blueprint '{}'", name);

    Ok(())
}

/// Recursively copy a directory, skipping entries whose names match `skip`.
fn copy_dir_recursive(src: &Path, dst: &Path, skip: &[&str]) -> Result<()> {
    for entry in fs::read_dir(src)
        .with_context(|| format!("Failed to read directory '{}'", src.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if skip.iter().any(|s| *s == name_str.as_ref()) {
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&name);

        if src_path.is_dir() {
            fs::create_dir_all(&dst_path)?;
            copy_dir_recursive(&src_path, &dst_path, &[])?; // skip only applies at top level
        } else {
            fs::copy(&src_path, &dst_path)
                .with_context(|| format!("Failed to copy '{}'", src_path.display()))?;
        }
    }
    Ok(())
}

/// List blueprint names from the global blueprints directory.
pub fn list_blueprint_names() -> Result<Vec<String>> {
    let bp_dir = blueprints_dir()?;
    if !bp_dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(&bp_dir)? {
        let entry = entry?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Count `.lua` files in a directory (0 if directory doesn't exist).
fn count_lua_files(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path().extension().map(|ext| ext == "lua").unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Validate a blueprint name: alphanumeric, hyphens, underscores.
fn validate_blueprint_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("Blueprint name cannot be empty");
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!(
            "Invalid blueprint name '{}' — use alphanumeric characters, hyphens, and underscores only",
            name
        );
    }
    Ok(())
}

/// Validate a slug: lowercase alphanumeric + underscores, not empty.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        anyhow::bail!("Slug cannot be empty");
    }
    if !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        anyhow::bail!(
            "Invalid slug '{}' — use lowercase letters, digits, and underscores only",
            slug
        );
    }
    if slug.starts_with('_') {
        anyhow::bail!("Slug cannot start with underscore");
    }
    Ok(())
}

/// Convert "snake_case" to "Title Case".
fn to_title_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Naive English singularization: strip trailing "s", "es", or "ies" → "y".
fn singularize(s: &str) -> String {
    let lower = s.to_lowercase();
    if lower.ends_with("ies") && lower.len() > 3 {
        format!("{}y", &s[..s.len() - 3])
    } else if lower.ends_with("ses") || lower.ends_with("xes") || lower.ends_with("zes")
        || lower.ends_with("shes") || lower.ends_with("ches")
    {
        s[..s.len() - 2].to_string()
    } else if lower.ends_with('s') && !lower.ends_with("ss") && lower.len() > 1 {
        s[..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Naive English pluralization: add "s" (or "es" for sibilants, "ies" for consonant+y).
fn pluralize(s: &str) -> String {
    if s.is_empty() {
        return s.to_string();
    }
    let lower = s.to_lowercase();
    if lower.ends_with("s") || lower.ends_with("x") || lower.ends_with("z")
        || lower.ends_with("sh") || lower.ends_with("ch")
    {
        format!("{}es", s)
    } else if lower.ends_with("y")
        && !lower.ends_with("ay")
        && !lower.ends_with("ey")
        && !lower.ends_with("oy")
        && !lower.ends_with("uy")
    {
        format!("{}ies", &s[..s.len() - 1])
    } else {
        format!("{}s", s)
    }
}

/// Valid field types for collection definitions.
pub const VALID_FIELD_TYPES: &[&str] = &[
    "text", "number", "textarea", "select", "checkbox", "date",
    "email", "json", "richtext", "relationship", "array", "group",
    "upload", "blocks",
];

struct FieldStub {
    name: String,
    field_type: String,
    required: bool,
}

/// Parse inline field shorthand: "title:text:required,status:select,body:textarea"
fn parse_fields_shorthand(s: &str) -> Result<Vec<FieldStub>> {

    let mut fields = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let segments: Vec<&str> = part.split(':').collect();
        if segments.len() < 2 {
            anyhow::bail!(
                "Invalid field shorthand '{}' — expected 'name:type[:required]'",
                part
            );
        }
        let name = segments[0].to_string();
        let field_type = segments[1].to_lowercase();
        if !VALID_FIELD_TYPES.contains(&field_type.as_str()) {
            anyhow::bail!(
                "Unknown field type '{}' — valid types: {}",
                field_type,
                VALID_FIELD_TYPES.join(", ")
            );
        }
        let required = segments.get(2).map(|s| *s == "required").unwrap_or(false);
        fields.push(FieldStub { name, field_type, required });
    }

    if fields.is_empty() {
        anyhow::bail!("No fields parsed from shorthand");
    }

    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_title_case() {
        assert_eq!(to_title_case("posts"), "Posts");
        assert_eq!(to_title_case("site_settings"), "Site Settings");
        assert_eq!(to_title_case("my_cool_thing"), "My Cool Thing");
    }

    #[test]
    fn test_pluralize() {
        assert_eq!(pluralize("Post"), "Posts");
        assert_eq!(pluralize("Category"), "Categories");
        assert_eq!(pluralize("Tag"), "Tags");
        assert_eq!(pluralize("Address"), "Addresses");
        assert_eq!(pluralize("Box"), "Boxes");
        assert_eq!(pluralize("Key"), "Keys");
    }

    #[test]
    fn test_validate_slug() {
        assert!(validate_slug("posts").is_ok());
        assert!(validate_slug("site_settings").is_ok());
        assert!(validate_slug("v2_users").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("Posts").is_err());
        assert!(validate_slug("my-slug").is_err());
        assert!(validate_slug("_private").is_err());
    }

    #[test]
    fn test_parse_fields_shorthand() {
        let fields = parse_fields_shorthand("title:text:required,body:textarea,published:checkbox").unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "title");
        assert_eq!(fields[0].field_type, "text");
        assert!(fields[0].required);
        assert_eq!(fields[1].name, "body");
        assert_eq!(fields[1].field_type, "textarea");
        assert!(!fields[1].required);
        assert_eq!(fields[2].name, "published");
        assert_eq!(fields[2].field_type, "checkbox");
    }

    #[test]
    fn test_parse_fields_shorthand_invalid() {
        assert!(parse_fields_shorthand("title").is_err());
        assert!(parse_fields_shorthand("title:unknown").is_err());
        assert!(parse_fields_shorthand("").is_err());
    }

    #[test]
    fn test_init_creates_structure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("my-project");
        init(Some(target.clone())).unwrap();

        assert!(target.join("crap.toml").exists());
        assert!(target.join("init.lua").exists());
        assert!(target.join(".luarc.json").exists());
        assert!(target.join(".gitignore").exists());
        assert!(target.join("collections").is_dir());
        assert!(target.join("globals").is_dir());
        assert!(target.join("hooks").is_dir());
        assert!(target.join("templates").is_dir());
        assert!(target.join("static").is_dir());
        assert!(target.join("migrations").is_dir());
        assert!(target.join("types").is_dir());
        assert!(target.join("types/crap.lua").exists());
    }

    #[test]
    fn test_init_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("existing");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("crap.toml"), "# existing").unwrap();

        let result = init(Some(target));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("refusing to overwrite"));
    }

    #[test]
    fn test_make_collection_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/posts.lua")).unwrap();
        assert!(content.contains("crap.collections.define(\"posts\""));
        assert!(content.contains("singular = \"Post\""));
        assert!(content.contains("plural = \"Posts\""));
        assert!(content.contains("timestamps = true"));
        assert!(content.contains("name = \"title\""));
        assert!(content.contains("type = \"text\""));
        assert!(content.contains("required = true"));
    }

    #[test]
    fn test_make_collection_with_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(
            tmp.path(), "articles",
            Some("headline:text:required,body:richtext,draft:checkbox"),
            true, false, false, false, false,
        ).unwrap();

        let content = fs::read_to_string(tmp.path().join("collections/articles.lua")).unwrap();
        assert!(content.contains("timestamps = false"));
        assert!(content.contains("name = \"headline\""));
        assert!(content.contains("name = \"body\""));
        assert!(content.contains("type = \"richtext\""));
        assert!(content.contains("name = \"draft\""));
        assert!(content.contains("use_as_title = \"headline\""));
    }

    #[test]
    fn test_make_collection_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();
        let result = make_collection(tmp.path(), "posts", None, false, false, false, false, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn test_make_collection_force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_collection(tmp.path(), "posts", None, false, false, false, false, false).unwrap();
        assert!(make_collection(tmp.path(), "posts", None, false, false, false, false, true).is_ok());
    }

    #[test]
    fn test_make_global() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_global(tmp.path(), "site_settings", false).unwrap();

        let content = fs::read_to_string(tmp.path().join("globals/site_settings.lua")).unwrap();
        assert!(content.contains("crap.globals.define(\"site_settings\""));
        assert!(content.contains("singular = \"Site Settings\""));
    }

    fn make_hook_opts<'a>(
        config_dir: &'a Path,
        name: &'a str,
        hook_type: HookType,
        collection: &'a str,
        position: &'a str,
        field: Option<&'a str>,
        force: bool,
    ) -> MakeHookOptions<'a> {
        MakeHookOptions { config_dir, name, hook_type, collection, position, field, force }
    }

    #[test]
    fn test_make_hook_collection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "auto_slug", HookType::Collection,
            "posts", "before_change", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/auto_slug.lua")).unwrap();
        assert!(content.contains("before_change hook for posts"));
        assert!(content.contains("crap.HookContext"));
        assert!(content.contains("return function(context)"));
    }

    #[test]
    fn test_make_hook_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "normalize", HookType::Field,
            "posts", "before_validate", Some("title"), false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/normalize.lua")).unwrap();
        assert!(content.contains("before_validate field hook for posts.title"));
        assert!(content.contains("crap.FieldHookContext"));
        assert!(content.contains("return function(value, context)"));
    }

    #[test]
    fn test_make_hook_access() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "admin_only", HookType::Access,
            "posts", "read", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/admin_only.lua")).unwrap();
        assert!(content.contains("read access control for posts"));
        assert!(content.contains("crap.AccessContext"));
        assert!(content.contains("return true"));
    }

    #[test]
    fn test_make_hook_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "auto_slug", HookType::Collection,
            "posts", "before_change", None, false,
        );
        make_hook(&opts).unwrap();
        let result = make_hook(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn test_make_hook_force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "auto_slug", HookType::Collection,
            "posts", "before_change", None, false,
        );
        make_hook(&opts).unwrap();
        let opts_force = make_hook_opts(
            tmp.path(), "auto_slug", HookType::Collection,
            "posts", "before_change", None, true,
        );
        assert!(make_hook(&opts_force).is_ok());
    }

    #[test]
    fn test_make_hook_invalid_position() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "bad", HookType::Collection,
            "posts", "invalid_position", None, false,
        );
        let result = make_hook(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid position"));
    }

    #[test]
    fn test_make_hook_invalid_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "", HookType::Collection,
            "posts", "before_change", None, false,
        );
        assert!(make_hook(&opts).is_err());

        let opts2 = make_hook_opts(
            tmp.path(), "bad-name", HookType::Collection,
            "posts", "before_change", None, false,
        );
        assert!(make_hook(&opts2).is_err());
    }

    #[test]
    fn test_make_hook_field_requires_field_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "hook", HookType::Field,
            "posts", "before_validate", None, false,
        );
        let result = make_hook(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--field"));
    }

    #[test]
    fn test_validate_blueprint_name() {
        assert!(validate_blueprint_name("blog").is_ok());
        assert!(validate_blueprint_name("my-blog").is_ok());
        assert!(validate_blueprint_name("blog_v2").is_ok());
        assert!(validate_blueprint_name("").is_err());
        assert!(validate_blueprint_name("bad name").is_err());
        assert!(validate_blueprint_name("bad/name").is_err());
    }

    #[test]
    fn test_copy_dir_recursive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        // Build a small tree
        fs::create_dir_all(src.join("collections")).unwrap();
        fs::create_dir_all(src.join("data")).unwrap();
        fs::write(src.join("crap.toml"), "# config").unwrap();
        fs::write(src.join("collections/posts.lua"), "-- posts").unwrap();
        fs::write(src.join("data/crap.db"), "binary").unwrap();

        fs::create_dir_all(&dst).unwrap();
        copy_dir_recursive(&src, &dst, &["data"]).unwrap();

        assert!(dst.join("crap.toml").exists());
        assert!(dst.join("collections/posts.lua").exists());
        assert!(!dst.join("data").exists(), "data/ should be skipped");
    }

    #[test]
    fn test_blueprint_save_requires_crap_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Empty dir — no crap.toml
        let result = blueprint_save(tmp.path(), "test", false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("crap.toml"));
    }

    #[test]
    fn test_blueprint_use_not_found() {
        let result = blueprint_use("nonexistent_test_bp_12345", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_blueprint_remove_not_found() {
        let result = blueprint_remove("nonexistent_test_bp_12345");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_count_lua_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("collections");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("posts.lua"), "").unwrap();
        fs::write(dir.join("tags.lua"), "").unwrap();
        fs::write(dir.join("readme.md"), "").unwrap();
        assert_eq!(count_lua_files(&dir), 2);
        assert_eq!(count_lua_files(&tmp.path().join("nope")), 0);
    }

    #[test]
    fn test_blueprint_roundtrip() {
        // Save a blueprint and use it to create a new project
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a fake config dir
        let config = tmp.path().join("my-config");
        fs::create_dir_all(config.join("collections")).unwrap();
        fs::create_dir_all(config.join("data")).unwrap();
        fs::create_dir_all(config.join("uploads")).unwrap();
        fs::write(config.join("crap.toml"), "[server]\nadmin_port = 4000\n").unwrap();
        fs::write(config.join("init.lua"), "-- hello").unwrap();
        fs::write(config.join("collections/posts.lua"), "-- posts").unwrap();
        fs::write(config.join("data/crap.db"), "should be skipped").unwrap();
        fs::write(config.join("uploads/photo.jpg"), "should be skipped").unwrap();

        // Save as blueprint (use a custom dir to avoid polluting real config)
        // We test the internal helpers instead of the full save/use flow
        // since those depend on the global config dir
        let bp_dir = tmp.path().join("blueprints");
        fs::create_dir_all(&bp_dir).unwrap();
        let bp_target = bp_dir.join("my-blog");
        fs::create_dir_all(&bp_target).unwrap();

        copy_dir_recursive(&config, &bp_target, BLUEPRINT_SKIP).unwrap();

        // Verify blueprint contents
        assert!(bp_target.join("crap.toml").exists());
        assert!(bp_target.join("init.lua").exists());
        assert!(bp_target.join("collections/posts.lua").exists());
        assert!(!bp_target.join("data").exists(), "data/ should be excluded");
        assert!(!bp_target.join("uploads").exists(), "uploads/ should be excluded");

        // "Use" the blueprint to create a new project
        let new_project = tmp.path().join("new-project");
        fs::create_dir_all(&new_project).unwrap();
        copy_dir_recursive(&bp_target, &new_project, &[]).unwrap();

        assert!(new_project.join("crap.toml").exists());
        assert!(new_project.join("init.lua").exists());
        assert!(new_project.join("collections/posts.lua").exists());

        let toml = fs::read_to_string(new_project.join("crap.toml")).unwrap();
        assert!(toml.contains("admin_port = 4000"));
    }

    #[test]
    fn test_templates_list() {
        // Just verify it runs without error
        assert!(templates_list(None).is_ok());
        assert!(templates_list(Some("templates")).is_ok());
        assert!(templates_list(Some("static")).is_ok());
        assert!(templates_list(Some("invalid")).is_err());
    }

    #[test]
    fn test_templates_list_has_files() {
        // Verify embedded dirs actually contain files
        let tpl_files = collect_embedded_files_flat(&EMBEDDED_TEMPLATES);
        assert!(!tpl_files.is_empty(), "should have embedded templates");
        assert!(tpl_files.iter().any(|(p, _)| p.ends_with(".hbs")));

        let static_files = collect_embedded_files_flat(&EMBEDDED_STATIC);
        assert!(!static_files.is_empty(), "should have embedded static files");
        assert!(static_files.iter().any(|(p, _)| p.ends_with(".css")));
    }

    #[test]
    fn test_templates_extract_specific() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false, None, false,
        ).unwrap();

        assert!(tmp.path().join("templates/layout/base.hbs").exists());
        let content = fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
        assert!(!content.is_empty());
    }

    #[test]
    fn test_templates_extract_static_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(
            tmp.path(),
            &["styles.css".to_string()],
            false, None, false,
        ).unwrap();

        assert!(tmp.path().join("static/styles.css").exists());
    }

    #[test]
    fn test_templates_extract_skips_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Extract once
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false, None, false,
        ).unwrap();

        // Write a marker to verify it doesn't get overwritten
        fs::write(tmp.path().join("templates/layout/base.hbs"), "CUSTOM").unwrap();

        // Extract again without --force
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false, None, false,
        ).unwrap();

        let content = fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
        assert_eq!(content, "CUSTOM", "should not overwrite without --force");
    }

    #[test]
    fn test_templates_extract_force_overwrites() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Extract once
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false, None, false,
        ).unwrap();

        // Write a marker
        fs::write(tmp.path().join("templates/layout/base.hbs"), "CUSTOM").unwrap();

        // Extract again with --force
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false, None, true,
        ).unwrap();

        let content = fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
        assert_ne!(content, "CUSTOM", "should overwrite with --force");
    }

    #[test]
    fn test_templates_extract_all_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(tmp.path(), &[], true, Some("templates"), false).unwrap();

        // Should have created template files
        assert!(tmp.path().join("templates/layout/base.hbs").exists());
        // Should NOT have created static files
        assert!(!tmp.path().join("static/styles.css").exists());
    }

    #[test]
    fn test_templates_extract_all_static() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(tmp.path(), &[], true, Some("static"), false).unwrap();

        // Should have created static files
        assert!(tmp.path().join("static/styles.css").exists());
        // Should NOT have created template files
        assert!(!tmp.path().join("templates/layout/base.hbs").exists());
    }

    #[test]
    fn test_templates_extract_requires_paths_or_all() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = templates_extract(tmp.path(), &[], false, None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--all"));
    }

    #[test]
    fn test_templates_extract_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Should not error, just print "Not found"
        templates_extract(
            tmp.path(),
            &["nonexistent/file.hbs".to_string()],
            false, None, false,
        ).unwrap();
    }
}
