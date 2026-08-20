//! Predicates over the reserved system / auto-generated column namespace.

/// Whether `name` is a system / auto-generated column rather than a user field.
///
/// The complete rule: every `_`-prefixed column is system (the frozen system
/// namespace — user fields may not begin with `_`), plus the non-prefixed auto
/// columns `id` / `parent_id` / `created_at` / `updated_at`. Used wherever a
/// user-supplied column reference (`default_sort`, `list_columns`) or a reserved
/// field-name check needs the single authoritative answer, so those checks can
/// never drift out of agreement.
#[must_use]
pub fn is_system_column(name: &str) -> bool {
    name.starts_with('_') || matches!(name, "id" | "parent_id" | "created_at" | "updated_at")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_columns_recognized() {
        for c in [
            "id",
            "parent_id",
            "created_at",
            "updated_at",
            "_status",
            "_deleted_at",
            "_ref_count",
            "_password_hash",
        ] {
            assert!(is_system_column(c), "{c}");
        }
        for u in ["title", "email", "body", "author"] {
            assert!(!is_system_column(u), "{u}");
        }
    }
}
