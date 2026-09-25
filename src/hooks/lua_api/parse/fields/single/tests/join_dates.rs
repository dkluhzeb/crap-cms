//! Join targets and flags, and date bounds, are checked at load.

use mlua::{Lua, Table};

use super::helpers::field_with;

// ── join strictness ─────────────────────────────────────────────────

/// Regression: a join field with missing / wrong-typed / empty
/// `collection`/`on` used to silently become empty strings ("validated
/// later" — which never happened) and matched nothing at populate time.
/// All three are now load-time errors.
#[test]
fn test_join_missing_collection_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("on", "author").unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("collection"), "{err}");
}

#[test]
fn test_join_non_string_on_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "posts").unwrap();
        f.set("on", true).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("'on'") && err.contains("boolean"), "{err}");
}

#[test]
fn test_join_empty_collection_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "").unwrap();
        f.set("on", "author").unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("non-empty"), "{err}");
}

#[test]
fn test_join_valid_config_parses() {
    let lua = Lua::new();
    field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "posts").unwrap();
        f.set("on", "author").unwrap();
    })
    .expect("valid join must parse");
}

/// A Join is virtual/read-only, so `required` / `localized` /
/// `required_locales` are meaningless and rejected at load (a
/// `localized + required` Join previously wedged non-draft writes).
fn join_with_flag(set_flag: impl Fn(&Table)) -> String {
    let lua = Lua::new();
    field_with(&lua, |f| {
        f.set("type", "join").unwrap();
        f.set("collection", "posts").unwrap();
        f.set("on", "author").unwrap();
        set_flag(f);
    })
    .unwrap_err()
    .to_string()
}

#[test]
fn test_join_required_flag_rejected() {
    let err = join_with_flag(|f| f.set("required", true).unwrap());
    assert!(
        err.contains("required") && err.contains("meaningless"),
        "{err}"
    );
}

#[test]
fn test_join_localized_flag_rejected() {
    let err = join_with_flag(|f| f.set("localized", true).unwrap());
    assert!(
        err.contains("localized") && err.contains("meaningless"),
        "{err}"
    );
}

#[test]
fn test_join_required_locales_rejected() {
    let err = join_with_flag(|f| f.set("required_locales", "all").unwrap());
    assert!(
        err.contains("required_locales") && err.contains("meaningless"),
        "{err}"
    );
}

// ── date-bound strictness ───────────────────────────────────────────

/// Regression: `min_date`/`max_date` used to silently drop non-string
/// values and accept arbitrary strings — the runtime compares the bound
/// lexically against the value's date part, so a malformed bound never
/// (or always) matched. Wrong type, bad format, and inverted ranges are
/// now load-time errors.
#[test]
fn test_date_bound_non_string_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "date").unwrap();
        f.set("min_date", 2024i64).unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("min_date"), "{err}");
}

#[test]
fn test_date_bound_bad_format_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "date").unwrap();
        f.set("max_date", "01.02.2024").unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("YYYY-MM-DD"), "{err}");
}

#[test]
fn test_date_bounds_inverted_range_rejected() {
    let lua = Lua::new();
    let err = field_with(&lua, |f| {
        f.set("type", "date").unwrap();
        f.set("min_date", "2024-12-31").unwrap();
        f.set("max_date", "2024-01-01").unwrap();
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("after max_date"), "{err}");
}

#[test]
fn test_date_bounds_valid_range_parses() {
    let lua = Lua::new();
    field_with(&lua, |f| {
        f.set("type", "date").unwrap();
        f.set("min_date", "2024-01-01").unwrap();
        f.set("max_date", "2024-12-31").unwrap();
    })
    .expect("valid date bounds must parse");
}
