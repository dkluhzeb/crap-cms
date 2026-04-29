//! Collection definition context — `{{collection.*}}` for collection-scoped pages.

use serde::Serialize;

use crate::{
    admin::context::FieldMeta,
    core::{
        CollectionDefinition,
        collection::{Auth, VersionsConfig},
        upload::CollectionUpload,
    },
};

/// Top-level collection metadata exposed to templates.
#[derive(Serialize)]
pub struct CollectionContext {
    pub slug: String,
    pub display_name: String,
    pub singular_name: String,
    pub title_field: Option<String>,
    pub timestamps: bool,
    pub is_auth: bool,
    pub is_upload: bool,
    pub has_drafts: bool,
    pub has_versions: bool,
    pub soft_delete: bool,
    pub can_permanently_delete: bool,
    pub admin: AdminMeta,
    pub upload: Option<UploadMeta>,
    pub versions: Option<VersionsMeta>,
    pub auth: Option<AuthMeta>,
    pub fields_meta: Vec<FieldMeta>,
}

/// Admin-presentation metadata pulled from `def.admin`.
#[derive(Serialize)]
pub struct AdminMeta {
    pub use_as_title: Option<String>,
    pub default_sort: Option<String>,
    pub hidden: bool,
    pub list_searchable_fields: Vec<String>,
}

/// Upload-collection metadata. Only present when `def.upload` is set.
#[derive(Serialize)]
pub struct UploadMeta {
    pub enabled: bool,
    pub mime_types: Vec<String>,
    pub max_file_size: Option<u64>,
    pub admin_thumbnail: Option<String>,
}

/// Versioning metadata. Only present when `def.versions` is set.
#[derive(Serialize)]
pub struct VersionsMeta {
    pub drafts: bool,
    pub max_versions: u32,
}

/// Auth-collection metadata. Only present when `def.auth` is set.
#[derive(Serialize)]
pub struct AuthMeta {
    pub enabled: bool,
    pub disable_local: bool,
    pub verify_email: bool,
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
            is_auth: def.is_auth_collection(),
            is_upload: def.is_upload_collection(),
            has_drafts: def.has_drafts(),
            has_versions: def.has_versions(),
            soft_delete: def.soft_delete,
            can_permanently_delete: def.access.delete.is_some(),
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
        assert_eq!(v["is_auth"], false);
        assert_eq!(v["is_upload"], false);
        assert_eq!(v["has_drafts"], false);
        assert_eq!(v["has_versions"], false);
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
    fn from_def_can_permanently_delete_true() {
        let mut def = CollectionDefinition::new("pages");
        def.access.delete = Some("access.admin_only".to_string());
        let v = serde_json::to_value(CollectionContext::from_def(&def)).unwrap();
        assert_eq!(v["can_permanently_delete"], true);
    }

    #[test]
    fn from_def_can_permanently_delete_false() {
        let def = CollectionDefinition::new("pages");
        let v = serde_json::to_value(CollectionContext::from_def(&def)).unwrap();
        assert_eq!(v["can_permanently_delete"], false);
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
