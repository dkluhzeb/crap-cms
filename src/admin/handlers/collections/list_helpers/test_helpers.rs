//! Shared fixtures for the list-helper tests.

use crate::{
    admin::handlers::shared::ListUrlContext,
    core::{
        FieldDefinition, FieldType, LocalizedString, SelectOption,
        collection::{AdminConfig, CollectionDefinition},
    },
};

/// A timestamped `posts` collection titled by `title`, with one field of each
/// commonly listed type.
pub(super) fn test_collection() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("status", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Draft".into()), "draft"),
                SelectOption::new(LocalizedString::Plain("Published".into()), "published"),
            ])
            .build(),
        FieldDefinition::builder("body", FieldType::Richtext).build(),
        FieldDefinition::builder("views", FieldType::Number).build(),
        FieldDefinition::builder("active", FieldType::Checkbox).build(),
        FieldDefinition::builder("date", FieldType::Date).build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..Default::default()
    };
    def
}

/// A list URL context for `posts` with the given sort and nothing else.
pub(super) fn test_url_ctx(sort: Option<&str>) -> ListUrlContext<'_> {
    ListUrlContext {
        base_url: "/admin/collections/posts",
        search: None,
        sort,
        per_page: None,
        where_params: "",
    }
}
