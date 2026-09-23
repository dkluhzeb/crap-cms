//! Which fields the list view may offer the viewer.

use std::collections::HashSet;

use crate::{admin::handlers::shared::auto_label_from_name, core::FieldDefinition};

/// The fields the list view may offer the viewer as a column, a sort, or a
/// filter: never a `hidden` field, and never one the read refuses to sort or
/// filter on for this viewer (the service's
/// [`unreadable_query_paths`](crate::service::unreadable_query_paths)).
/// Offering one used to turn a click on it into a whole-collection 403.
#[derive(Debug, Default)]
pub(in crate::admin::handlers::collections) struct ListFieldAccess {
    unreadable: HashSet<String>,
}

impl ListFieldAccess {
    /// Access that withholds the `unreadable` top-level field names (and every
    /// `hidden` field).
    pub fn new(unreadable: HashSet<String>) -> Self {
        Self { unreadable }
    }

    /// Whether `field` may be offered as a column, sort, or filter.
    pub fn offers(&self, field: &FieldDefinition) -> bool {
        !field.hidden && !self.unreadable.contains(&field.name)
    }
}

/// Get the display label for a field (admin label or auto-generated from name).
pub(in crate::admin::handlers::collections) fn field_label(field: &FieldDefinition) -> String {
    match &field.admin.label {
        Some(label) => label.resolve_current().to_string(),
        None => auto_label_from_name(&field.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAdmin, FieldType, LocalizedString};

    #[test]
    fn field_label_uses_admin_label() {
        let f = FieldDefinition::builder("my_field", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Custom Label".into()))
                    .build(),
            )
            .build();
        assert_eq!(field_label(&f), "Custom Label");
    }

    #[test]
    fn field_label_falls_back_to_name() {
        let f = FieldDefinition::builder("my_field", FieldType::Text).build();
        assert_eq!(field_label(&f), "My Field");
    }

    #[test]
    fn hidden_and_unreadable_fields_are_not_offered() {
        let access = ListFieldAccess::new(HashSet::from(["secret".to_string()]));

        let plain = FieldDefinition::builder("title", FieldType::Text).build();
        let hidden = FieldDefinition::builder("internal", FieldType::Text)
            .hidden(true)
            .build();
        let denied = FieldDefinition::builder("secret", FieldType::Text).build();

        assert!(access.offers(&plain));
        assert!(!access.offers(&hidden));
        assert!(!access.offers(&denied));
    }
}
