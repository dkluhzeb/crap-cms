//! URL building and encoding utilities for list pages.

/// Shared context for building list URLs — base path, search, sort, page
/// size, and filter params.
///
/// Used by `resolve_columns`, `compute_title_sort`, the search form, and the
/// pagination helpers so every list link carries the same view state.
pub(crate) struct ListUrlContext<'a> {
    pub base_url: &'a str,
    pub search: Option<&'a str>,
    pub sort: Option<&'a str>,
    /// The page size the viewer asked for; `None` keeps the configured
    /// default out of the URL.
    pub per_page: Option<i64>,
    /// The `where[…]` params (and `trash=1`) of the current view, raw.
    pub where_params: &'a str,
}

impl ListUrlContext<'_> {
    /// Build a page-based list URL.
    pub fn page_url(&self, page: i64) -> String {
        build_list_url(
            self.base_url,
            page,
            self.per_page,
            self.search,
            self.sort,
            self.where_params,
        )
    }

    /// Build a cursor-based list URL.
    pub fn cursor_url(&self, cursor_param: &str, cursor_value: &str) -> String {
        build_list_url_with_cursor(
            self.base_url,
            1,
            self.per_page,
            self.search,
            self.sort,
            self.where_params,
            Some((cursor_param, cursor_value)),
        )
    }

    /// Build a sort URL (page 1, with a specific sort override).
    pub fn sort_url(&self, sort_field: &str) -> String {
        build_list_url(
            self.base_url,
            1,
            self.per_page,
            self.search,
            Some(sort_field),
            self.where_params,
        )
    }

    /// The "clear search" link: page 1 of the same view without the search
    /// term — sort, page size, filters, and the trash view stay.
    pub fn clear_search_url(&self) -> String {
        build_list_url(
            self.base_url,
            1,
            self.per_page,
            None,
            self.sort,
            self.where_params,
        )
    }

    /// The `(name, value)` pairs (decoded) the search form carries as hidden
    /// inputs, so a search keeps the sort, page size, filters, and trash view
    /// and only resets the page position.
    pub fn search_form_params(&self) -> Vec<(String, String)> {
        let mut params = Vec::new();

        if let Some(sort) = self.sort {
            params.push(("sort".to_string(), sort.to_string()));
        }

        if let Some(per_page) = self.per_page {
            params.push(("per_page".to_string(), per_page.to_string()));
        }

        let carried = self
            .where_params
            .split('&')
            .filter(|part| is_view_param(part))
            .map(|part| {
                let (key, value) = part.split_once('=').unwrap_or((part, ""));
                (url_decode(key), url_decode(value))
            });
        params.extend(carried);

        params
    }
}

/// Whether a raw query entry is view state every list link carries: a
/// `where[…]` filter or the trash flag.
fn is_view_param(part: &str) -> bool {
    part.starts_with("where%5B") || part.starts_with("where[") || part == "trash=1"
}

/// Simple percent-decoding for URL query values.
///
/// Collects decoded bytes into a `Vec<u8>` then converts via `String::from_utf8_lossy`
/// so multi-byte UTF-8 sequences (e.g. `%C3%A9` → `é`) decode correctly.
/// Malformed `%XX` sequences are preserved literally instead of being silently dropped.
pub(crate) fn url_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut iter = s.bytes();

    while let Some(b) = iter.next() {
        if b == b'+' {
            bytes.push(b' ');
        } else if b == b'%' {
            let hi = iter.next();
            let lo = hi.and_then(|_| iter.next());

            if let (Some(h), Some(l)) = (
                hi.and_then(|c| (c as char).to_digit(16)),
                lo.and_then(|c| (c as char).to_digit(16)),
            ) {
                bytes.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
            } else {
                // Malformed %XX — preserve the literal characters
                bytes.push(b'%');
                if let Some(h) = hi {
                    bytes.push(h);
                }
                if let Some(l) = lo {
                    bytes.push(l);
                }
            }
        } else {
            bytes.push(b);
        }
    }

    String::from_utf8_lossy(&bytes).into_owned()
}

/// Build a list URL preserving all query params (pagination, search, sort, filters).
///
/// For page-based pagination, includes `page=N`. For cursor-based pagination,
/// pass `page=0` and provide a cursor string instead.
pub(crate) fn build_list_url(
    base: &str,
    page: i64,
    per_page: Option<i64>,
    search: Option<&str>,
    sort: Option<&str>,
    raw_where: &str,
) -> String {
    build_list_url_with_cursor(base, page, per_page, search, sort, raw_where, None)
}

