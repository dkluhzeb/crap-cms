//! `export` command — dump collection data to JSON.

use std::{
    collections::HashMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::Utc;
use serde_json::{Map, Value, to_string_pretty, to_value};

use crate::{
    cli,
    commands::{
        Project,
        export::file::{EXPORT_FORMAT_VERSION, ExportFile},
        open_project,
    },
    config::LocaleConfig,
    core::{CollectionDefinition, Document, Registry, flatten_group_fields, nest_group_fields},
    db::{DbConnection, LocaleContext, query},
};

/// One collection an export reads.
struct ExportTarget<'a> {
    slug: &'a str,
    def: &'a CollectionDefinition,
    /// Carry each account's credentials as a `_credentials` object.
    include_credentials: bool,
}

/// Export one collection's documents, trashed ones included, as a JSON array.
/// With `include_credentials`, each account of an auth collection carries its
/// credentials as a `_credentials` object.
fn export_collection(
    conn: &dyn DbConnection,
    target: &ExportTarget<'_>,
    locale: &LocaleConfig,
) -> Result<Value> {
    let (slug, def) = (target.slug, target.def);

    // Read in "all locales" mode so localized fields export as
    // `{ "<locale>": value }` objects — lossless across every translation,
    // and required at all: a bare-column read on a localized collection is
    // a SQL error (the columns are `title__en`, not `title`).
    let locale_ctx = LocaleContext::from_locale_string(Some("all"), locale)
        .map_err(|e| anyhow!("locale context: {e}"))?;

    let find_query = query::FindQuery::builder().include_deleted(true).build();
    let mut docs = query::find(conn, slug, def, &find_query, locale_ctx.as_ref())?;

    for doc in &mut docs {
        query::hydrate_document(conn, slug, &def.fields, doc, None, locale_ctx.as_ref())?;
        localize_join_rows(conn, target, doc, locale)?;
    }

    let credentials = if target.include_credentials && def.is_auth_collection() {
        query::read_credentials(conn, slug)?
    } else {
        HashMap::new()
    };

    let docs_json = docs
        .into_iter()
        .map(|doc| {
            let creds = credentials.get(&doc.id.to_string()).cloned();
            let mut value = to_value(doc)?;

            if let (Some(obj), Some(creds)) = (value.as_object_mut(), creds) {
                obj.insert("_credentials".to_string(), Value::Object(creds));
            }

            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Value::Array(docs_json))
}

/// Replace each localized join field's rows — read under the default locale —
/// with every locale's own rows, as `{ "<locale>": rows }` like a localized
/// column.
fn localize_join_rows(
    conn: &dyn DbConnection,
    target: &ExportTarget<'_>,
    doc: &mut Document,
    locale: &LocaleConfig,
) -> Result<()> {
    if !locale.is_enabled() {
        return Ok(());
    }

    let fields = &target.def.fields;
    let rows = query::locale_join_rows(
        conn,
        doc,
        query::JoinOwner::new(target.slug, fields),
        locale,
    )?;
    if rows.is_empty() {
        return Ok(());
    }

    let mut flat = flatten_group_fields(&doc.fields, fields);
    for (key, by_locale) in rows {
        flat.insert(key, Value::Object(by_locale));
    }
    doc.fields = nest_group_fields(&flat, fields);

    Ok(())
}

/// The collections to export: the one `collection_filter` names, or all of
/// them in slug order.
fn export_slugs(registry: &Registry, collection_filter: Option<&str>) -> Result<Vec<String>> {
    if let Some(slug) = collection_filter {
        if registry.get_collection(slug).is_none() {
            bail!("Collection '{slug}' not found");
        }

        return Ok(vec![slug.to_string()]);
    }

    let mut slugs: Vec<String> = registry
        .collections
        .keys()
        .map(ToString::to_string)
        .collect();
    slugs.sort();

    Ok(slugs)
}

/// The sibling `path` is staged under while being written: `<name>.tmp` in
/// the same directory, so the final rename never crosses a filesystem.
fn staged_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".tmp");

    path.with_file_name(name)
}

