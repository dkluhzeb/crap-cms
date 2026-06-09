//! Status display functions — formatting and printing project info sections.

use std::{fs, path::Path};

use crate::{
    cli::{self, Table},
    config::CrapConfig,
    core::{Access, HookRef, Hooks, LiveMode, LiveSetting, Registry},
    db::{DbConnection, DbPool, migrate, query},
};

/// Format a byte count as a human-readable string (e.g., "1.5 MB").
///
/// Uses integer arithmetic to compute one decimal place, sidestepping the
/// precision-loss that comes with `bytes as f64` for large `u64` values.
pub(super) fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    fn split(bytes: u64, unit: u64) -> (u64, u64) {
        (bytes / unit, (bytes % unit) * 10 / unit)
    }

    if bytes >= GB {
        let (whole, tenths) = split(bytes, GB);
        format!("{whole}.{tenths} GB")
    } else if bytes >= MB {
        let (whole, tenths) = split(bytes, MB);
        format!("{whole}.{tenths} MB")
    } else if bytes >= KB {
        let (whole, tenths) = split(bytes, KB);
        format!("{whole}.{tenths} KB")
    } else {
        format!("{bytes} bytes")
    }
}

/// Recursively sum file sizes in a directory.
pub(super) fn dir_size(path: &Path) -> u64 {
    if !path.is_dir() {
        return 0;
    }
    let mut total = 0u64;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata();

            if let Ok(m) = meta {
                if m.is_dir() {
                    total += dir_size(&entry.path());
                } else {
                    total += m.len();
                }
            }
        }
    }
    total
}

/// Count files (non-directories) in a directory recursively.
pub(super) fn walkdir_count(path: &Path) -> usize {
    let mut count = 0;

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(m) = entry.metadata() {
                if m.is_dir() {
                    count += walkdir_count(&entry.path());
                } else {
                    count += 1;
                }
            }
        }
    }

    count
}

/// Print database info (path and size for `SQLite`, backend name otherwise).
pub(super) fn print_db_info(cfg: &CrapConfig, config_dir: &Path, conn: &dyn DbConnection) {
    match conn.kind() {
        "sqlite" => {
            let db_path = cfg.db_path(config_dir);
            let db_size = fs::metadata(&db_path).map_or(0, |m| m.len());

            cli::kv(
                "Database",
                &format!("{} ({})", db_path.display(), format_bytes(db_size)),
            );
        }
        other => {
            cli::kv("Database", &format!("{other} backend"));
        }
    }
}

/// Print admin-UI customization summary — overrides shadowing built-in
/// defaults plus user-original additions, with a hint about
/// drift / cleanup state when relevant. Surfaces the data
/// `crap-cms templates status` shows in detail, condensed to one line
/// for the project overview.
///
/// Silently emits nothing when the config dir has no `templates/` and
/// no `static/` directories (fresh install) — the kv line would be
/// noise.
pub(super) fn print_customizations(config_dir: &Path) {
    // I/O issue under config_dir — silently skip
    let Ok(counts) = crate::commands::templates::customization_counts(config_dir) else {
        return;
    };

    if counts.overrides == 0 && counts.additions == 0 {
        return;
    }

    let mut value = format!(
        "{} override(s), {} addition(s)",
        counts.overrides, counts.additions
    );

    let mut notes = Vec::new();
    if counts.actionable > 0 {
        notes.push(format!("{} need attention", counts.actionable));
    }
    if counts.pristine > 0 {
        notes.push(format!(
            "{} pristine (extracted, unedited)",
            counts.pristine
        ));
    }
    if !notes.is_empty() {
        value.push_str(" — ");
        value.push_str(&notes.join(", "));
    }

    cli::kv("Customizations", &value);

    if counts.actionable > 0 || counts.pristine > 0 {
        cli::hint("Run `crap-cms templates status` for per-file detail.");
    }
}

/// Print upload directory stats (total size and file count).
pub(super) fn print_uploads_info(config_dir: &Path) {
    let uploads_dir = config_dir.join("uploads");

    if uploads_dir.is_dir() {
        let uploads_size = dir_size(&uploads_dir);
        let file_count = walkdir_count(&uploads_dir);

        cli::kv(
            "Uploads",
            &format!("{} ({} file(s))", format_bytes(uploads_size), file_count),
        );
    }
}

