//! Single-field rendering: `write_field` (emits a single
//! `---@field name? type` line) and `field_to_lua_type` (maps a
//! [`FieldDefinition`] to its `LuaLS` type string).

use crate::core::{FieldDefinition, FieldType};

use super::super::helpers::{is_optional, rel_has_many, to_pascal_case, w};

/// Write a single field's type definition.
pub(super) fn write_field(out: &mut String, field: &FieldDefinition, parent_pascal: &str) {
    // Row is layout-only — promote sub-fields to parent level (no prefix)
    if field.field_type == FieldType::Row {
        for sub in &field.fields {
            write_field(out, sub, parent_pascal);
        }
        return;
    }
    // Collapsible is layout-only — promote sub-fields to parent level (no prefix)
    if field.field_type == FieldType::Collapsible {
        for sub in &field.fields {
            write_field(out, sub, parent_pascal);
        }
        return;
    }
    // Tabs is layout-only — promote sub-fields to parent level (no prefix)
    if field.field_type == FieldType::Tabs {
        for tab in &field.tabs {
            for sub in &tab.fields {
                write_field(out, sub, parent_pascal);
            }
        }
        return;
    }
    // Emit a comment for polymorphic relationships listing target collections
    if field.field_type == FieldType::Relationship
        && let Some(rc) = &field.relationship
        && rc.is_polymorphic()
    {
        let targets = rc.all_collections().join(", ");
        w!(out, "--- Polymorphic relationship — targets: {}", targets);
    }
    let lua_type = field_to_lua_type(field, parent_pascal);
    let opt = if is_optional(field) { "?" } else { "" };
    w!(out, "---@field {}{opt} {lua_type}", field.name);
}

