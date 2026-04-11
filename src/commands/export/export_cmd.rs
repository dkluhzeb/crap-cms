//! `export` command — dump collection data to JSON.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::Utc;
use serde_json::{Map, Value, json};

use crate::{cli, commands::load_config_and_sync, db::query};

/// Export collection data to JSON.
#[cfg(not(tarpaulin_include))]
pub fn export(
    config_dir: &Path,
    collection_filter: Option<String>,
    output: Option<PathBuf>,
) -> Result<()> {
    let (pool, registry) = load_config_and_sync(config_dir)?;

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let conn = pool.get().context("Failed to get database connection")?;

    let mut collections_data = Map::new();

    let slugs: Vec<String> = if let Some(ref slug) = collection_filter {
        if reg.get_collection(slug).is_none() {
            bail!("Collection '{}' not found", slug);
        }
        vec![slug.clone()]
    } else {
        let mut s: Vec<String> = reg.collections.keys().map(|s| s.to_string()).collect();
        s.sort();
        s
    };

    for slug in &slugs {
        let def = &reg.collections[slug.as_str()];
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

    let output_json = json!({
        "crap_version": env!("CARGO_PKG_VERSION"),
        "exported_at": Utc::now().to_rfc3339(),
        "collections": collections_data,
    });

    match output {
        Some(path) => {
            let content = serde_json::to_string_pretty(&output_json)?;

            fs::write(&path, content)
                .with_context(|| format!("Failed to write {}", path.display()))?;

            cli::success(&format!(
                "Exported {} collection(s) to {}",
                slugs.len(),
                path.display()
            ));
        }
        None => {
            println!("{}", serde_json::to_string_pretty(&output_json)?);
        }
    }

    Ok(())
}