/// Print locale configuration summary.
pub(super) fn print_locale_info(cfg: &CrapConfig) {
    if cfg.locale.is_enabled() {
        cli::kv(
            "Locales",
            &format!(
                "{} (default: {}{})",
                cfg.locale.locales.join(", "),
                cfg.locale.default_locale,
                if cfg.locale.fallback {
                    ", fallback enabled"
                } else {
                    ""
                }
            ),
        );
    }
}

/// Print collections table with row counts, trash counts, and tags.
pub(super) fn print_collections(reg: &Registry, conn: &dyn DbConnection) {
    if reg.collections.is_empty() {
        cli::dim("Collections: (none)");
        return;
    }

    let mut table = Table::new(vec!["Collection", "Rows", "Trash", "Tags"]);
    let mut slugs: Vec<_> = reg.collections.keys().collect();

    slugs.sort();

    for slug in slugs {
        let def = &reg.collections[slug];
        let count = query::count(conn, slug, def, &[], None).unwrap_or(0);

        let trash_str = if def.soft_delete {
            let trash_count = trash_count(conn, slug);
            trash_count.to_string()
        } else {
            "-".to_string()
        };

        let mut tags = Vec::new();

        if def.is_auth_collection() {
            tags.push("auth");
        }

        if def.is_upload_collection() {
            tags.push("upload");
        }

        if def.has_versions() {
            tags.push("versions");
        }

        if def.soft_delete {
            tags.push("soft_delete");
        }

        let tag_str = tags.join(", ");

        table.row(vec![slug, &count.to_string(), &trash_str, &tag_str]);
    }

    table.print();
}

/// Count soft-deleted documents in a collection.
fn trash_count(conn: &dyn DbConnection, slug: &str) -> i64 {
    conn.query_one(
        &format!("SELECT COUNT(*) AS cnt FROM \"{slug}\" WHERE _deleted_at IS NOT NULL"),
        &[],
    )
    .ok()
    .flatten()
    .and_then(|r| r.get_i64("cnt").ok())
    .unwrap_or(0)
}

/// Print globals table.
pub(super) fn print_globals(reg: &Registry) {
    if reg.globals.is_empty() {
        cli::dim("Globals: (none)");
        return;
    }

    let mut table = Table::new(vec!["Global"]);
    let mut slugs: Vec<_> = reg.globals.keys().collect();

    slugs.sort();

    for slug in slugs {
        table.row(vec![slug]);
    }

    table.print();
}

/// Print server configuration summary.
pub(super) fn print_server_info(cfg: &CrapConfig) {
    cli::kv(
        "Admin",
        &format!("{}:{}", cfg.server.host, cfg.server.admin_port),
    );
    cli::kv(
        "gRPC",
        &format!("{}:{}", cfg.server.host, cfg.server.grpc_port),
    );
    cli::kv(
        "Compression",
        &format!("{:?}", cfg.server.compression).to_lowercase(),
    );

    let rate = if cfg.server.grpc_rate_limit_requests == 0 {
        "disabled".to_string()
    } else {
        format!(
            "{} req/{}s",
            cfg.server.grpc_rate_limit_requests, cfg.server.grpc_rate_limit_window
        )
    };
    cli::kv("Rate limit", &rate);
}

/// Print access rules overview for collections and globals.
pub(super) fn print_access(cfg: &CrapConfig, reg: &Registry) {
    let mut table = Table::new(vec!["Target", "Read", "Create", "Update", "Delete"]);
    let mut has_rows = false;

    let mut slugs: Vec<_> = reg.collections.keys().collect();
    slugs.sort();

    for slug in slugs {
        let a = &reg.collections[slug].access;
        let row = access_row(slug, a);

        if row.iter().skip(1).any(|v| *v != "-") {
            table.row(row.iter().map(std::string::String::as_str).collect());
            has_rows = true;
        }
    }

    let mut global_slugs: Vec<_> = reg.globals.keys().collect();
    global_slugs.sort();

    for slug in global_slugs {
        let a = &reg.globals[slug].access;
        let label = format!("{slug} (global)");
        let row = access_row(&label, a);

        if row.iter().skip(1).any(|v| *v != "-") {
            table.row(row.iter().map(std::string::String::as_str).collect());
            has_rows = true;
        }
    }

    if !has_rows {
        let deny_str = if cfg.access.default_deny {
            "default_deny = true (all denied)"
        } else {
            "default_deny = false (all open)"
        };
        cli::dim(&format!("Access rules: (none) — {deny_str}"));
        return;
    }

    table.print();

    let default = if cfg.access.default_deny {
        "deny"
    } else {
        "allow"
    };
    cli::dim(&format!("  Unset rules default to: {default}"));
}