/// Map a field definition to its Lua type string.
fn field_to_lua_type(field: &FieldDefinition, parent_pascal: &str) -> String {
    match &field.field_type {
        FieldType::Text => {
            if field.has_many {
                "string[]".to_string()
            } else {
                "string".to_string()
            }
        }
        FieldType::Textarea
        | FieldType::Email
        | FieldType::Date
        | FieldType::Richtext
        | FieldType::Code => "string".to_string(),
        FieldType::Number => {
            if field.has_many {
                "number[]".to_string()
            } else {
                "number".to_string()
            }
        }
        FieldType::Checkbox => "boolean".to_string(),
        FieldType::Json => "any".to_string(),
        FieldType::Select | FieldType::Radio => {
            let base = if field.options.is_empty() {
                "string".to_string()
            } else {
                field
                    .options
                    .iter()
                    .map(|o| format!("\"{}\"", o.value))
                    .collect::<Vec<_>>()
                    .join(" | ")
            };

            if field.has_many {
                if field.options.is_empty() {
                    "string[]".to_string()
                } else {
                    format!("({base}|string)[]")
                }
            } else {
                base
            }
        }
        FieldType::Relationship => match &field.relationship {
            Some(rc) if rc.is_polymorphic() && rc.has_many => "string[]".to_string(),
            Some(rc) if rc.is_polymorphic() => "string".to_string(),
            Some(rc) if rc.has_many => "string[]".to_string(),
            _ => "string".to_string(),
        },
        FieldType::Array => {
            let sub = format!("{}{}", parent_pascal, to_pascal_case(&field.name));
            format!("crap.array_row.{sub}[]")
        }
        FieldType::Group => {
            if field.fields.is_empty() {
                "table".to_string()
            } else {
                let sub = format!("{}{}", parent_pascal, to_pascal_case(&field.name));
                format!("crap.group.{sub}")
            }
        }
        // Layout-only; sub-fields are promoted
        FieldType::Row | FieldType::Collapsible | FieldType::Tabs => "table".to_string(),
        FieldType::Upload => {
            if rel_has_many(field) {
                "string[]".to_string()
            } else {
                "string".to_string()
            }
        }
        FieldType::Blocks | FieldType::Join => "table[]".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{checkbox_field, select_field, text_field};
    use super::*;
    use crate::core::SelectOption;
    use crate::core::{LocalizedString, RelationshipConfig};
    #[test]
    fn field_type_mapping() {
        let f = text_field("x", true);
        assert_eq!(field_to_lua_type(&f, "Test"), "string");

        let mut f = text_field("x", true);
        f.field_type = FieldType::Number;
        assert_eq!(field_to_lua_type(&f, "Test"), "number");

        let f = checkbox_field("x");
        assert_eq!(field_to_lua_type(&f, "Test"), "boolean");

        let mut f = text_field("x", true);
        f.field_type = FieldType::Json;
        assert_eq!(field_to_lua_type(&f, "Test"), "any");
    }

    #[test]
    fn select_with_options() {
        let f = select_field("status", true, &["draft", "published"]);
        assert_eq!(field_to_lua_type(&f, "Test"), "\"draft\" | \"published\"");
    }

    #[test]
    fn select_without_options() {
        let f = select_field("status", true, &[]);
        assert_eq!(field_to_lua_type(&f, "Test"), "string");
    }

    #[test]
    fn optional_logic() {
        assert!(!is_optional(&text_field("x", true)));
        assert!(is_optional(&text_field("x", false)));
        let mut cb = checkbox_field("x");
        cb.required = true;
        assert!(is_optional(&cb));
    }

    #[test]
    fn lua_relationship_has_many() {
        let f = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string[]");
    }

    #[test]
    fn lua_polymorphic_has_one_type() {
        let mut rc = RelationshipConfig::new("posts", false);
        rc.polymorphic = vec!["posts".into(), "pages".into()];
        let f = FieldDefinition::builder("subject", FieldType::Relationship)
            .required(true)
            .relationship(rc)
            .build();
        // Polymorphic has-one stores "collection/id" composite as string
        assert_eq!(field_to_lua_type(&f, "Test"), "string");
    }

    #[test]
    fn lua_polymorphic_has_many_type() {
        let mut rc = RelationshipConfig::new("articles", true);
        rc.polymorphic = vec!["articles".into(), "videos".into()];
        let f = FieldDefinition::builder("related", FieldType::Relationship)
            .relationship(rc)
            .build();
        // Polymorphic has-many stores array of "collection/id" composites
        assert_eq!(field_to_lua_type(&f, "Test"), "string[]");
    }

    #[test]
    fn lua_relationship_has_one() {
        let f = FieldDefinition::builder("author", FieldType::Relationship)
            .required(true)
            .relationship(RelationshipConfig::new("users", false))
            .build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string");
    }

    #[test]
    fn lua_array_type() {
        let f = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label", true)])
            .build();
        assert_eq!(field_to_lua_type(&f, "Test"), "crap.array_row.TestItems[]");
    }

    #[test]
    fn lua_group_type_empty() {
        let f = FieldDefinition::builder("seo", FieldType::Group).build();
        assert_eq!(field_to_lua_type(&f, "Test"), "table");
    }

    #[test]
    fn lua_group_type_with_subfields() {
        let f = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                text_field("title", true),
                text_field("description", false),
            ])
            .build();
        assert_eq!(field_to_lua_type(&f, "Test"), "crap.group.TestSeo");
    }

    #[test]
    fn lua_upload_type() {
        let f = FieldDefinition::builder("image", FieldType::Upload).build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string");
    }

    #[test]
    fn upload_has_many_generates_array_type() {
        let f = FieldDefinition::builder("images", FieldType::Upload)
            .relationship(RelationshipConfig::new("", true))
            .build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string[]");
    }

    #[test]
    fn lua_blocks_type() {
        let f = FieldDefinition::builder("content", FieldType::Blocks).build();
        assert_eq!(field_to_lua_type(&f, "Test"), "table[]");
    }

    #[test]
    fn lua_text_has_many() {
        let f = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string[]");
    }

    #[test]
    fn lua_number_has_many() {
        let f = FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build();
        assert_eq!(field_to_lua_type(&f, "Test"), "number[]");
    }

    #[test]
    fn lua_email_type() {
        let f = FieldDefinition::builder("email", FieldType::Email).build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string");
    }

    #[test]
    fn lua_date_type() {
        let f = FieldDefinition::builder("at", FieldType::Date).build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string");
    }

    #[test]
    fn lua_richtext_type() {
        let f = FieldDefinition::builder("body", FieldType::Richtext).build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string");
    }

    #[test]
    fn lua_textarea_type() {
        let f = FieldDefinition::builder("notes", FieldType::Textarea).build();
        assert_eq!(field_to_lua_type(&f, "Test"), "string");
    }

    #[test]
    fn lua_code_join_radio_fields() {
        // Code maps to string, Join maps to table[], Radio maps to string/union
        let f_code = FieldDefinition::builder("snippet", FieldType::Code).build();
        let f_join = FieldDefinition::builder("refs", FieldType::Join).build();
        let f_radio = FieldDefinition::builder("color", FieldType::Radio).build();
        assert_eq!(field_to_lua_type(&f_code, "Test"), "string");
        assert_eq!(field_to_lua_type(&f_join, "Test"), "table[]");
        assert_eq!(field_to_lua_type(&f_radio, "Test"), "string");
    }

    #[test]
    fn lua_select_has_many_with_and_without_options() {
        // has_many with options → (opt1 | opt2|string)[]
        let f_with_opts = FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("A".into()), "a"),
                SelectOption::new(LocalizedString::Plain("B".into()), "b"),
            ])
            .build();
        let result = field_to_lua_type(&f_with_opts, "Test");
        assert!(
            result.contains("\"a\""),
            "should include option 'a': {result}"
        );
        assert!(
            result.contains("\"b\""),
            "should include option 'b': {result}"
        );
        assert!(result.ends_with("[]"), "should be an array type: {result}");

        // has_many without options → string[]
        let f_no_opts = FieldDefinition::builder("cats", FieldType::Select)
            .has_many(true)
            .build();
        assert_eq!(field_to_lua_type(&f_no_opts, "Test"), "string[]");
    }
}
