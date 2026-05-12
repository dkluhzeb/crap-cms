//! Typed admin URL builders.
//!
//! Replaces ad-hoc `format!("/admin/...")` strings throughout the admin
//! handlers. Helps with grep-ability and prevents subtle path drift between
//! call sites that reference the same route.

/// `/admin` — dashboard root.
pub const DASHBOARD: &str = "/admin";

/// `/admin/login` — login page.
pub const LOGIN: &str = "/admin/login";

/// `/admin/collections` — collection-list landing page.
pub const COLLECTIONS_ROOT: &str = "/admin/collections";

/// `/admin/login?success={key}` — login with a post-flow success message.
/// `key` is a translation key like `"success_email_verified"`.
pub fn login_with_success(key: &str) -> String {
    format!("{LOGIN}?success={key}")
}

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

/// `/admin/collections/{slug}/{id}/versions?page={page}` — paginated version list.
pub fn collection_item_versions_page(slug: &str, id: &str, page: u64) -> String {
    format!("/admin/collections/{slug}/{id}/versions?page={page}")
}

/// `/admin/collections/{slug}/{id}/versions/{version_id}/restore` — version restore endpoint.
pub fn collection_item_version_restore(slug: &str, id: &str, version_id: &str) -> String {
    format!("/admin/collections/{slug}/{id}/versions/{version_id}/restore")
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

/// `/admin/p/{slug}` — custom admin page route.
pub fn custom_page(slug: &str) -> String {
    format!("/admin/p/{slug}")
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
    fn custom_page_path() {
        assert_eq!(custom_page("system_info"), "/admin/p/system_info");
    }

    #[test]
    fn login_constants_and_success() {
        assert_eq!(DASHBOARD, "/admin");
        assert_eq!(LOGIN, "/admin/login");
        assert_eq!(COLLECTIONS_ROOT, "/admin/collections");
        assert_eq!(
            login_with_success("success_email_verified"),
            "/admin/login?success=success_email_verified"
        );
    }

    #[test]
    fn collection_version_paths() {
        assert_eq!(
            collection_item_versions_page("posts", "abc", 2),
            "/admin/collections/posts/abc/versions?page=2"
        );
        assert_eq!(
            collection_item_version_restore("posts", "abc", "v1"),
            "/admin/collections/posts/abc/versions/v1/restore"
        );
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
