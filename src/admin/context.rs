//! Centralized template context builder for all admin handlers.
//!
//! Every admin page receives a structured context built through `ContextBuilder`.
//! This replaces the ad-hoc `serde_json::json!()` calls in individual handlers.

use serde_json::{json, Map, Value};

use crate::admin::AdminState;
use crate::core::auth::Claims;
use crate::core::collection::{CollectionDefinition, GlobalDefinition};
use crate::core::field::FieldDefinition;

/// Page type identifiers for template conditional logic.
pub enum PageType {
    Dashboard,
    CollectionList,
    CollectionItems,
    CollectionEdit,
    CollectionCreate,
    CollectionDelete,
    CollectionVersions,
    GlobalEdit,
    AuthLogin,
    AuthForgot,
    AuthReset,
    Error403,
    Error404,
    Error500,
}

impl PageType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PageType::Dashboard => "dashboard",
            PageType::CollectionList => "collection_list",
            PageType::CollectionItems => "collection_items",
            PageType::CollectionEdit => "collection_edit",
            PageType::CollectionCreate => "collection_create",
            PageType::CollectionDelete => "collection_delete",
            PageType::CollectionVersions => "collection_versions",
            PageType::GlobalEdit => "global_edit",
            PageType::AuthLogin => "auth_login",
            PageType::AuthForgot => "auth_forgot",
            PageType::AuthReset => "auth_reset",
            PageType::Error403 => "error_403",
            PageType::Error404 => "error_404",
            PageType::Error500 => "error_500",
        }
    }
}

/// A breadcrumb entry with a label and optional URL.
pub struct Breadcrumb {
    pub label: String,
    pub url: Option<String>,
}

impl Breadcrumb {
    pub fn link(label: impl Into<String>, url: impl Into<String>) -> Self {
        Self { label: label.into(), url: Some(url.into()) }
    }
    pub fn current(label: impl Into<String>) -> Self {
        Self { label: label.into(), url: None }
    }
}

/// Centralized builder for admin template contexts.
pub struct ContextBuilder {
    data: Map<String, Value>,
}

impl ContextBuilder {
    /// Create a new builder with `crap`, `nav`, and `user` pre-populated.
    pub fn new(state: &AdminState, claims: Option<&Claims>) -> Self {
        let mut data = Map::new();

        // crap metadata
        data.insert("crap".into(), json!({
            "version": env!("CARGO_PKG_VERSION"),
            "dev_mode": state.config.admin.dev_mode,
            "auth_enabled": has_auth_collections(state),
        }));

        // nav
        data.insert("nav".into(), json!({
            "collections": build_nav_collections(state),
            "globals": build_nav_globals(state),
        }));

        // user
        if let Some(c) = claims {
            data.insert("user".into(), json!({
                "email": c.email,
                "id": c.sub,
                "collection": c.collection,
            }));
        }

        Self { data }
    }

    /// Create a minimal builder for auth pages (no nav, no user).
    pub fn auth(state: &AdminState) -> Self {
        let mut data = Map::new();
        data.insert("crap".into(), json!({
            "version": env!("CARGO_PKG_VERSION"),
            "dev_mode": state.config.admin.dev_mode,
            "auth_enabled": true,
        }));
        Self { data }
    }

    /// Set page metadata (type and title).
    pub fn page(mut self, page_type: PageType, title: impl Into<String>) -> Self {
        let title_str = title.into();
        // Top-level `title` for layout/base backward compat during transition
        self.data.insert("title".into(), Value::String(title_str.clone()));
        let page = self.data.entry("page").or_insert_with(|| json!({}));
        if let Some(obj) = page.as_object_mut() {
            obj.insert("title".into(), Value::String(title_str));
            obj.insert("type".into(), Value::String(page_type.as_str().to_string()));
        }
        self
    }

    /// Set breadcrumbs on the page object.
    pub fn breadcrumbs(mut self, crumbs: Vec<Breadcrumb>) -> Self {
        let crumbs_json: Vec<Value> = crumbs.into_iter().map(|c| {
            let mut m = serde_json::Map::new();
            m.insert("label".into(), Value::String(c.label));
            if let Some(url) = c.url {
                m.insert("url".into(), Value::String(url));
            }
            Value::Object(m)
        }).collect();
        // Set on page.breadcrumbs
        let page = self.data.entry("page").or_insert_with(|| json!({}));
        if let Some(obj) = page.as_object_mut() {
            obj.insert("breadcrumbs".into(), Value::Array(crumbs_json.clone()));
        }
        // Also top-level for backward compat with breadcrumb partial
        self.data.insert("breadcrumbs".into(), Value::Array(crumbs_json));
        self
    }

    /// Set the collection definition context.
    pub fn collection_def(mut self, def: &CollectionDefinition) -> Self {
        self.data.insert("collection".into(), build_collection_context(def));
        self
    }

    /// Set the global definition context.
    pub fn global_def(mut self, def: &GlobalDefinition) -> Self {
        self.data.insert("global".into(), build_global_context(def));
        self
    }