fn access_row(target: &str, a: &Access) -> Vec<String> {
    let fmt = |opt: &Option<HookRef>| opt.as_ref().map_or("-", HookRef::reference).to_string();

    vec![
        target.to_string(),
        fmt(&a.read),
        fmt(&a.create),
        fmt(&a.update),
        fmt(&a.delete),
    ]
}

/// Print live event configuration per collection.
pub(super) fn print_live(cfg: &CrapConfig, reg: &Registry) {
    if !cfg.live.enabled {
        cli::dim("Live events: disabled");
        return;
    }

    let mut rows = Vec::new();

    let mut slugs: Vec<_> = reg.collections.keys().collect();
    slugs.sort();

    for slug in &slugs {
        let def = &reg.collections[*slug];
        let status = live_status(def.live.as_ref(), def.live_mode);
        rows.push((slug.to_string(), status));
    }

    let mut global_slugs: Vec<_> = reg.globals.keys().collect();
    global_slugs.sort();

    for slug in &global_slugs {
        let def = &reg.globals[*slug];
        let status = live_status(def.live.as_ref(), def.live_mode);
        rows.push((format!("{slug} (global)"), status));
    }

    // Only show if any collection has non-default config
    let non_default: Vec<_> = rows.iter().filter(|(_, s)| s != "metadata").collect();

    if non_default.is_empty() {
        cli::kv(
            "Live events",
            &format!("enabled — all {} target(s) use metadata mode", rows.len()),
        );
        return;
    }

    let mut table = Table::new(vec!["Target", "Mode"]);

    for (target, status) in &rows {
        table.row(vec![target.as_str(), status.as_str()]);
    }

    table.print();
}

fn live_status(live: Option<&LiveSetting>, mode: LiveMode) -> String {
    match live {
        Some(LiveSetting::Disabled) => "disabled".to_string(),
        Some(LiveSetting::Function(f)) => format!("filter: {}", f.reference()),
        None => match mode {
            LiveMode::Full => "full".to_string(),
            LiveMode::Metadata => "metadata".to_string(),
        },
    }
}

/// Print versioning configuration per collection.
pub(super) fn print_versions(reg: &Registry) {
    let versioned: Vec<_> = {
        let mut slugs: Vec<_> = reg.collections.keys().collect();
        slugs.sort();
        slugs
            .into_iter()
            .filter_map(|slug| {
                let def = &reg.collections[slug];
                def.versions.as_ref().map(|v| (slug, v))
            })
            .collect()
    };

    if versioned.is_empty() {
        cli::dim("Versioning: (none)");
        return;
    }

    let mut table = Table::new(vec!["Collection", "Drafts", "Max versions"]);

    for (slug, v) in &versioned {
        let drafts = if v.drafts { "yes" } else { "no" };
        let max = if v.max_versions == 0 {
            "unlimited".to_string()
        } else {
            v.max_versions.to_string()
        };
        table.row(vec![slug, drafts, &max]);
    }

    table.print();
}

/// Print migration status (total, applied, pending).
pub(super) fn print_migrations(config_dir: &Path, pool: &DbPool) {
    let migrations_dir = config_dir.join("migrations");
    let all_files = migrate::list_migration_files(&migrations_dir).unwrap_or_default();
    let applied = migrate::get_applied_migrations(pool).unwrap_or_default();
    let pending = all_files.iter().filter(|f| !applied.contains(*f)).count();

    cli::kv(
        "Migrations",
        &format!(
            "{} total, {} applied, {} pending",
            all_files.len(),
            applied.len(),
            pending
        ),
    );
}

