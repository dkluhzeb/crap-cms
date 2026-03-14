//! Builder for admin template contexts.
//!
//! See [`ContextBuilder`] for details.

use axum::Extension;
use serde_json::{Map, Value, json};

use crate::{
    admin::AdminState,
    config::LocaleConfig,
    core::{
        Document,
        auth::{AuthUser, Claims},
        collection::{CollectionDefinition, GlobalDefinition},
    },
};

use crate::admin::context::{Breadcrumb, PageType, build_collection_context, build_global_context};

/// Centralized builder for admin template contexts.
pub struct ContextBuilder {
    pub(crate) data: Map<String, Value>,
}

impl ContextBuilder {
    /// Create a new builder with `crap`, `nav`, `user`, and locale pre-populated.
    pub fn new(state: &AdminState, claims: Option<&Claims>) -> Self {
        let mut data = Map::new();

        // crap metadata
        data.insert(
            "crap".into(),
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "build_hash": env!("BUILD_HASH"),
                "dev_mode": state.config.admin.dev_mode,
                "auth_enabled": has_auth_collections(state),
            }),
        );

        // nav
        data.insert(
            "nav".into(),
            json!({
                "collections": build_nav_collections(state),
                "globals": build_nav_globals(state),
            }),
        );

        // user
        if let Some(c) = claims {
            data.insert(
                "user".into(),
                json!({
                    "email": c.email,
                    "id": c.sub,
                    "collection": c.collection,
                }),
            );
        }

        // locale defaults
        data.insert(
            "_locale".into(),
            Value::String(state.config.locale.default_locale.clone()),
        );
        data.insert(
            "available_locales".into(),
            json!(state.translations.available_locales()),
        );

        Self { data }
    }

    /// Create a minimal builder for auth pages (no nav, no user).
    pub fn auth(state: &AdminState) -> Self {
        let mut data = Map::new();
        data.insert(
            "crap".into(),
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "build_hash": env!("BUILD_HASH"),
                "dev_mode": state.config.admin.dev_mode,
                "auth_enabled": true,
            }),
        );
        // locale defaults for auth pages
        data.insert(
            "_locale".into(),
            Value::String(state.config.locale.default_locale.clone()),
        );
        data.insert(
            "available_locales".into(),
            json!(state.translations.available_locales()),
        );
        Self { data }
    }

    /// Set the locale for this context (overrides the default).
    pub fn locale(mut self, locale: &str) -> Self {
        self.data
            .insert("_locale".into(), Value::String(locale.to_string()));
        self
    }

    /// Set the locale from an optional auth user (convenience for handlers).
    pub fn locale_from_auth(self, auth_user: &Option<Extension<AuthUser>>) -> Self {
        if let Some(Extension(au)) = auth_user {
            self.locale(&au.ui_locale)
        } else {
            self
        }
    }

    /// Set page metadata (type and title).
    pub fn page(mut self, page_type: PageType, title: impl Into<String>) -> Self {
        let title_str = title.into();
        // Top-level `title` for layout/base backward compat during transition
        self.data
            .insert("title".into(), Value::String(title_str.clone()));
        let page = self.data.entry("page").or_insert_with(|| json!({}));
        if let Some(obj) = page.as_object_mut() {
            obj.insert("title".into(), Value::String(title_str));
            obj.insert("type".into(), Value::String(page_type.as_str().to_string()));
        }
        self
    }

    /// Set breadcrumbs on the page object.
    pub fn breadcrumbs(mut self, crumbs: Vec<Breadcrumb>) -> Self {
        let crumbs_json: Vec<Value> = crumbs
            .into_iter()
            .map(|c| {
                let mut m = Map::new();
                m.insert("label".into(), Value::String(c.label));
                if let Some(url) = c.url {
                    m.insert("url".into(), Value::String(url));
                }
                if let Some(name) = c.label_name {
                    m.insert("label_name".into(), Value::String(name));
                }
                Value::Object(m)
            })
            .collect();
        // Set on page.breadcrumbs
        let page = self.data.entry("page").or_insert_with(|| json!({}));
        if let Some(obj) = page.as_object_mut() {
            obj.insert("breadcrumbs".into(), Value::Array(crumbs_json.clone()));
        }
        // Also top-level for backward compat with breadcrumb partial
        self.data
            .insert("breadcrumbs".into(), Value::Array(crumbs_json));
        self
    }

    /// Set the collection definition context.
    pub fn collection_def(mut self, def: &CollectionDefinition) -> Self {
        self.data
            .insert("collection".into(), build_collection_context(def));
        self
    }

    /// Set the global definition context.
    pub fn global_def(mut self, def: &GlobalDefinition) -> Self {
        self.data.insert("global".into(), build_global_context(def));
        self
    }

    /// Set a minimal document context (e.g., for error re-renders with just ID).
    pub fn document_stub(mut self, id: &str) -> Self {
        self.data.insert("document".into(), json!({ "id": id }));
        self
    }

    /// Set the document with explicit status (for edit pages before the document is fully loaded).
    pub fn document_with_status(mut self, doc: &Document, status: &str) -> Self {
        let mut doc_json = json!({
            "id": doc.id,
            "created_at": doc.created_at,
            "updated_at": doc.updated_at,
            "status": status,
        });
        doc_json["data"] = json!(doc.fields);
        self.data.insert("document".into(), doc_json);
        self
    }

    /// Set the items list (for collection list pages).
    pub fn items(mut self, items: Vec<Value>) -> Self {
        self.data.insert("items".into(), Value::Array(items));
        self
    }

    /// Set the processed fields array (for edit/create forms).
    pub fn fields(mut self, fields: Vec<Value>) -> Self {
        self.data.insert("fields".into(), Value::Array(fields));
        self
    }

    /// Set pagination data.
    pub fn pagination(
        mut self,
        page: i64,
        per_page: i64,
        total: i64,
        prev_url: String,
        next_url: String,
    ) -> Self {
        let total_pages = ((total as f64) / (per_page as f64)).ceil() as i64;
        self.data.insert(
            "pagination".into(),
            json!({
                "page": page,
                "per_page": per_page,
                "total": total,
                "total_pages": total_pages,
                "has_prev": page > 1,
                "has_next": page < total_pages,
                "prev_url": prev_url,
                "next_url": next_url,
            }),
        );
        // Backward compat: top-level pagination vars for templates
        self.data
            .insert("has_pagination".into(), json!(total_pages > 1));
        self.data.insert("page".into(), json!(page));
        self.data.insert("per_page".into(), json!(per_page));
        self.data.insert("total".into(), json!(total));
        self.data.insert("total_pages".into(), json!(total_pages));
        self.data.insert("has_prev".into(), json!(page > 1));
        self.data
            .insert("has_next".into(), json!(page < total_pages));
        self.data.insert("prev_url".into(), Value::String(prev_url));
        self.data.insert("next_url".into(), Value::String(next_url));
        self
    }

    /// Set interpolation param for page title translation.
    pub fn page_title_name(mut self, name: impl Into<String>) -> Self {
        let page = self.data.entry("page").or_insert_with(|| json!({}));
        if let Some(obj) = page.as_object_mut() {
            obj.insert("title_name".into(), Value::String(name.into()));
        }
        self
    }

    /// Set an arbitrary key-value pair.
    pub fn set(mut self, key: impl Into<String>, value: Value) -> Self {
        self.data.insert(key.into(), value);
        self
    }

    /// Set editor locale context (content locales from config, not UI translation locales).
    pub fn editor_locale(mut self, editor_locale: Option<&str>, config: &LocaleConfig) -> Self {
        if !config.is_enabled() {
            return self;
        }
        let current = editor_locale.unwrap_or(&config.default_locale);
        let locales: Vec<Value> = config
            .locales
            .iter()
            .map(|l| {
                json!({
                    "value": l,
                    "label": l.to_uppercase(),
                    "selected": l == current,
                })
            })
            .collect();
        self.data.insert("has_editor_locales".into(), json!(true));
        self.data
            .insert("editor_locale".into(), Value::String(current.to_string()));
        self.data.insert("editor_locales".into(), json!(locales));
        self
    }

    /// Merge all keys from a JSON object into the context (for locale data, etc.).
    pub fn merge(mut self, obj: Value) -> Self {
        if let Some(map) = obj.as_object() {
            for (k, v) in map {
                self.data.insert(k.clone(), v.clone());
            }
        }
        self
    }

    /// Build the final context value.
    pub fn build(self) -> Value {
        Value::Object(self.data)
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

fn build_nav_collections(state: &AdminState) -> Value {
    let mut collections: Vec<Value> = state
        .registry
        .collections
        .values()
        .map(|def| {
            json!({
                "slug": def.slug,
                "display_name": def.display_name(),
                "is_auth": def.is_auth_collection(),
                "is_upload": def.is_upload_collection(),
            })
        })
        .collect();
    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    Value::Array(collections)
}

fn build_nav_globals(state: &AdminState) -> Value {
    let mut globals: Vec<Value> = state
        .registry
        .globals
        .values()
        .map(|def| {
            json!({
                "slug": def.slug,
                "display_name": def.display_name(),
            })
        })
        .collect();
    globals.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    Value::Array(globals)
}

fn has_auth_collections(state: &AdminState) -> bool {
    state
        .registry
        .collections
        .values()
        .any(|def| def.is_auth_collection())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    use crate::{
        admin::context::Breadcrumb,
        config::LocaleConfig,
        core::{
            collection::{CollectionDefinition, GlobalDefinition, Labels},
            document::DocumentBuilder,
            field::{FieldAdmin, FieldDefinition, FieldType, LocalizedString},
        },
    };

    // --- ContextBuilder: editor_locale ---

    #[test]
    fn context_builder_editor_locale_sets_data() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        };
        let result = builder.editor_locale(Some("de"), &config).build();
        assert_eq!(result["has_editor_locales"], true);
        assert_eq!(result["editor_locale"], "de");
        let locales = result["editor_locales"].as_array().unwrap();
        assert_eq!(locales.len(), 2);
        assert_eq!(locales[0]["value"], "en");
        assert_eq!(locales[0]["selected"], false);
        assert_eq!(locales[1]["value"], "de");
        assert_eq!(locales[1]["selected"], true);
    }

    #[test]
    fn context_builder_editor_locale_disabled_noop() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let config = LocaleConfig::default(); // empty = disabled
        let result = builder.editor_locale(Some("de"), &config).build();
        assert!(result.get("has_editor_locales").is_none());
    }

    #[test]
    fn context_builder_editor_locale_defaults_to_default() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        };
        let result = builder.editor_locale(None, &config).build();
        assert_eq!(result["editor_locale"], "en");
    }

    // --- ContextBuilder: merge ---

    #[test]
    fn context_builder_merge_adds_keys() {
        let mut data = Map::new();
        data.insert("existing".into(), json!("value"));
        let mut builder = ContextBuilder { data };
        builder = builder.merge(json!({
            "new_key": "new_value",
            "another": 42,
        }));
        let result = builder.build();
        assert_eq!(result["existing"], "value");
        assert_eq!(result["new_key"], "new_value");
        assert_eq!(result["another"], 42);
    }

    #[test]
    fn context_builder_merge_non_object_is_noop() {
        let mut data = Map::new();
        data.insert("key".into(), json!("val"));
        let builder = ContextBuilder { data };
        let builder = builder.merge(json!("not an object"));
        let result = builder.build();
        assert_eq!(result["key"], "val");
    }

    // --- ContextBuilder: set ---

    #[test]
    fn context_builder_set_adds_value() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.set("foo", json!("bar"));
        let result = builder.build();
        assert_eq!(result["foo"], "bar");
    }

    // --- ContextBuilder: items ---

    #[test]
    fn context_builder_items_sets_array() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.items(vec![json!({"id": "1"}), json!({"id": "2"})]);
        let result = builder.build();
        let items = result["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    // --- ContextBuilder: fields ---

    #[test]
    fn context_builder_fields_sets_array() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.fields(vec![json!({"name": "title"})]);
        let result = builder.build();
        let fields = result["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0]["name"], "title");
    }

    // --- ContextBuilder: document_stub ---

    #[test]
    fn context_builder_document_stub_sets_id() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.document_stub("abc123");
        let result = builder.build();
        assert_eq!(result["document"]["id"], "abc123");
    }

    // --- ContextBuilder: pagination ---

    #[test]
    fn context_builder_pagination_computes_total_pages() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.pagination(2, 10, 25, "/prev".to_string(), "/next".to_string());
        let result = builder.build();
        assert_eq!(result["pagination"]["page"], 2);
        assert_eq!(result["pagination"]["per_page"], 10);
        assert_eq!(result["pagination"]["total"], 25);
        assert_eq!(result["pagination"]["total_pages"], 3);
        assert_eq!(result["pagination"]["has_prev"], true);
        assert_eq!(result["pagination"]["has_next"], true);
        assert_eq!(result["has_pagination"], true);
    }

    #[test]
    fn context_builder_pagination_first_page_no_prev() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.pagination(1, 10, 5, "/prev".to_string(), "/next".to_string());
        let result = builder.build();
        assert_eq!(result["pagination"]["has_prev"], false);
        assert_eq!(result["pagination"]["has_next"], false);
        assert_eq!(result["has_pagination"], false);
    }

    // --- ContextBuilder: page ---

    #[test]
    fn context_builder_page_sets_type_and_title() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.page(PageType::Dashboard, "My Dashboard");
        let result = builder.build();
        assert_eq!(result["title"], "My Dashboard");
        assert_eq!(result["page"]["title"], "My Dashboard");
        assert_eq!(result["page"]["type"], "dashboard");
    }

    // --- ContextBuilder: breadcrumbs ---

    #[test]
    fn context_builder_breadcrumbs_sets_both() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.breadcrumbs(vec![
            Breadcrumb::link("Home", "/admin"),
            Breadcrumb::current("Posts"),
        ]);
        let result = builder.build();
        let crumbs = result["breadcrumbs"].as_array().unwrap();
        assert_eq!(crumbs.len(), 2);
        assert_eq!(crumbs[0]["label"], "Home");
        assert_eq!(crumbs[0]["url"], "/admin");
        assert_eq!(crumbs[1]["label"], "Posts");
        assert!(crumbs[1].get("url").is_none() || crumbs[1]["url"].is_null());
        // Also check page.breadcrumbs
        let page_crumbs = result["page"]["breadcrumbs"].as_array().unwrap();
        assert_eq!(page_crumbs.len(), 2);
    }

    // --- ContextBuilder: document_with_status ---

    #[test]
    fn context_builder_document_with_status() {
        let doc = DocumentBuilder::new("doc1")
            .fields(HashMap::from([("title".to_string(), json!("Hello"))]))
            .created_at(Some("2026-01-01"))
            .updated_at(Some("2026-01-02"))
            .build();
        let data = Map::new();
        let builder = ContextBuilder { data };
        let builder = builder.document_with_status(&doc, "draft");
        let result = builder.build();
        assert_eq!(result["document"]["id"], "doc1");
        assert_eq!(result["document"]["status"], "draft");
        assert_eq!(result["document"]["created_at"], "2026-01-01");
        assert_eq!(result["document"]["updated_at"], "2026-01-02");
    }

    // --- ContextBuilder: collection_def ---

    #[test]
    fn context_builder_collection_def_sets_collection() {
        let mut def = CollectionDefinition::new("posts");
        def.labels = Labels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        };
        let data = Map::new();
        let builder = ContextBuilder { data };
        let result = builder.collection_def(&def).build();
        assert_eq!(result["collection"]["slug"], "posts");
    }

    // --- ContextBuilder: global_def ---

    #[test]
    fn context_builder_global_def_sets_global() {
        let mut def = GlobalDefinition::new("settings");
        def.labels = Labels {
            singular: Some(LocalizedString::Plain("Settings".to_string())),
            plural: None,
        };
        let data = Map::new();
        let builder = ContextBuilder { data };
        let result = builder.global_def(&def).build();
        assert_eq!(result["global"]["slug"], "settings");
    }

    // --- ContextBuilder: locale ---

    #[test]
    fn context_builder_locale_overrides() {
        let mut data = Map::new();
        data.insert("_locale".into(), json!("en"));
        let builder = ContextBuilder { data };
        let result = builder.locale("fr").build();
        assert_eq!(result["_locale"], "fr");
    }

    // --- ContextBuilder: document_stub with collection_def ---

    #[test]
    fn context_builder_document_stub_sets_id_only() {
        let data = Map::new();
        let builder = ContextBuilder { data };
        let result = builder.document_stub("xyz999").build();
        assert_eq!(result["document"]["id"], "xyz999");
    }

    // --- build_fields_meta coverage via collection_def ---

    #[test]
    fn context_builder_collection_def_with_fields() {
        let mut def = CollectionDefinition::new("articles");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .unique(true)
                .localized(true)
                .admin(
                    FieldAdmin::builder()
                        .label(LocalizedString::Plain("Title".to_string()))
                        .hidden(false)
                        .readonly(true)
                        .width("50%")
                        .description(LocalizedString::Plain("The title field".to_string()))
                        .placeholder(LocalizedString::Plain("Enter title".to_string()))
                        .build(),
                )
                .build(),
        ];
        let data = Map::new();
        let builder = ContextBuilder { data };
        let result = builder.collection_def(&def).build();
        let meta = result["collection"]["fields_meta"].as_array().unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0]["name"], "title");
        assert_eq!(meta[0]["required"], true);
    }
}