    /// Set the document context (for edit pages).
    #[allow(dead_code)]
    pub fn document(mut self, doc: &crate::core::Document) -> Self {
        let mut doc_json = json!({
            "id": doc.id,
            "created_at": doc.created_at,
            "updated_at": doc.updated_at,
        });
        // Add status if present
        if let Some(status) = doc.fields.get("_status").and_then(|v| v.as_str()) {
            doc_json["status"] = json!(status);
        }
        // Add raw data
        doc_json["data"] = json!(doc.fields);
        self.data.insert("document".into(), doc_json);
        self
    }

    /// Set a minimal document context (e.g., for error re-renders with just ID).
    pub fn document_stub(mut self, id: &str) -> Self {
        self.data.insert("document".into(), json!({ "id": id }));
        self
    }

    /// Set the document with explicit status (for edit pages before the document is fully loaded).
    pub fn document_with_status(mut self, doc: &crate::core::Document, status: &str) -> Self {
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
        self.data.insert("pagination".into(), json!({
            "page": page,
            "per_page": per_page,
            "total": total,
            "total_pages": total_pages,
            "has_prev": page > 1,
            "has_next": page < total_pages,
            "prev_url": prev_url,
            "next_url": next_url,
        }));
        // Backward compat: top-level pagination vars for templates
        self.data.insert("has_pagination".into(), json!(total_pages > 1));
        self.data.insert("page".into(), json!(page));
        self.data.insert("per_page".into(), json!(per_page));
        self.data.insert("total".into(), json!(total));
        self.data.insert("total_pages".into(), json!(total_pages));
        self.data.insert("has_prev".into(), json!(page > 1));
        self.data.insert("has_next".into(), json!(page < total_pages));
        self.data.insert("prev_url".into(), Value::String(prev_url));
        self.data.insert("next_url".into(), Value::String(next_url));
        self
    }

    /// Set an arbitrary key-value pair.
    pub fn set(mut self, key: impl Into<String>, value: Value) -> Self {
        self.data.insert(key.into(), value);
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

/// Build the enriched collection context from a definition.
pub fn build_collection_context(def: &CollectionDefinition) -> Value {
    json!({
        "slug": def.slug,
        "display_name": def.display_name(),
        "singular_name": def.singular_name(),
        "title_field": def.title_field(),
        "timestamps": def.timestamps,
        "is_auth": def.is_auth_collection(),
        "is_upload": def.is_upload_collection(),
        "has_drafts": def.has_drafts(),
        "has_versions": def.has_versions(),
        "admin": {
            "use_as_title": def.admin.use_as_title,
            "default_sort": def.admin.default_sort,
            "hidden": def.admin.hidden,
            "list_searchable_fields": def.admin.list_searchable_fields,
        },
        "upload": def.upload.as_ref().map(|u| json!({
            "enabled": u.enabled,
            "mime_types": u.mime_types,
            "max_file_size": u.max_file_size,
            "admin_thumbnail": u.admin_thumbnail,
        })),
        "versions": def.versions.as_ref().map(|v| json!({
            "drafts": v.drafts,
            "max_versions": v.max_versions,
        })),
        "auth": def.auth.as_ref().map(|a| json!({
            "enabled": a.enabled,
            "disable_local": a.disable_local,
            "verify_email": a.verify_email,
        })),
        "fields_meta": build_fields_meta(&def.fields),
    })
}

/// Build the enriched global context from a definition.
pub fn build_global_context(def: &GlobalDefinition) -> Value {
    json!({
        "slug": def.slug,
        "display_name": def.display_name(),
        "fields_meta": build_fields_meta(&def.fields),
    })
}

/// Build field metadata array for template conditional logic.
pub fn build_fields_meta(fields: &[FieldDefinition]) -> Value {
    let meta: Vec<Value> = fields.iter().map(|f| {
        json!({
            "name": f.name,
            "field_type": f.field_type.as_str(),
            "required": f.required,
            "unique": f.unique,
            "localized": f.localized,
            "admin": {
                "label": f.admin.label.as_ref().map(|ls| ls.resolve_default()),
                "hidden": f.admin.hidden,
                "readonly": f.admin.readonly,
                "width": f.admin.width,
                "description": f.admin.description.as_ref().map(|ls| ls.resolve_default()),
                "placeholder": f.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
            },
        })
    }).collect();
    Value::Array(meta)
}

// ── Internal helpers ──────────────────────────────────────────────────────

fn build_nav_collections(state: &AdminState) -> Value {
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Registry lock poisoned: {}", e);
            return json!([]);
        }
    };
    let mut collections: Vec<Value> = reg.collections.values()
        .map(|def| json!({
            "slug": def.slug,
            "display_name": def.display_name(),
            "is_auth": def.is_auth_collection(),
            "is_upload": def.is_upload_collection(),
        }))
        .collect();
    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    Value::Array(collections)
}

fn build_nav_globals(state: &AdminState) -> Value {
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Registry lock poisoned: {}", e);
            return json!([]);
        }
    };
    let mut globals: Vec<Value> = reg.globals.values()
        .map(|def| json!({
            "slug": def.slug,
            "display_name": def.display_name(),
        }))
        .collect();
    globals.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    Value::Array(globals)
}

fn has_auth_collections(state: &AdminState) -> bool {
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(_) => return false,
    };
    reg.collections.values().any(|def| def.is_auth_collection())
}
