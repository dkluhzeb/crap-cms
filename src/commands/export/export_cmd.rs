//! `export` command — dump collection data to JSON.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use serde_json::{Map, Value};

use crate::{
    cli,
    commands::{export::file::ExportFile, load_config_and_sync},
    db::query,
};

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
) -> Result<()> {
    let (pool, registry) = load_config_and_sync(config_dir)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let mut collections_data = Map::new();

    let slugs: Vec<String> = if let Some(slug) = collection_filter {
        if registry.get_collection(slug).is_none() {
            bail!("Collection '{slug}' not found");
        }
        vec![slug.to_string()]
    } else {
        let mut s: Vec<String> = registry
            .collections
            .keys()
            .map(std::string::ToString::to_string)
            .collect();
        s.sort();
        s
    };

    for slug in &slugs {
        let def = &registry.collections[slug.as_str()];
        let find_query = query::FindQuery::default();

        let mut docs = query::find(&conn, slug, def, &find_query, None)?;

        for doc in &mut docs {
            query::hydrate_document(&conn, slug, &def.fields, doc, None, None)?;
        }

        let docs_json: Vec<Value> = docs
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;

        collections_data.insert(slug.clone(), Value::Array(docs_json));
    }

    let output_file = ExportFile {
        format_version: crate::commands::export::file::EXPORT_FORMAT_VERSION,
        crap_version: env!("CARGO_PKG_VERSION").to_string(),
        exported_at: Utc::now().to_rfc3339(),
        collections: collections_data,
    };

    match output {
        Some(path) => {
            let content = serde_json::to_string_pretty(&output_file)?;

            fs::write(&path, content)
                .with_context(|| format!("Failed to write {}", path.display()))?;

            cli::success(&format!(
                "Exported {} collection(s) to {}",
                slugs.len(),
                path.display()
            ));
        }
        None => {
            println!("{}", serde_json::to_string_pretty(&output_file)?);
        }
    }

    Ok(())
}
