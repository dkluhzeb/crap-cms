//! The name an upload is stored under: `{id}_{sanitized original name}`.

use nanoid::nanoid;

use super::validate::sanitize_filename;

/// Length of the generated id every stored upload name starts with
/// (`{id}_{sanitized}`).
///
/// [`stored_name`] writes it and [`original_filename`] strips it, so the two
/// read the same number and cannot drift.
pub const STORED_ID_LEN: usize = 10;

/// A fresh stored name for an upload the client named `original`: a new
/// random id, then the sanitized name.
pub(super) fn stored_name(original: &str) -> String {
    let id = nanoid!(STORED_ID_LEN);

    format!("{id}_{}", sanitize_filename(original))
}

/// The original filename inside a stored upload name, or `None` when `stored`
/// does not have the `{id}_{name}` shape the write path produces.
///
/// The id comes from `nanoid!()`, whose alphabet contains `_`, so the
/// separator is the underscore at exactly [`STORED_ID_LEN`] — splitting on the
/// *first* underscore truncates every name whose id happens to contain one.
#[must_use]
pub fn original_filename(stored: &str) -> Option<&str> {
    // `_` is never a UTF-8 continuation byte, so finding one at this index
    // also proves the index is a character boundary.
    if stored.as_bytes().get(STORED_ID_LEN) != Some(&b'_') {
        return None;
    }

    let name = &stored[STORED_ID_LEN + 1..];

    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the original name was recovered by splitting on the FIRST
    /// underscore, but `nanoid!()`'s alphabet contains `_` — so roughly one id
    /// in seven truncated the name a download was offered under.
    #[test]
    fn the_original_name_survives_an_id_that_contains_underscores() {
        assert_eq!(
            original_filename("ab_cd12_x9_quarterly-report.pdf"),
            Some("quarterly-report.pdf"),
        );
        assert_eq!(
            original_filename("V1StGXR8_Z_my_notes.txt"),
            Some("my_notes.txt"),
        );
    }

    /// A name that is not `{id}_{name}` is not a stored upload name: no
    /// separator at the id length, nothing after it, or too short.
    #[test]
    fn a_name_without_the_stored_shape_yields_nothing() {
        assert!(original_filename("photo.png").is_none());
        assert!(original_filename("abcdefghijphoto.png").is_none());
        assert!(original_filename("abcdefghij_").is_none());
        assert!(original_filename("").is_none());
    }

    /// The writer and the parser agree on the id length: a freshly generated
    /// stored name round-trips to the sanitized original.
    #[test]
    fn a_freshly_built_stored_name_round_trips() {
        let stored = stored_name("Holiday Photo.JPG");

        assert_eq!(original_filename(&stored), Some("holiday-photo.jpg"));
    }
}
