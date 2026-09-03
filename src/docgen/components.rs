//! Generate the component reference tables (`docs/src/admin-ui/reference/
//! components.md`, regions `components-*`) from the component sources
//! themselves: every `customElements.define('crap-…')` under
//! `static/components/` must carry `@category` and `@stability` header
//! annotations, and the table rows are parsed from those headers — the
//! hand tables drifted every time a component landed or changed tier.

use include_dir::Dir;

use crate::scaffold::EMBEDDED_STATIC;

struct ComponentDoc {
    tag: String,
    stability: String,
    summary: String,
    source: String,
    /// The `crap:*-request` discovery event (singletons), resolved from the
    /// `EV_*_REQUEST` identifier the component uses via `events.js`.
    event: Option<String>,
}

/// Resolve the discovery event a component listens for: find the first
/// `EV_…_REQUEST` identifier in its source and look up the literal in
/// `components/events.js` (`export const EV_X = 'crap:…';`).
fn discovery_event(src: &str) -> Option<String> {
    let idx = src.find("EV_")?;
    let ident: String = src[idx..]
        .chars()
        .take_while(|c| c.is_ascii_uppercase() || *c == '_' || c.is_ascii_digit())
        .collect();
    if !ident.ends_with("_REQUEST") {
        // Scan further occurrences for a *_REQUEST identifier.
        let mut rest = &src[idx + ident.len()..];
        loop {
            let i = rest.find("EV_")?;
            let id: String = rest[i..]
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || *c == '_' || c.is_ascii_digit())
                .collect();
            if id.ends_with("_REQUEST") {
                return resolve_event_literal(&id);
            }
            rest = &rest[i + id.len().max(3)..];
        }
    }
    resolve_event_literal(&ident)
}

/// Look up `export const <ident> = 'literal';` in `components/events.js`.
fn resolve_event_literal(ident: &str) -> Option<String> {
    let events = EMBEDDED_STATIC
        .get_file("components/events.js")?
        .contents_utf8()?;
    let needle = format!("export const {ident} = '");
    let i = events.find(&needle)?;
    let rest = &events[i + needle.len()..];
    let literal: String = rest.chars().take_while(|c| *c != '\'').collect();
    Some(literal)
}

fn walk<'a>(dir: &'a Dir<'a>, out: &mut Vec<&'a include_dir::File<'a>>) {
    for f in dir.files() {
        out.push(f);
    }
    for d in dir.dirs() {
        walk(d, out);
    }
}

/// Header title line → human summary. Handles both orders:
/// `Toast notifications — `<crap-toast>`.` and
/// `<crap-confirm> — Confirmation guard …`.
fn summary_from_header(header: &str, tag: &str) -> String {
    for line in header.lines() {
        let line = line.trim_start_matches([' ', '*']).trim();
        if !line.contains(tag) {
            continue;
        }
        if let Some((before, after)) = line.split_once('—') {
            let before = before.trim();
            let after = after.trim().trim_end_matches('.');
            let cleaned = if before.contains(tag) {
                after
            } else {
                before.trim_end_matches('.')
            };
            let cleaned = cleaned
                .replace(&format!("`<{tag}>`"), "")
                .replace(&format!("<{tag}>"), "");
            return cleaned.trim().trim_end_matches('.').to_string();
        }
    }

    String::new()
}

fn annotation(header: &str, key: &str) -> Option<String> {
    header.lines().find_map(|l| {
        l.trim_start_matches([' ', '*'])
            .trim()
            .strip_prefix(&format!("@{key} "))
            .map(|v| v.trim().to_string())
    })
}

fn collect() -> Vec<(String, ComponentDoc)> {
    let components = EMBEDDED_STATIC
        .get_dir("components")
        .expect("static/components embedded");

    let mut files = Vec::new();
    walk(components, &mut files);

    let mut docs = Vec::new();

    for file in files {
        if file.path().extension().is_none_or(|e| e != "js") {
            continue;
        }
        let path = file.path().to_string_lossy().to_string();
        let Some(src) = file.contents_utf8() else {
            continue;
        };

        // Header = the leading /** … */ block.
        let header = src.split_once("*/").map(|(h, _)| h).unwrap_or_default();

        let mut rest = src;
        while let Some(i) = rest.find("customElements.define('") {
            rest = &rest[i + "customElements.define('".len()..];
            let tag: String = rest.chars().take_while(|c| *c != '\'').collect();

            let category = annotation(header, "category").unwrap_or_else(|| {
                panic!("{path}: defines <{tag}> but has no @category header annotation")
            });
            let stability = annotation(header, "stability").unwrap_or_else(|| {
                panic!("{path}: defines <{tag}> but has no @stability header annotation")
            });

            docs.push((
                category,
                ComponentDoc {
                    summary: summary_from_header(header, &tag),
                    tag,
                    stability,
                    source: format!("static/{path}"),
                    event: discovery_event(src),
                },
            ));
        }
    }

    docs.sort_by(|a, b| a.1.tag.cmp(&b.1.tag));
    docs
}

/// Render the table for one `@category` (`singleton`, `form-field`,
/// `enhancer`).
///
/// # Panics
///
/// Panics when a component file defines a custom element without
/// `@category` / `@stability` header annotations — the drift gate this
/// generator exists for.
#[must_use]
pub fn generate_component_table(category: &str) -> String {
    use std::fmt::Write as _;

    // Singletons carry their discovery event (`crap:*-request`); the other
    // categories interact via markup/attributes, documented in their prose
    // sections and `events.md`.
    let with_events = category == "singleton";

    let mut out = if with_events {
        String::from("| Tag | Event | Stability | Summary | Source |\n|---|---|---|---|---|\n")
    } else {
        String::from("| Tag | Stability | Summary | Source |\n|---|---|---|---|\n")
    };

    for (cat, d) in collect() {
        if cat != category {
            continue;
        }
        if with_events {
            let event = d
                .event
                .map_or_else(|| "—".to_string(), |e| format!("`{e}`"));
            let _ = writeln!(
                out,
                "| `<{}>` | {event} | {} | {} | `{}` |",
                d.tag, d.stability, d.summary, d.source
            );
        } else {
            let _ = writeln!(
                out,
                "| `<{}>` | {} | {} | `{}` |",
                d.tag, d.stability, d.summary, d.source
            );
        }
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_renders_known_components() {
        let s = generate_component_table("singleton");
        assert!(
            s.contains("| `<crap-toast>` | `crap:toast-request` | stable |"),
            "{s}"
        );
        // Mounted directly, no request event.
        let dialog_row = s
            .lines()
            .find(|l| l.contains("crap-session-dialog"))
            .unwrap();
        assert!(dialog_row.contains("| — |"), "{dialog_row}");

        let f = generate_component_table("form-field");
        assert!(f.contains("`<crap-relationship-search>`"), "{f}");
        assert!(f.contains("`<crap-array-row>`"), "{f}");

        let e = generate_component_table("enhancer");
        assert!(e.contains("`<crap-filter-builder>`"), "{e}");
        assert!(e.contains("`<crap-collapsible>`"), "{e}");
    }
}
