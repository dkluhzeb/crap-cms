//! Ranged reads: the request shape ([`ByteRange`]) every storage backend
//! accepts, and the read result ([`RangedObject`]) every backend returns.
//!
//! The types live next to the trait rather than in the serve handler because
//! a backend that can ask its remote for a slice (S3 sends an HTTP `Range`
//! header) must be able to say which slice it actually got back — the serve
//! route needs that to answer `206` with a truthful `Content-Range`.

/// A byte range requested from a stored object, in the two forms an HTTP
/// `Range: bytes=…` header can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `bytes=<start>-<end>` (both inclusive), or `bytes=<start>-` when `end`
    /// is `None`.
    Offset {
        /// First byte offset the caller wants.
        start: u64,
        /// Last byte offset the caller wants, inclusive. `None` runs to the
        /// end of the object.
        end: Option<u64>,
    },
    /// `bytes=-<n>` — the final `n` bytes of the object.
    Suffix(u64),
}

impl ByteRange {
    /// `bytes=<start>-<end>`, both offsets inclusive.
    #[must_use]
    pub fn inclusive(start: u64, end: u64) -> Self {
        Self::Offset {
            start,
            end: Some(end),
        }
    }

    /// `bytes=<start>-` — from `start` to the end of the object.
    #[must_use]
    pub fn from_start(start: u64) -> Self {
        Self::Offset { start, end: None }
    }

    /// Resolve against a known total size, yielding the inclusive
    /// `(first, last)` offsets to serve.
    ///
    /// `None` means the range is unsatisfiable — the caller answers `416`: a
    /// start at or past the end of the object, a zero-length suffix, or an
    /// empty object (no offset can be satisfied in a zero-byte object).
    #[must_use]
    pub fn resolve(self, total: u64) -> Option<(u64, u64)> {
        if total == 0 {
            return None;
        }

        let last = total - 1;

        match self {
            Self::Offset { start, end } => {
                if start > last {
                    return None;
                }

                let end = end.unwrap_or(last).min(last);

                (start <= end).then_some((start, end))
            }
            Self::Suffix(0) => None,
            Self::Suffix(n) => Some((total.saturating_sub(n), last)),
        }
    }
}

/// The outcome of one backend read: the bytes, plus whatever the backend knows
/// about the object they came from.
///
/// `range` is `None` when `data` is the whole object — either because no range
/// was asked for, or because the backend could not slice remotely and the
/// caller must slice the full bytes itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangedObject {
    /// The bytes read.
    pub data: Vec<u8>,
    /// Total size of the stored object, when the backend reports it.
    pub total_size: Option<u64>,
    /// Inclusive `(first, last)` offsets that `data` covers within the object.
    pub range: Option<(u64, u64)>,
    /// The backend's entity tag for the object, quotes stripped, when it has
    /// one.
    pub etag: Option<String>,
    /// The backend's last-modified stamp as an HTTP-date string, when it has
    /// one.
    pub last_modified: Option<String>,
}

impl RangedObject {
    /// Start building a read result holding the object's full bytes.
    #[must_use]
    pub fn whole(data: Vec<u8>) -> RangedObjectBuilder {
        RangedObjectBuilder::new(data)
    }
}

/// Builder for [`RangedObject`].
///
/// `data` is taken in `new()`; `total_size` defaults to the length of `data`
/// (correct for a whole-object read) and every other field is absent until a
/// backend supplies it.
pub struct RangedObjectBuilder {
    data: Vec<u8>,
    total_size: Option<u64>,
    range: Option<(u64, u64)>,
    etag: Option<String>,
    last_modified: Option<String>,
}

impl RangedObjectBuilder {
    /// Create a builder over the bytes a backend read.
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        let total_size = u64::try_from(data.len()).ok();

