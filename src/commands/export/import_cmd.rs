//! `import` command — load collection data from JSON.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

use crate::{
    cli,
    commands::{
        Project, cli_infra,
        export::{
            file::ExportFile,
            import_checks::{
                check_duplicate_ids, check_import_slugs, check_totp_secrets, import_slugs,
                read_export_file,
            },
            import_row::ImportTarget,
            import_write::{ImportBatch, Imported, import_batches, settle_after_commit},
        },
        open_project,
    },
    config::LocaleConfig,
    core::Registry,
    db::DbConnection,
};

/// Where an import reads from: the export, and this installation's schema and
/// locales.
struct ImportSource<'a> {
    registry: &'a Registry,
    export_file: &'a ExportFile,
    locale: &'a LocaleConfig,
}

/// Report what each collection imported, and the accounts left unable to log in.
fn report_import(batches: &[ImportBatch<'_>], imported: &[Imported<'_>]) {
    for batch in batches {
        let slug = batch.target.slug;
        cli::success(&format!(
            "Imported {} document(s) into '{slug}'",
            batch.docs.len()
        ));

        let without_password: Vec<&Imported<'_>> = imported
            .iter()
            .filter(|doc| doc.target.slug == slug && doc.lacks_password)
            .collect();

        if without_password.is_empty() {
            continue;
        }

        cli::warning(&format!(
            "{} account(s) in '{slug}' have no password and can't log in until one is set.",
            without_password.len()
        ));

        if without_password.iter().any(|doc| !doc.carried_credentials) {
            cli::hint("Export with --include-credentials to carry passwords.");
        }
    }

    cli::success(&format!("Total: {} document(s) imported", imported.len()));
}

/// One collection's documents in the export and its target in the schema.
fn resolve_batch<'a>(
    tx: &dyn DbConnection,
    source: &ImportSource<'a>,
    slug: &'a str,
) -> Result<ImportBatch<'a>> {
    let def = source
        .registry
        .get_collection(slug)
        .ok_or_else(|| anyhow!("Collection '{slug}' exists in import file but not in schema"))?;

    let docs = source
        .export_file
        .collections
        .get(slug)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Expected array for collection '{slug}'"))?;

    Ok(ImportBatch {
        target: ImportTarget::resolve(tx, slug, def, source.locale)?,
        docs,
    })
}

/// Import collection data from JSON, all of it in one transaction: an import
/// that fails leaves nothing behind.
///
/// # Errors
///
/// Returns an error if config loading, file reading, JSON parsing, or any
/// per-document write fails.
#[cfg(not(tarpaulin_include))]
pub fn import(config_dir: &Path, file: &Path, collection_filter: Option<&str>) -> Result<()> {
    let Project {
        lock: _instance_lock,
        config: cfg,
        registry,
        pool,
    } = open_project(config_dir)?;

    let export_file = read_export_file(file)?;
    let slugs = import_slugs(&export_file, collection_filter)?;
    check_import_slugs(&registry, &slugs)?;

    // Built — and a configured Redis reached — before anything is written.
    let infra = cli_infra(config_dir, &registry, &cfg, &pool)?;

    let mut conn = pool.write().context("Failed to get database connection")?;
    // IMMEDIATE takes SQLite's write lock up front, so the import's reads and
    // writes on one transaction can't fail on a busy snapshot under a
    // concurrent writer. On Postgres it is a plain transaction.
    let tx = conn
        .transaction_immediate()
        .context("Failed to begin transaction")?;

    let source = ImportSource {
        registry: &registry,
        export_file: &export_file,
        locale: &cfg.locale,
    };
    let batches = slugs
        .iter()
        .map(|slug| resolve_batch(&tx, &source, slug))
        .collect::<Result<Vec<_>>>()?;

    check_duplicate_ids(&batches)?;
    check_totp_secrets(&batches, cfg.auth.secret.as_ref())?;

    let imported = import_batches(&tx, &batches)?;

    tx.commit().context("Failed to commit the import")?;
    settle_after_commit(&infra, &imported);
    report_import(&batches, &imported);

    Ok(())
}
