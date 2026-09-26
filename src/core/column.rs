//! Predicates over the reserved system / auto-generated column namespace.

/// The non-prefixed auto-generated columns every collection table carries. These
/// are reserved for the framework and rejected as user field names. The single
/// source of truth shared by [`is_system_column`] and the field-name reservation
/// in `parse::fields`, so the two can never list different columns.
pub const AUTO_COLUMNS: &[&str] = &["id", "parent_id", "created_at", "updated_at"];

/// The optimistic-locking revision counter every collection and global row
/// carries — a system column, and the key a read document carries it under.
pub const REVISION_COLUMN: &str = "_revision";

/// Field names reserved for the framework that are not columns. `collection` is
/// the tag a populated relationship target carries beside its `id` (the key
/// polymorphic references are told apart by, and the one embedded-document
/// processing trusts); a user field of that name would collide with it in
/// every document envelope. Rejected as a user field name at any depth, like
/// [`AUTO_COLUMNS`].
pub const RESERVED_FIELD_NAMES: &[&str] = &["collection"];

/// Whether `name` may not be used as a user field name: an auto-generated
/// column ([`AUTO_COLUMNS`]) or a reserved envelope key
/// ([`RESERVED_FIELD_NAMES`]). The `_` prefix and the companion-column
/// suffixes are reserved separately by the field parser.
#[must_use]
pub fn is_reserved_field_name(name: &str) -> bool {
    AUTO_COLUMNS.contains(&name) || RESERVED_FIELD_NAMES.contains(&name)
}

/// Whether `name` is a system / auto-generated column rather than a user field.
///
/// The complete rule: every `_`-prefixed column is system (the frozen system
/// namespace — user fields may not begin with `_`), plus the non-prefixed
/// [`AUTO_COLUMNS`]. Used wherever a user-supplied column reference
/// (`default_sort`, `list_columns`) or a reserved field-name check needs the
/// single authoritative answer, so those checks can never drift out of agreement.
#[must_use]
pub fn is_system_column(name: &str) -> bool {
    name.starts_with('_') || AUTO_COLUMNS.contains(&name)
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
            REVISION_COLUMN,
            "_password_hash",
        ] {
            assert!(is_system_column(c), "{c}");
        }
        for u in ["title", "email", "body", "author"] {
            assert!(!is_system_column(u), "{u}");
        }
    }

    /// `collection` is the populated-target tag: reserved as a field name,
    /// but it is not a column (`default_sort` / `list_columns` never see it).
    #[test]
    fn collection_is_a_reserved_field_name_but_not_a_column() {
        assert!(is_reserved_field_name("collection"));
        assert!(!is_system_column("collection"));

        for c in AUTO_COLUMNS {
            assert!(is_reserved_field_name(c), "{c}");
        }

        assert!(!is_reserved_field_name("title"));
    }
}
