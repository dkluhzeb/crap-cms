//! Global definition context — `{{global.*}}` for global-scoped pages.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    admin::context::{FieldMeta, VersionsMeta},
    core::collection::GlobalDefinition,
};

/// Top-level global metadata exposed to templates.
#[derive(Serialize, JsonSchema)]
pub struct GlobalContext {
    pub slug: String,
    pub display_name: String,
    pub has_drafts: bool,
    pub has_versions: bool,
    pub versions: Option<VersionsMeta>,
    pub fields_meta: Vec<FieldMeta>,
}

impl GlobalContext {
    /// Build the typed context from a [`GlobalDefinition`].
    pub fn from_def(def: &GlobalDefinition) -> Self {
        Self {
            slug: def.slug.to_string(),
            display_name: def.display_name().to_string(),
            has_drafts: def.has_drafts(),
            has_versions: def.has_versions(),
            versions: def.versions.as_ref().map(VersionsMeta::from_def),
            fields_meta: FieldMeta::from_defs(&def.fields),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::{
        collection::{GlobalDefinition, Labels},
        field::{FieldDefinition, FieldType, LocalizedString},
    };

    #[test]
    fn from_def_includes_all_fields() {
        let mut def = GlobalDefinition::new("settings");
        def.labels = Labels {
            singular: Some(LocalizedString::Plain("Settings".to_string())),
            plural: None,
        };
        def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
        let v = serde_json::to_value(GlobalContext::from_def(&def)).unwrap();
        assert_eq!(v["slug"], "settings");
        assert_eq!(v["display_name"], "Settings");
        assert_eq!(v["has_drafts"], false);
        assert_eq!(v["has_versions"], false);
        let meta = v["fields_meta"].as_array().unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0]["name"], "site_name");
    }

    #[test]
    fn from_def_no_versions_serializes_as_null() {
        let def = GlobalDefinition::new("settings");
        let v = serde_json::to_value(GlobalContext::from_def(&def)).unwrap();
        assert!(v["versions"].is_null());
    }
}