/// Print hooks assigned to collections and globals.
pub(super) fn print_hooks(reg: &Registry) {
    let mut rows = Vec::new();

    let mut slugs: Vec<_> = reg.collections.keys().collect();
    slugs.sort();

    for slug in slugs {
        let h = &reg.collections[slug].hooks;
        let assigned = collect_hook_names(h);

        if !assigned.is_empty() {
            rows.push((slug.to_string(), assigned));
        }
    }

    let mut global_slugs: Vec<_> = reg.globals.keys().collect();
    global_slugs.sort();

    for slug in global_slugs {
        let h = &reg.globals[slug].hooks;
        let assigned = collect_hook_names(h);

        if !assigned.is_empty() {
            rows.push((format!("{slug} (global)"), assigned));
        }
    }

    if rows.is_empty() {
        cli::dim("Hooks: (none)");
        return;
    }

    let mut table = Table::new(vec!["Target", "Hook", "Functions"]);

    for (target, hooks) in &rows {
        for (i, (event, fns)) in hooks.iter().enumerate() {
            let target_col = if i == 0 { target.as_str() } else { "" };
            let joined = fns
                .iter()
                .map(HookRef::reference)
                .collect::<Vec<_>>()
                .join(", ");
            table.row(vec![target_col, event, &joined]);
        }
    }

    table.print();
}

/// Collect non-empty hook events with their function names.
fn collect_hook_names(h: &Hooks) -> Vec<(&'static str, &Vec<HookRef>)> {
    let events: &[(&str, &Vec<HookRef>)] = &[
        ("before_validate", &h.before_validate),
        ("before_change", &h.before_change),
        ("after_change", &h.after_change),
        ("before_read", &h.before_read),
        ("after_read", &h.after_read),
        ("before_delete", &h.before_delete),
        ("after_delete", &h.after_delete),
        ("before_broadcast", &h.before_broadcast),
    ];

    events
        .iter()
        .filter(|(_, fns)| !fns.is_empty())
        .map(|(name, fns)| (*name, *fns))
        .collect()
}

/// Print jobs summary (defined, running, failed in last 24h).
pub(super) fn print_jobs(reg: &Registry, conn: &dyn DbConnection, config_dir: &Path) {
    let jobs_dir = config_dir.join("jobs");

    if !jobs_dir.is_dir() {
        return;
    }

    let defined = reg.jobs.len();
    let running: i64 = conn
        .query_one(
            "SELECT COUNT(*) AS cnt FROM _crap_jobs WHERE status = 'running'",
            &[],
        )
        .ok()
        .flatten()
        .and_then(|r| r.get_i64("cnt").ok())
        .unwrap_or(0);
    let failed_24h = query::jobs::count_failed_since(conn, 86400).unwrap_or(0);

    let mut parts = vec![format!("{} defined", defined)];

    if running > 0 {
        parts.push(format!("{running} running"));
    }

    if failed_24h > 0 {
        parts.push(format!("{failed_24h} failed (24h)"));
    }

    cli::kv("Jobs", &parts.join(", "));
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_values() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1048576), "1.0 MB");
        assert_eq!(format_bytes(1073741824), "1.0 GB");
    }

    #[test]
    fn dir_size_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(dir_size(tmp.path()), 0);
    }

    #[test]
    fn dir_size_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        fs::write(tmp.path().join("b.txt"), "world!").unwrap();
        assert_eq!(dir_size(tmp.path()), 11); // 5 + 6
    }

    #[test]
    fn dir_size_nested() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("a.txt"), "abc").unwrap();
        fs::write(tmp.path().join("sub/b.txt"), "defg").unwrap();
        assert_eq!(dir_size(tmp.path()), 7); // 3 + 4
    }

    #[test]
    fn dir_size_nonexistent() {
        assert_eq!(dir_size(Path::new("/nonexistent/path")), 0);
    }

    #[test]
    fn walkdir_count_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(walkdir_count(tmp.path()), 0);
    }

    #[test]
    fn walkdir_count_with_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("a.txt"), "").unwrap();
        fs::write(tmp.path().join("sub/b.txt"), "").unwrap();
        assert_eq!(walkdir_count(tmp.path()), 2);
    }
}
