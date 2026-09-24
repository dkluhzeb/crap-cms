//! Up-front checks on an export, run before anything is written: its format,
//! the collections it names, duplicate ids, and TOTP secrets sealed under
//! another auth secret.

use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, from_str};

use crate::{
    cli,
    commands::export::{
        file::{EXPORT_FORMAT_VERSION, ExportFile},
        import_write::ImportBatch,
    },
    config::CrapConfig,
    core::{Registry, auth::open_totp_secret},
};

/// Read and parse an export, refusing a format newer than this binary
/// understands.
pub(super) fn read_export_file(file: &Path) -> Result<ExportFile> {
    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    let export_file: ExportFile = from_str(&content).context("Failed to parse JSON")?;

    if export_file.format_version > EXPORT_FORMAT_VERSION {
        bail!(
            "This export uses format version {} but this crap-cms only supports up to {}. \
             Upgrade crap-cms to import it.",
            export_file.format_version,
            EXPORT_FORMAT_VERSION
        );
    }

    let current = env!("CARGO_PKG_VERSION");
    if let Some(warning) =
        CrapConfig::check_version_against(Some(&export_file.crap_version), current)
    {
        cli::warning(&warning.replace("config requires", "export file was created with"));
    }

    Ok(export_file)
}

/// The collections to import: the one `collection_filter` names, or every
/// collection in the export.
pub(super) fn import_slugs(
    export_file: &ExportFile,
    collection_filter: Option<&str>,
) -> Result<Vec<String>> {
    let Some(slug) = collection_filter else {
        return Ok(export_file.collections.keys().cloned().collect());
    };

    if !export_file.collections.contains_key(slug) {
        bail!("Collection '{slug}' not found in import file");
    }

    Ok(vec![slug.to_string()])
}

/// Verify every collection in the import set exists in the registry BEFORE
/// any write, so an unknown slug is reported up front rather than midway.
pub(super) fn check_import_slugs(registry: &Registry, slugs: &[String]) -> Result<()> {
    let unknown: Vec<&str> = slugs
        .iter()
        .filter(|s| registry.get_collection(s).is_none())
        .map(String::as_str)
        .collect();

    if unknown.is_empty() {
        return Ok(());
    }

    bail!(
        "Collection(s) {} exist in the import file but not in the schema — nothing was imported",
        unknown
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Refuse an export that lists a document twice in one collection: the later
/// copy would silently overwrite the earlier one.
pub(super) fn check_duplicate_ids(batches: &[ImportBatch<'_>]) -> Result<()> {
    let mut duplicates = Vec::new();

    for batch in batches {
        let mut seen = HashSet::new();
        let ids = batch
            .docs
            .iter()
            .filter_map(|doc| doc.get("id").and_then(Value::as_str));

        for id in ids {
            if !seen.insert(id) {
                duplicates.push(format!("{}/{id}", batch.target.slug));
            }
        }
    }

    if duplicates.is_empty() {
        return Ok(());
    }

    bail!(
        "The export lists these documents more than once: {}. Nothing was imported.",
        duplicates.join(", ")
    )
}

/// Refuse accounts whose TOTP secret doesn't open with this installation's
/// auth secret. It was sealed under another one, and an account whose secret
/// can't be read re-enrolls on its next login — letting whoever holds the
/// password register an authenticator of their own.
pub(super) fn check_totp_secrets(batches: &[ImportBatch<'_>], auth_secret: &str) -> Result<()> {
    let sealed_elsewhere: Vec<String> = batches
        .iter()
        .filter(|batch| batch.target.credential_columns.contains(&"_totp_secret"))
        .flat_map(|batch| {
            batch.docs.iter().filter_map(move |doc| {
                let sealed = doc.pointer("/_credentials/_totp_secret")?.as_str()?;
                let id = doc.get("id").and_then(Value::as_str).unwrap_or("?");

                (open_totp_secret(auth_secret, sealed).is_none())
                    .then(|| format!("{}/{id}", batch.target.slug))
            })
        })
        .collect();

    if sealed_elsewhere.is_empty() {
        return Ok(());
    }

    bail!(
        "{} account(s) carry a TOTP secret sealed with a different auth secret: {}. Import into \
         an installation that uses the same auth secret (a backup carries a generated one), or \
         export without --include-credentials. Nothing was imported.",
        sealed_elsewhere.len(),
        sealed_elsewhere.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        commands::export::import_row::ImportTarget,
        config::LocaleConfig,
        core::{CollectionDefinition, auth::seal_totp_secret},
    };

    /// Regression: an unknown slug must be rejected before any collection is
    /// written (previously detected lazily, after earlier collections had
    /// already committed).
    #[test]
    fn unknown_import_slugs_rejected_up_front() {
        let shared = Registry::shared();
        shared
            .write()
            .unwrap()
            .register_collection(CollectionDefinition::new("posts"));
        let registry = (*Registry::snapshot(&shared)).clone();

        assert!(check_import_slugs(&registry, &["posts".to_string()]).is_ok());

        let err = check_import_slugs(
            &registry,
            &[
                "posts".to_string(),
                "ghosts".to_string(),
                "zombies".to_string(),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("'ghosts', 'zombies'"), "{err}");
        assert!(err.contains("nothing was imported"), "{err}");
    }

    /// A TOTP secret sealed under another auth secret is refused, naming the
    /// account; one sealed under this installation's secret imports.
    #[test]
    fn totp_secrets_sealed_with_another_auth_secret_are_refused() {
        let def = CollectionDefinition::new("users");
        let locale = LocaleConfig::default();
        let target = || {
            ImportTarget::builder("users", &def, &locale)
                .credential_columns(vec!["_totp_secret"])
                .build()
        };

        let here = seal_totp_secret("this-secret", "JBSWY3DPEHPK3PXP").unwrap();
        let elsewhere = seal_totp_secret("another-secret", "JBSWY3DPEHPK3PXP").unwrap();

        let docs = [
            json!({ "id": "u1", "_credentials": { "_totp_secret": here } }),
            json!({ "id": "u2", "_credentials": { "_totp_secret": elsewhere } }),
        ];

        let refused = [ImportBatch {
            target: target(),
            docs: &docs,
        }];
        let err = check_totp_secrets(&refused, "this-secret")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("users/u2") && !err.contains("users/u1"),
            "{err}"
        );

        let accepted = [ImportBatch {
            target: target(),
            docs: &docs[..1],
        }];
        assert!(check_totp_secrets(&accepted, "this-secret").is_ok());
    }

    /// A document listed twice in one collection is refused before anything
    /// is written, instead of the later copy overwriting the earlier one.
    #[test]
    fn duplicate_document_ids_are_refused() {
        let def = CollectionDefinition::new("posts");
        let locale = LocaleConfig::default();
        let docs = vec![
            json!({ "id": "p1" }),
            json!({ "id": "p2" }),
            json!({ "id": "p1" }),
        ];
        let batches = [ImportBatch {
            target: ImportTarget::builder("posts", &def, &locale).build(),
            docs: &docs,
        }];

        let err = check_duplicate_ids(&batches).unwrap_err().to_string();

        assert!(err.contains("posts/p1") && !err.contains("p2"), "{err}");
    }
}
