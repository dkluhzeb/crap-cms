//! Typed admin URL builders.
//!
//! Replaces ad-hoc `format!("/admin/...")` strings throughout the admin
//! handlers. Helps with grep-ability and prevents subtle path drift between
//! call sites that reference the same route.

/// `/admin/collections/{slug}` — collection list.
pub fn collection(slug: &str) -> String {
    format!("/admin/collections/{slug}")
}

/// `/admin/collections/{slug}?trash=1` — trash view of the collection list.
pub fn collection_trash(slug: &str) -> String {
    format!("/admin/collections/{slug}?trash=1")
}

/// `/admin/collections/{slug}/create` — new-document form.
pub fn collection_create(slug: &str) -> String {
    format!("/admin/collections/{slug}/create")
}

/// `/admin/collections/{slug}/{id}` — edit form for a specific document.
pub fn collection_item(slug: &str, id: &str) -> String {
    format!("/admin/collections/{slug}/{id}")
}

/// `/admin/collections/{slug}/{id}/versions` — version list for a document.
pub fn collection_item_versions(slug: &str, id: &str) -> String {
    format!("/admin/collections/{slug}/{id}/versions")
}

/// `/admin/globals/{slug}` — edit form for a global.
pub fn global(slug: &str) -> String {
    format!("/admin/globals/{slug}")
}

/// `/admin/globals/{slug}/versions` — version list for a global.
pub fn global_versions(slug: &str) -> String {
    format!("/admin/globals/{slug}/versions")
}

/// `/admin/globals/{slug}/versions?page={page}` — paginated version list.
///
/// `page` is `u64` — page numbers are non-negative. Callers using `i64`
/// must cast (`as u64`) to make the sign assumption explicit.
pub fn global_versions_page(slug: &str, page: u64) -> String {
    format!("/admin/globals/{slug}/versions?page={page}")
}

/// `/admin/globals/{slug}/versions/{version_id}/restore` — version restore endpoint.
pub fn global_version_restore(slug: &str, version_id: &str) -> String {
    format!("/admin/globals/{slug}/versions/{version_id}/restore")
}

/// `/admin/mfa?collection={slug}` — MFA challenge with the auth collection slug.
pub fn mfa_with_collection(slug: &str) -> String {
    format!("/admin/mfa?collection={slug}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_paths_format_correctly() {
        assert_eq!(collection("posts"), "/admin/collections/posts");
        assert_eq!(
            collection_trash("posts"),
            "/admin/collections/posts?trash=1"
        );
        assert_eq!(
            collection_create("posts"),
            "/admin/collections/posts/create"
        );
        assert_eq!(
            collection_item("posts", "abc"),
            "/admin/collections/posts/abc"
        );
        assert_eq!(
            collection_item_versions("posts", "abc"),
            "/admin/collections/posts/abc/versions"
        );
    }

    #[test]
    fn global_paths_format_correctly() {
        assert_eq!(global("settings"), "/admin/globals/settings");
        assert_eq!(
            global_versions("settings"),
            "/admin/globals/settings/versions"
        );
        assert_eq!(
            global_versions_page("settings", 3),
            "/admin/globals/settings/versions?page=3"
        );
        assert_eq!(
            global_version_restore("settings", "v123"),
            "/admin/globals/settings/versions/v123/restore"
        );
    }

    #[test]
    fn mfa_path_carries_collection() {
        assert_eq!(mfa_with_collection("users"), "/admin/mfa?collection=users");
    }

    #[test]
    fn helpers_accept_string_borrows() {
        let slug = String::from("posts");
        let id = String::from("abc");
        // `&String` auto-derefs to `&str`.
        assert_eq!(collection(&slug), "/admin/collections/posts");
        assert_eq!(collection_item(&slug, &id), "/admin/collections/posts/abc");
    }
}