/// Write `content` to `path` through a staged sibling renamed into place only
/// once it is complete. A kill mid-write leaves the stage behind, never a
/// truncated file under the final name that reads as a valid export.
fn write_atomically(path: &Path, content: &str) -> Result<()> {
    let staged = staged_path(path);

    fs::write(&staged, content).with_context(|| format!("Failed to write {}", staged.display()))?;

    if let Err(e) = fs::rename(&staged, path) {
        let _ = fs::remove_file(&staged);

        return Err(e).with_context(|| format!("Failed to write {}", path.display()));
    }

    Ok(())
}

/// Write the export to `output`, or print it when there is none.
fn write_export(export_file: &ExportFile, output: Option<PathBuf>) -> Result<()> {
    let content = to_string_pretty(export_file)?;

    let Some(path) = output else {
        println!("{content}");
        return Ok(());
    };

    write_atomically(&path, &content)?;

    cli::success(&format!(
        "Exported {} collection(s) to {}",
        export_file.collections.len(),
        path.display()
    ));

    Ok(())
}

/// Export collection data to JSON.
///
/// # Errors
///
/// Returns an error if config loading, pool creation, the collection scan,
/// or writing the output file fails.
#[cfg(not(tarpaulin_include))]
pub fn export(
    config_dir: &Path,
    collection_filter: Option<&str>,
    output: Option<PathBuf>,
    include_credentials: bool,
) -> Result<()> {
    let Project {
        lock: _instance_lock,
        config: cfg,
        registry,
        pool,
    } = open_project(config_dir)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let mut collections = Map::new();

    for slug in export_slugs(&registry, collection_filter)? {
        let target = ExportTarget {
            slug: &slug,
            def: &registry.collections[slug.as_str()],
            include_credentials,
        };
        let docs = export_collection(&conn, &target, &cfg.locale)?;

        collections.insert(slug.clone(), docs);
    }

    let export_file = ExportFile {
        format_version: EXPORT_FORMAT_VERSION,
        crap_version: env!("CARGO_PKG_VERSION").to_string(),
        exported_at: Utc::now().to_rfc3339(),
        collections,
    };

    write_export(&export_file, output)
}

#[cfg(test)]
mod tests {
    use serde_json::from_str;

    use super::*;

    fn empty_export() -> ExportFile {
        ExportFile {
            format_version: EXPORT_FORMAT_VERSION,
            crap_version: "test".to_string(),
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            collections: Map::new(),
        }
    }

    #[test]
    fn the_stage_is_a_sibling_of_the_final_file() {
        assert_eq!(
            staged_path(Path::new("/out/data/export.json")),
            PathBuf::from("/out/data/export.json.tmp")
        );
    }

    /// The export lands under its final name, complete, and the stage it was
    /// written through is gone.
    #[test]
    fn write_export_renames_a_complete_stage_into_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("export.json");

        write_export(&empty_export(), Some(path.clone())).expect("write export");

        let written: ExportFile =
            from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(written.format_version, EXPORT_FORMAT_VERSION);
        assert_eq!(written.crap_version, "test");
        assert!(!staged_path(&path).exists(), "the stage is renamed away");
    }

    /// Regression: the export was written straight to its final name, so a
    /// kill mid-write left a truncated file that looked like a valid export.
    /// The final name is only ever the renamed, complete stage: an existing
    /// file keeps its full content until the replacement is whole.
    #[test]
    fn a_failed_write_leaves_the_previous_export_intact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("export.json");
        fs::write(&path, "previous").expect("seed");

        // Staging into a directory that does not exist fails before the
        // rename, the way an interrupted write never reaches it.
        let missing = tmp.path().join("missing").join("export.json");
        assert!(write_atomically(&missing, "partial").is_err());

        assert_eq!(fs::read_to_string(&path).expect("read"), "previous");
        assert!(!staged_path(&missing).exists());
    }
}