/// Build a list URL with optional cursor parameter for cursor-based pagination.
pub(crate) fn build_list_url_with_cursor(
    base: &str,
    page: i64,
    per_page: Option<i64>,
    search: Option<&str>,
    sort: Option<&str>,
    raw_where: &str,
    cursor: Option<(&str, &str)>,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Cursor pagination doesn't use page numbers — omit page param when a cursor is present
    if cursor.is_none() {
        parts.push(format!("page={page}"));
    }

    if let Some(pp) = per_page {
        parts.push(format!("per_page={pp}"));
    }

    if let Some(s) = search {
        parts.push(format!("search={}", url_encode(s)));
    }

    if let Some(s) = sort {
        parts.push(format!("sort={}", url_encode(s)));
    }

    if let Some((param, value)) = cursor {
        parts.push(format!("{}={}", param, url_encode(value)));
    }

    // Preserve where params and trash flag from original query string
    parts.extend(
        raw_where
            .split('&')
            .filter(|part| is_view_param(part))
            .map(str::to_string),
    );

    format!("{}?{}", base, parts.join("&"))
}

/// Simple percent-encoding for URL query values.
fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                format!("{}", b as char)
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_list_url tests ---

    #[test]
    fn build_list_url_basic() {
        let url = build_list_url("/admin/collections/posts", 2, None, None, None, "");
        assert_eq!(url, "/admin/collections/posts?page=2");
    }

    #[test]
    fn build_list_url_with_search_sort() {
        let url = build_list_url(
            "/admin/collections/posts",
            1,
            None,
            Some("hello"),
            Some("-title"),
            "",
        );
        assert!(url.contains("search=hello"));
        assert!(url.contains("sort=-title"));
    }

    #[test]
    fn build_list_url_preserves_where() {
        let url = build_list_url(
            "/admin/collections/posts",
            1,
            None,
            None,
            None,
            "where[title][equals]=foo&page=1",
        );
        assert!(url.contains("where[title][equals]=foo"));
        assert!(!url.contains("page=1&page=1")); // should not duplicate page
    }

    #[test]
    fn build_list_url_with_cursor_param() {
        let url = build_list_url_with_cursor(
            "/admin/collections/posts",
            1,
            None,
            None,
            Some("title"),
            "",
            Some(("after_cursor", "abc123")),
        );
        assert!(url.contains("after_cursor=abc123"));
        assert!(url.contains("sort=title"));
        assert!(
            !url.contains("page="),
            "Cursor URLs should not include page param"
        );
    }

    #[test]
    fn build_list_url_with_cursor_none() {
        let url =
            build_list_url_with_cursor("/admin/collections/posts", 2, None, None, None, "", None);
        assert_eq!(url, "/admin/collections/posts?page=2");
        assert!(!url.contains("cursor"));
    }

    fn ctx<'a>(sort: Option<&'a str>, where_params: &'a str) -> ListUrlContext<'a> {
        ListUrlContext {
            base_url: "/admin/collections/posts",
            search: Some("hello"),
            sort,
            per_page: Some(50),
            where_params,
        }
    }

    /// Regression: `per_page` was accepted by the list but dropped from every
    /// page, sort, and cursor link, so page 2 came back at the default size
    /// and overlapped page 1.
    #[test]
    fn every_list_link_carries_per_page() {
        let c = ctx(Some("title"), "");

        assert!(c.page_url(2).contains("per_page=50"));
        assert!(c.sort_url("-title").contains("per_page=50"));
        assert!(c.cursor_url("after_cursor", "x").contains("per_page=50"));
        assert!(c.clear_search_url().contains("per_page=50"));
    }

    /// Regression: the search form carried only the search term and sort —
    /// searching inside the trash left the trash, and any search dropped the
    /// active filters. Clearing the search dropped everything.
    #[test]
    fn search_keeps_filters_trash_sort_and_page_size() {
        let c = ctx(
            Some("-title"),
            "trash=1&where%5Btitle%5D%5Bcontains%5D=a%20b",
        );

        assert_eq!(
            c.search_form_params(),
            vec![
                ("sort".to_string(), "-title".to_string()),
                ("per_page".to_string(), "50".to_string()),
                ("trash".to_string(), "1".to_string()),
                ("where[title][contains]".to_string(), "a b".to_string()),
            ]
        );

        let clear = c.clear_search_url();
        assert!(!clear.contains("search="), "{clear}");
        assert!(clear.contains("sort=-title"), "{clear}");
        assert!(clear.contains("trash=1"), "{clear}");
        assert!(
            clear.contains("where%5Btitle%5D%5Bcontains%5D=a%20b"),
            "{clear}"
        );
    }

    // --- url_decode tests ---

    #[test]
    fn url_decode_basic() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("foo+bar"), "foo bar");
        assert_eq!(url_decode("plain"), "plain");
    }

    #[test]
    fn url_decode_multibyte_utf8() {
        assert_eq!(url_decode("%C3%A9"), "é");
    }

    #[test]
    fn url_decode_cjk() {
        assert_eq!(url_decode("%E4%B8%AD"), "中");
    }

    #[test]
    fn url_decode_emoji() {
        assert_eq!(url_decode("%F0%9F%98%80"), "😀");
    }

    #[test]
    fn url_decode_invalid_hex_preserves() {
        assert_eq!(url_decode("%ZZ"), "%ZZ");
    }

    #[test]
    fn url_decode_trailing_percent() {
        assert_eq!(url_decode("test%"), "test%");
    }

    #[test]
    fn url_decode_partial_hex() {
        assert_eq!(url_decode("test%2"), "test%2");
    }
}
