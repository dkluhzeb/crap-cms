//! Guard: every translation key the admin can render exists in every shipped
//! locale, with the same placeholders.
//!
//! A missing key does not fail loudly — the translator falls back to English,
//! then to the key itself, so the admin shows `validation.password_policy`
//! under a password field or a raw `fields` heading. This scan closes that
//! gap for the keys spelled literally in the source:
//!
//! - `{{t "key"}}` / `(t "key")` and every `*_key="key"` partial argument in
//!   `templates/`, including that the call passes every placeholder the
//!   English string interpolates;
//! - every `"validation.…"` key literal in production Rust (field errors);
//! - every literal page title (`PageMeta::new(PageType::…, "key")`), which the
//!   layout translates;
//! - and that `en.json` and `de.json` carry the same keys with the same
//!   placeholders.
//!
//! Keys chosen at runtime (a field label run through `t`, an operator key held
//! in a variable) are out of reach of a textual scan. The admin JS keys have
//! their own guard beside `ADMIN_JS_KEYS`.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use regex::Regex;

mod common;

use common::production_code;

/// The shipped locales, by file name.
const LOCALES: [&str; 2] = ["en", "de"];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn load_locale(locale: &str) -> HashMap<String, String> {
    let path = root().join("translations").join(format!("{locale}.json"));
    let text = fs::read_to_string(&path).expect("translation file readable");

    serde_json::from_str(&text).expect("translation file is a flat string map")
}

fn locales() -> Vec<(&'static str, HashMap<String, String>)> {
    LOCALES.iter().map(|l| (*l, load_locale(l))).collect()
}

/// The `{{name}}` placeholders a translation interpolates.
fn placeholders(template: &str) -> BTreeSet<String> {
    let re = Regex::new(r"\{\{(\w+)\}\}").expect("placeholder regex");

    re.captures_iter(template)
        .map(|c| c[1].to_string())
        .collect()
}

/// Every file under `dir` with extension `ext`, recursively.
fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            files(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

fn relative(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// A key some source location uses, and the placeholder names it passes
/// (`None` when the call site's arguments are not checked).
struct KeyUse {
    location: String,
    key: String,
    passed: Option<BTreeSet<String>>,
}

/// Every literal key the templates translate.
fn template_key_uses() -> Vec<KeyUse> {
    let t_call = Regex::new(r#"(?:\{\{|\()t\s+"([^"]+)"([^})]*)"#).expect("t regex");
    let key_arg = Regex::new(r#"\b\w+_key="([^"{]+)""#).expect("key arg regex");
    let hash_arg = Regex::new(r"(\w+)=").expect("hash arg regex");

    let mut paths = Vec::new();
    files(&root().join("templates"), "hbs", &mut paths);
    paths.sort();

    let mut uses = Vec::new();

    for path in paths {
        let src = fs::read_to_string(&path).expect("template readable");
        let location = relative(&path);

        for c in t_call.captures_iter(&src) {
            let passed = hash_arg
                .captures_iter(&c[2])
                .map(|a| a[1].to_string())
                .collect();

            uses.push(KeyUse {
                location: location.clone(),
                key: c[1].to_string(),
                passed: Some(passed),
            });
        }

        for c in key_arg.captures_iter(&src) {
            uses.push(KeyUse {
                location: location.clone(),
                key: c[1].to_string(),
                passed: None,
            });
        }
    }

    uses
}

/// Every validation key and literal page title in production Rust.
fn rust_key_uses() -> Vec<KeyUse> {
    let validation = Regex::new(r#""(validation\.[a-z_]+)""#).expect("validation regex");
    let page_title =
        Regex::new(r#"PageMeta::new\(\s*PageType::\w+,\s*"([^"]+)""#).expect("title regex");

    let mut paths = Vec::new();
    files(&root().join("src"), "rs", &mut paths);
    paths.sort();

    let mut uses = Vec::new();

    for path in paths {
        let src = production_code(&fs::read_to_string(&path).expect("source readable"));
        let location = relative(&path);

        for re in [&validation, &page_title] {
            for c in re.captures_iter(&src) {
                uses.push(KeyUse {
                    location: location.clone(),
                    key: c[1].to_string(),
                    passed: None,
                });
            }
        }
    }

    uses
}

/// Every problem with `uses` against the shipped locales.
fn problems(uses: &[KeyUse]) -> Vec<String> {
    let locales = locales();
    let mut out = Vec::new();

    for u in uses {
        for (name, strings) in &locales {
            if !strings.contains_key(&u.key) {
                out.push(format!(
                    "{}: `{}` missing from {name}.json",
                    u.location, u.key
                ));
            }
        }

        let (Some(passed), Some(en)) = (&u.passed, locales[0].1.get(&u.key)) else {
            continue;
        };

        let unpassed: Vec<String> = placeholders(en).difference(passed).cloned().collect();
        if !unpassed.is_empty() {
            out.push(format!(
                "{}: `{}` interpolates {unpassed:?} but the call does not pass it",
                u.location, u.key
            ));
        }
    }

    out
}

#[test]
fn every_template_translation_key_exists_in_every_locale() {
    let uses = template_key_uses();
    assert!(!uses.is_empty(), "the template scan found no keys at all");

    let problems = problems(&uses);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_validation_key_and_page_title_exists_in_every_locale() {
    let uses = rust_key_uses();
    assert!(
        uses.iter().any(|u| u.key == "validation.required"),
        "the Rust scan no longer finds the validation keys"
    );

    let problems = problems(&uses);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn locales_carry_the_same_keys_and_placeholders() {
    let locales = locales();
    let (base_name, base) = &locales[0];
    let mut problems = Vec::new();

    for (name, strings) in &locales[1..] {
        for (key, en) in base {
            match strings.get(key) {
                None => problems.push(format!(
                    "`{key}` is in {base_name}.json but not {name}.json"
                )),
                Some(other) if placeholders(en) != placeholders(other) => problems.push(format!(
                    "`{key}` placeholders differ: {base_name} {:?}, {name} {:?}",
                    placeholders(en),
                    placeholders(other)
                )),
                Some(_) => {}
            }
        }

        for key in strings.keys().filter(|k| !base.contains_key(*k)) {
            problems.push(format!(
                "`{key}` is in {name}.json but not {base_name}.json"
            ));
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Positive control: the scan reports a key that does not exist.
#[test]
fn the_scan_reports_a_missing_key() {
    let problems = problems(&[KeyUse {
        location: "fixture".to_string(),
        key: "no_such_key_anywhere".to_string(),
        passed: None,
    }]);

    assert_eq!(problems.len(), LOCALES.len());
}