        Self {
            data,
            total_size,
            range: None,
            etag: None,
            last_modified: None,
        }
    }

    /// Record the object's total size, overriding the default (the length of
    /// `data`) — a backend that returned a slice knows the whole is larger.
    #[must_use]
    pub fn total_size(mut self, total: Option<u64>) -> Self {
        if let Some(total) = total {
            self.total_size = Some(total);
        }

        self
    }

    /// Record the inclusive offsets `data` covers within the object.
    #[must_use]
    pub fn range(mut self, range: Option<(u64, u64)>) -> Self {
        self.range = range;
        self
    }

    /// Record the backend's entity tag (quotes stripped by the backend).
    #[must_use]
    pub fn etag(mut self, etag: Option<String>) -> Self {
        self.etag = etag;
        self
    }

    /// Record the backend's last-modified stamp as an HTTP-date string.
    #[must_use]
    pub fn last_modified(mut self, last_modified: Option<String>) -> Self {
        self.last_modified = last_modified;
        self
    }

    /// Build the final [`RangedObject`].
    #[must_use]
    pub fn build(self) -> RangedObject {
        RangedObject {
            data: self.data,
            total_size: self.total_size,
            range: self.range,
            etag: self.etag,
            last_modified: self.last_modified,
        }
    }
}

/// Slice `data` to `range` locally — the fallback every backend that cannot
/// ask its remote for a slice shares (the trait's default `get_range`, and a
/// backend whose remote ignored the `Range` header and answered `200`).
///
/// Returns the read result, or `None` when the range is unsatisfiable.
#[must_use]
pub fn slice_locally(data: &[u8], range: &ByteRange) -> Option<RangedObject> {
    let total = u64::try_from(data.len()).ok()?;
    let (first, last) = range.resolve(total)?;

    // Both offsets came from `resolve`, so they are inside `data` and fit a
    // usize on any platform that could hold `data` in the first place.
    let from = usize::try_from(first).ok()?;
    let to = usize::try_from(last).ok()?;

    Some(
        RangedObject::whole(data[from..=to].to_vec())
            .total_size(Some(total))
            .range(Some((first, last)))
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, RangedObject, slice_locally};

    #[test]
    fn resolves_offset_ranges_against_the_total() {
        assert_eq!(ByteRange::inclusive(0, 9).resolve(100), Some((0, 9)));
        assert_eq!(ByteRange::from_start(90).resolve(100), Some((90, 99)));

        // An end past the last byte is clamped, not rejected.
        assert_eq!(ByteRange::inclusive(90, 500).resolve(100), Some((90, 99)));
    }

    #[test]
    fn resolves_suffix_ranges_and_clamps_an_oversized_one() {
        assert_eq!(ByteRange::Suffix(10).resolve(100), Some((90, 99)));
        assert_eq!(ByteRange::Suffix(500).resolve(100), Some((0, 99)));
    }

    #[test]
    fn an_unsatisfiable_range_resolves_to_none() {
        assert_eq!(ByteRange::inclusive(100, 200).resolve(100), None);
        assert_eq!(ByteRange::from_start(100).resolve(100), None);
        assert_eq!(ByteRange::Suffix(0).resolve(100), None);
        assert_eq!(ByteRange::inclusive(0, 0).resolve(0), None);
    }

    #[test]
    fn slicing_locally_returns_the_requested_bytes_and_the_full_total() {
        let object = slice_locally(b"0123456789", &ByteRange::inclusive(2, 4))
            .expect("range is satisfiable");

        assert_eq!(object.data, b"234");
        assert_eq!(object.total_size, Some(10));
        assert_eq!(object.range, Some((2, 4)));
    }

    #[test]
    fn slicing_an_unsatisfiable_range_returns_none() {
        assert!(slice_locally(b"0123456789", &ByteRange::from_start(10)).is_none());
    }

    #[test]
    fn a_whole_object_read_reports_its_own_length_as_the_total() {
        let object = RangedObject::whole(b"abcd".to_vec()).build();

        assert_eq!(object.total_size, Some(4));
        assert!(object.range.is_none());
        assert!(object.etag.is_none());
    }
}
