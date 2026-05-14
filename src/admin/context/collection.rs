//! Collection definition context — `{{collection.*}}` for collection-scoped pages.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    admin::context::FieldMeta,
    core::{CollectionDefinition, VersionsConfig, collection::Auth, upload::CollectionUpload},
    typegen::LuaAnnotation,
};

/// Top-level collection metadata exposed to templates.
///
/// Capability bits (`is_auth`, `is_upload`, `has_versions`, `has_drafts`)
/// are intentionally NOT separate fields — templates derive them from the
/// presence of the corresponding `Option<*Meta>` sub-struct (and the
/// `drafts` flag inside `versions`). Single source of truth: if `auth`
/// is `None`, the collection isn't auth-enabled; if `versions.drafts` is
/// false, drafts aren't on.
#[derive(Serialize, JsonSchema)]
pub struct CollectionContext {
    pub slug: String,
    pub display_name: String,
    pub singular_name: String,
    pub title_field: Option<String>,
    pub timestamps: bool,
    pub soft_delete: bool,
    pub admin: AdminMeta,
    pub upload: Option<UploadMeta>,
    pub versions: Option<VersionsMeta>,
    pub auth: Option<AuthMeta>,
    pub fields_meta: Vec<FieldMeta>,
}

/// Admin-presentation metadata pulled from `def.admin`.
#[derive(Serialize, JsonSchema)]
pub struct AdminMeta {
    pub use_as_title: Option<String>,
    pub default_sort: Option<String>,
    pub hidden: bool,
    pub list_searchable_fields: Vec<String>,
}

/// Upload-collection metadata. Only present when `def.upload` is set.
#[derive(Serialize, JsonSchema)]
pub struct UploadMeta {
    pub enabled: bool,
    pub mime_types: Vec<String>,
    pub max_file_size: Option<u64>,
    pub admin_thumbnail: Option<String>,
}

/// Versioning metadata. Only present when `def.versions` is set.
#[derive(Serialize, JsonSchema)]
pub struct VersionsMeta {
    pub drafts: bool,
    pub max_versions: u32,
}

/// Auth-collection metadata. Only present when `def.auth` is set.
#[derive(Serialize, JsonSchema)]
pub struct AuthMeta {
    pub enabled: bool,
    pub disable_local: bool,
    pub verify_email: bool,
}

impl LuaAnnotation for AdminMeta {
    fn render_lua_annotation(out: &mut String) {
        out.push_str("---@class crap.template.admin_meta\n");
        out.push_str("---@field use_as_title? string\n");
        out.push_str("---@field default_sort? string\n");
        out.push_str("---@field hidden boolean\n");
        out.push_str("---@field list_searchable_fields string[]\n");
        out.push('\n');
    }
}

impl LuaAnnotation for UploadMeta {
    fn render_lua_annotation(out: &mut String) {
        out.push_str("---@class crap.template.upload_meta\n");
        out.push_str("---@field enabled boolean\n");
        out.push_str("---@field mime_types string[]\n");
        out.push_str("---@field max_file_size? integer\n");
        out.push_str("---@field admin_thumbnail? string\n");
        out.push('\n');
    }
}

impl LuaAnnotation for VersionsMeta {
    fn render_lua_annotation(out: &mut String) {
        out.push_str("---@class crap.template.versions_meta\n");
        out.push_str("---@field drafts boolean\n");
        out.push_str("---@field max_versions integer\n");
        out.push('\n');
    }
}

impl LuaAnnotation for AuthMeta {
    fn render_lua_annotation(out: &mut String) {
        out.push_str("---@class crap.template.auth_meta\n");
        out.push_str("---@field enabled boolean\n");
        out.push_str("---@field disable_local boolean\n");
        out.push_str("---@field verify_email boolean\n");
        out.push('\n');
    }
}

