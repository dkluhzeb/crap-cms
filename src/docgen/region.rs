//! Marker-delimited generated regions inside otherwise hand-written
//! Markdown pages.
//!
//! A page that mixes prose with a generated table carries the table between
//! `<!-- GENERATED:<id> BEGIN -->` / `<!-- GENERATED:<id> END -->` markers.
//! `cargo xtask gen-doc-tables` re-renders every region (or diffs it in
//! `--check` mode), and an in-crate test per generator asserts the committed
//! region matches its renderer — the same double-gate as the whole-file
//! generators.

use anyhow::{Result, bail};

fn markers(id: &str) -> (String, String) {
    (
        format!("<!-- GENERATED:{id} BEGIN -->"),
        format!("<!-- GENERATED:{id} END -->"),
    )
}

/// Extract the generated region `id` from a document (without markers).
///
/// # Errors
///
/// Returns an error when either marker is missing or they are out of order.
pub fn extract(doc: &str, id: &str) -> Result<String> {
    let (begin, end) = markers(id);

    let Some(b) = doc.find(&begin) else {
        bail!("missing marker {begin}");
    };
    let Some(e) = doc.find(&end) else {
        bail!("missing marker {end}");
    };
    if e < b {
        bail!("markers for region '{id}' are out of order");
    }

    Ok(doc[b + begin.len()..e].trim_matches('\n').to_string())
}

/// Replace the generated region `id` with `content`, keeping the markers.
///
/// # Errors
///
/// Returns an error when the markers are missing or out of order.
pub fn inject(doc: &str, id: &str, content: &str) -> Result<String> {
    let (begin, end) = markers(id);

    let Some(b) = doc.find(&begin) else {
        bail!("missing marker {begin}");
    };
    let Some(e) = doc.find(&end) else {
        bail!("missing marker {end}");
    };
    if e < b {
        bail!("markers for region '{id}' are out of order");
    }

    Ok(format!(
        "{}{begin}\n{content}\n{end}{}",
        &doc[..b],
        &doc[e + end.len()..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_and_extract_round_trip() {
        let doc = "intro\n<!-- GENERATED:x BEGIN -->\nold\n<!-- GENERATED:x END -->\ntail";
        let out = inject(doc, "x", "new table").unwrap();
        assert!(out.contains(
            "intro\n<!-- GENERATED:x BEGIN -->\nnew table\n<!-- GENERATED:x END -->\ntail"
        ));
        assert_eq!(extract(&out, "x").unwrap(), "new table");
    }

    #[test]
    fn missing_marker_errors() {
        assert!(extract("no markers here", "x").is_err());
        assert!(inject("only <!-- GENERATED:x BEGIN -->", "x", "c").is_err());
    }
}