impl LuaAnnotation for CollectionContext {
    fn render_lua_annotation(out: &mut String) {
        out.push_str("---@class crap.template.collection\n");
        out.push_str("---@field slug string\n");
        out.push_str("---@field display_name string\n");
        out.push_str("---@field singular_name string\n");
        out.push_str("---@field title_field? string\n");
        out.push_str("---@field timestamps boolean\n");
        out.push_str("---@field soft_delete boolean\n");
        out.push_str("---@field admin crap.template.admin_meta\n");
        out.push_str("---@field upload? crap.template.upload_meta\n");
        out.push_str("---@field versions? crap.template.versions_meta\n");
        out.push_str("---@field auth? crap.template.auth_meta\n");
        out.push_str("---@field fields_meta crap.template.field_meta[]\n");
        out.push('\n');
    }
}

impl CollectionContext {
    /// Build the typed context from a [`CollectionDefinition`].
    pub fn from_def(def: &CollectionDefinition) -> Self {
        Self {
            slug: def.slug.to_string(),
            display_name: def.display_name().to_string(),
            singular_name: def.singular_name().to_string(),
            title_field: def.title_field().map(str::to_string),
            timestamps: def.timestamps,
            soft_delete: def.soft_delete,
            admin: AdminMeta::from_def(def),
            upload: def.upload.as_ref().map(UploadMeta::from_def),
            versions: def.versions.as_ref().map(VersionsMeta::from_def),
            auth: def.auth.as_ref().map(AuthMeta::from_def),
            fields_meta: FieldMeta::from_defs(&def.fields),
        }
    }
}

impl AdminMeta {
    fn from_def(def: &CollectionDefinition) -> Self {
        Self {
            use_as_title: def.admin.use_as_title.clone(),
            default_sort: def.admin.default_sort.clone(),
            hidden: def.admin.hidden,
            list_searchable_fields: def.admin.list_searchable_fields.clone(),
        }
    }
}

impl UploadMeta {
    fn from_def(u: &CollectionUpload) -> Self {
        Self {
            enabled: u.enabled,
            mime_types: u.mime_types.clone(),
            max_file_size: u.max_file_size,
            admin_thumbnail: u.admin_thumbnail.clone(),
        }
    }
}

impl VersionsMeta {
    pub(crate) fn from_def(v: &VersionsConfig) -> Self {
        Self {
            drafts: v.drafts,
            max_versions: v.max_versions,
        }
    }
}

impl AuthMeta {
    fn from_def(a: &Auth) -> Self {
        Self {
            enabled: a.enabled,
            disable_local: a.disable_local,
            verify_email: a.verify_email,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::{
        collection::Labels,
        field::{FieldDefinition, FieldType, LocalizedString},
    };

    #[test]
    fn from_def_includes_all_fields() {
        let mut def = CollectionDefinition::new("posts");
        def.labels = Labels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        };
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
        ];
        let ctx = CollectionContext::from_def(&def);
        let v = serde_json::to_value(&ctx).unwrap();
        assert_eq!(v["slug"], "posts");
        assert_eq!(v["display_name"], "Posts");
        assert_eq!(v["singular_name"], "Post");
        assert_eq!(v["timestamps"], true);
        assert!(v["auth"].is_null());
        assert!(v["upload"].is_null());
        assert!(v["versions"].is_null());
        assert_eq!(v["soft_delete"], false);
        let meta = v["fields_meta"].as_array().unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0]["name"], "title");
    }

    #[test]
    fn from_def_soft_delete_enabled() {
        let mut def = CollectionDefinition::new("pages");
        def.soft_delete = true;
        let v = serde_json::to_value(CollectionContext::from_def(&def)).unwrap();
        assert_eq!(v["soft_delete"], true);
    }

    #[test]
    fn from_def_no_optional_blocks_serialize_as_null() {
        let def = CollectionDefinition::new("pages");
        let v = serde_json::to_value(CollectionContext::from_def(&def)).unwrap();
        assert!(v["upload"].is_null());
        assert!(v["versions"].is_null());
        assert!(v["auth"].is_null());
    }
}
