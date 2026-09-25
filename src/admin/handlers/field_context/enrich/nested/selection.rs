//! Recursively enriches nested relationship/upload fields — inside layout
//! containers and array/blocks rows — with their DB-resolved selections.

use serde_json::Value;

use crate::{
    admin::{
        context::field::{
            ArrayField, BlocksField, FieldContext, RelationshipField, RelationshipSelectedItem,
            UploadField,
        },
        handlers::{
            field_context::enrich::{
                EnrichCtx, gated_find_by_id, polymorphic_selected_from_value,
                types::{
                    build_upload_item, enrich_richtext, resolve_has_many_items,
                    resolve_upload_has_many,
                },
            },
            shared::admin_form_fields,
        },
    },
    core::{FieldDefinition, upload},
};

/// Recursively enrich Upload and Relationship sub-field contexts with options from the database.
/// Called for sub-fields inside layout containers (Row, Collapsible, Tabs, Group) and
/// composite fields (Array, Blocks) that can't be enriched during initial context building.
///
/// The defs run through [`admin_form_fields`] — the same filter that produced
/// `sub_fields` — so the `zip` pairs each context with the def it was built from.
pub fn enrich_nested_fields(
    sub_fields: &mut [FieldContext],
    field_defs: &[FieldDefinition],
    ctx: &EnrichCtx,
) {
    for (fc, field_def) in sub_fields.iter_mut().zip(admin_form_fields(field_defs)) {
        match fc {
            FieldContext::Relationship(rf) => {
                enrich_nested_relationship(rf, field_def, ctx);
            }
            FieldContext::Upload(uf) => {
                enrich_nested_upload(uf, field_def, ctx);
            }
            FieldContext::Row(rfld) => {
                enrich_nested_fields(&mut rfld.sub_fields, &field_def.fields, ctx);
            }
            FieldContext::Collapsible(gf) | FieldContext::Group(gf) => {
                enrich_nested_fields(&mut gf.sub_fields, &field_def.fields, ctx);
            }
            FieldContext::Tabs(tf) => {
                for (tab_panel, tab_def) in tf.tabs.iter_mut().zip(field_def.tabs.iter()) {
                    enrich_nested_fields(&mut tab_panel.sub_fields, &tab_def.fields, ctx);
                }
            }
            FieldContext::Array(af) => {
                enrich_nested_array(af, field_def, ctx);
            }
            FieldContext::Blocks(bf) => {
                enrich_nested_blocks(bf, field_def, ctx);
            }
            FieldContext::Richtext(rf) => {
                // A richtext field inside a Group must get its custom-node
                // enrichment too, exactly as one inside a Row/Tabs does —
                // otherwise the group's editor is missing node definitions.
                enrich_richtext(rf, ctx.reg);
            }
            _ => {}
        }
    }
}

/// Extract selected relationship ids from a nested row's stored value — a JSON
/// array of id strings, or a JSON-array string (both shapes appear depending on
/// whether the row came from a join table or nested JSON).
fn selected_ids_from_value(value: &Value) -> Vec<String> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
            .collect(),
        Value::String(s) => serde_json::from_str::<Vec<String>>(s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn enrich_nested_relationship(
    rf: &mut RelationshipField,
    field_def: &FieldDefinition,
    ctx: &EnrichCtx,
) {
    let Some(ref rc) = field_def.relationship else {
        return;
    };

    // Polymorphic values are `collection/id` composites, each resolved in its
    // own collection — never against the first target collection. Left
    // unresolved, the component would submit an empty value and a save would
    // clear the stored references.
    if rc.is_polymorphic() {
        rf.selected_items = Some(polymorphic_selected_from_value(
            rc,
            Some(&rf.base.value),
            ctx,
        ));
        return;
    }

    let Some(related_def) = ctx.reg.get_collection(&rc.collection) else {
        return;
    };
    let title_field = related_def
        .title_field()
        .map(std::string::ToString::to_string);

    // Has-many: resolve labels for every selected id from this row's value.
    // (Nothing "upstream" builds these — a has-many relationship inside a group
    // or array/block row previously rendered with no chips because the value
    // carries the ids but the labels were never resolved.)
    if rc.has_many {
        let ids = selected_ids_from_value(&rf.base.value);
        rf.selected_items = Some(resolve_has_many_items(
            &ids,
            &rc.collection,
            related_def,
            title_field.as_ref(),
            ctx,
        ));
        return;
    }

    let current_value = rf.base.value.as_str().unwrap_or("");

    if current_value.is_empty() {
        rf.selected_items = Some(Vec::new());
        return;
    }

    // Access-gated: never label a nested relationship target the viewer can't read.
    let item = gated_find_by_id(ctx, &rc.collection, related_def, current_value).map(|doc| {
        let label = title_field
            .as_ref()
            .and_then(|f| doc.get_str(f))
            .unwrap_or(&doc.id)
            .to_string();
        RelationshipSelectedItem {
            id: doc.id.to_string(),
            label,
            ..Default::default()
        }
    });

    // A target the viewer cannot resolve keeps its stored id (unlabelled).
    let item = item.unwrap_or_else(|| RelationshipSelectedItem::unavailable(current_value));

    rf.selected_items = Some(vec![item]);
}

fn enrich_nested_upload(uf: &mut UploadField, field_def: &FieldDefinition, ctx: &EnrichCtx) {
    let Some(ref rc) = field_def.relationship else {
        return;
    };

    let Some(related_def) = ctx.reg.get_collection(&rc.collection) else {
        return;
    };

    let title_field = related_def
        .title_field()
        .map(std::string::ToString::to_string);
    let admin_thumbnail = related_def
        .upload
        .as_ref()
        .and_then(|u| u.admin_thumbnail.clone());

    // Has-many: resolve every selected upload's label/thumbnail from this row's
    // value — nothing upstream builds these (same fix as nested has-many
    // relationships).
    if rc.has_many {
        let ids = selected_ids_from_value(&uf.base.value);
        uf.selected_items = Some(resolve_upload_has_many(
            &ids,
            &rc.collection,
            related_def,
            title_field.as_ref(),
            admin_thumbnail.as_ref(),
            ctx,
        ));
        return;
    }

    let current_value = uf.base.value.as_str().unwrap_or("");

    if current_value.is_empty() {
        uf.selected_items = Some(Vec::new());
        return;
    }

    // Access-gated: never label a nested upload target the viewer cannot read.
    let Some(mut doc) = gated_find_by_id(ctx, &rc.collection, related_def, current_value) else {
        uf.selected_items = Some(vec![RelationshipSelectedItem::unavailable(current_value)]);
        return;
    };

    upload::shape_read_document(related_def, &mut doc);

    let item = build_upload_item(&doc, title_field.as_ref(), admin_thumbnail.as_ref(), true);
    let label = item.label.clone();
    let thumb_url = item.thumbnail_url.clone();

    uf.selected_items = Some(vec![item]);
    uf.selected_filename = Some(label);

    if let Some(url) = thumb_url {
        uf.selected_preview_url = Some(url);
    }
}

fn enrich_nested_array(af: &mut ArrayField, field_def: &FieldDefinition, ctx: &EnrichCtx) {
    // Recurse into array rows' sub-fields
    if let Some(rows) = af.rows.as_mut() {
        for row in rows.iter_mut() {
            enrich_nested_fields(&mut row.sub_fields, &field_def.fields, ctx);
        }
    }

    // Enrich the <template> sub-fields so new rows added via JS have upload/relationship options
    enrich_nested_fields(&mut af.sub_fields, &field_def.fields, ctx);
}

fn enrich_nested_blocks(bf: &mut BlocksField, field_def: &FieldDefinition, ctx: &EnrichCtx) {
    // Recurse into block rows' sub-fields, matching each row's block type
    if let Some(rows) = bf.rows.as_mut() {
        for row in rows.iter_mut() {
            if let Some(block_def) = field_def
                .blocks
                .iter()
                .find(|bd| bd.block_type == row.block_type)
            {
                enrich_nested_fields(&mut row.sub_fields, &block_def.fields, ctx);
            }
        }
    }

    // Enrich block definition templates so new block rows have upload/relationship options
    for (def_ctx, block_def) in bf.block_definitions.iter_mut().zip(field_def.blocks.iter()) {
        enrich_nested_fields(&mut def_ctx.fields, &block_def.fields, ctx);
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        admin::handlers::field_context::enrich::test_helpers::{
            enrich_nested_fields_values, make_field,
        },
        core::{
            BlockDefinition, CollectionDefinition, FieldType, Registry, RelationshipConfig,
            upload::CollectionUpload,
        },
    };

    // ── enrich_nested_fields: relationships, uploads, recursion ──────

    #[test]
    fn enrich_nested_fields_upload_gets_options() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                alt TEXT,
                caption TEXT,
                filename TEXT,
                mime_type TEXT,
                url TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, alt, filename, mime_type, url, created_at, updated_at)
            VALUES ('img1', 'Logo', 'logo.png', 'image/png', '/uploads/media/logo.png', '2024-01-01', '2024-01-01');
            INSERT INTO media (id, alt, filename, mime_type, url, created_at, updated_at)
            VALUES ('img2', 'Banner', 'banner.jpg', 'image/jpeg', '/uploads/media/banner.jpg', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let mut media_def = CollectionDefinition::new("media");
        media_def.timestamps = true;
        media_def.fields = vec![
            make_field("alt", FieldType::Text),
            make_field("caption", FieldType::Text),
            make_field("filename", FieldType::Text),
            make_field("mime_type", FieldType::Text),
            make_field("url", FieldType::Text),
        ];
        media_def.upload = Some(CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".to_string()],
            ..Default::default()
        });

        let mut registry = Registry::new();
        registry.register_collection(media_def);

        let mut upload_field = make_field("image", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig::new("media", false));

        let field_defs = vec![upload_field];
        let mut sub_fields = vec![json!({
            "name": "content[0][image]",
            "field_type": "upload",
            "value": "img1",
            "relationship_collection": "media",
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let items = sub_fields[0]["selected_items"]
            .as_array()
            .expect("selected_items should be populated");
        assert_eq!(items.len(), 1, "Should have 1 selected item");
        assert_eq!(items[0]["id"], "img1");
        assert_eq!(items[0]["label"], "logo.png");
    }

    /// Regression (form #5, upload variant): a HAS-MANY upload nested in a row
    /// must resolve every selected file's label — previously it returned early
    /// with no `selected_items`, so no thumbnails showed.
    #[test]
    fn enrich_nested_fields_has_many_upload_gets_all_labels() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                alt TEXT,
                caption TEXT,
                filename TEXT,
                mime_type TEXT,
                url TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, filename, mime_type, url, created_at, updated_at)
            VALUES ('img1', 'logo.png', 'image/png', '/uploads/media/logo.png', '2024-01-01', '2024-01-01');
            INSERT INTO media (id, filename, mime_type, url, created_at, updated_at)
            VALUES ('img2', 'banner.jpg', 'image/jpeg', '/uploads/media/banner.jpg', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let mut media_def = CollectionDefinition::new("media");
        media_def.timestamps = true;
        media_def.fields = vec![
            make_field("filename", FieldType::Text),
            make_field("mime_type", FieldType::Text),
            make_field("url", FieldType::Text),
        ];
        media_def.upload = Some(CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".to_string()],
            ..Default::default()
        });

        let mut registry = Registry::new();
        registry.register_collection(media_def);

        let mut upload_field = make_field("gallery", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig::new("media", true));

        let field_defs = vec![upload_field];
        let mut sub_fields = vec![json!({
            "name": "content[0][gallery]",
            "field_type": "upload",
            "value": ["img1", "img2"],
            "relationship_collection": "media",
            "has_many": true,
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let items = sub_fields[0]["selected_items"]
            .as_array()
            .expect("selected_items should be populated for has-many upload");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "img1");
        assert_eq!(items[1]["id"], "img2");
    }

    #[test]
    fn enrich_nested_fields_relationship_gets_options() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, name, created_at, updated_at)
            VALUES ('u1', 'Alice', '2024-01-01', '2024-01-01');
            INSERT INTO users (id, name, created_at, updated_at)
            VALUES ('u2', 'Bob', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut users_def = CollectionDefinition::new("users");
        users_def.timestamps = true;
        users_def.fields = vec![make_field("name", FieldType::Text)];
        users_def.admin.use_as_title = Some("name".to_string());

        let mut registry = Registry::new();
        registry.register_collection(users_def);

        let mut rel_field = make_field("author", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig::new("users", false));

        let field_defs = vec![rel_field];
        let mut sub_fields = vec![json!({
            "name": "items[0][author]",
            "field_type": "relationship",
            "value": "u1",
            "relationship_collection": "users",
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let items = sub_fields[0]["selected_items"]
            .as_array()
            .expect("selected_items should be populated");
        assert_eq!(items.len(), 1, "Should have 1 selected item");
        assert_eq!(items[0]["id"], "u1");
        assert_eq!(items[0]["label"], "Alice");
    }

    /// Regression (form #5): a HAS-MANY relationship nested in a row must
    /// resolve labels for every selected id — previously it returned early with
    /// no `selected_items`, so the editor saw no chips (the ids saved fine, but
    /// the current selection was invisible).
    #[test]
    fn enrich_nested_fields_has_many_relationship_gets_all_labels() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, name, created_at, updated_at)
            VALUES ('u1', 'Alice', '2024-01-01', '2024-01-01');
            INSERT INTO users (id, name, created_at, updated_at)
            VALUES ('u2', 'Bob', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut users_def = CollectionDefinition::new("users");
        users_def.timestamps = true;
        users_def.fields = vec![make_field("name", FieldType::Text)];
        users_def.admin.use_as_title = Some("name".to_string());

        let mut registry = Registry::new();
        registry.register_collection(users_def);

        let mut rel_field = make_field("authors", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig::new("users", true));

        let field_defs = vec![rel_field];
        let mut sub_fields = vec![json!({
            "name": "items[0][authors]",
            "field_type": "relationship",
            "value": ["u1", "u2"],
            "relationship_collection": "users",
            "has_many": true,
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let items = sub_fields[0]["selected_items"]
            .as_array()
            .expect("selected_items should be populated for has-many");
        assert_eq!(items.len(), 2, "both selected ids resolve to labels");
        assert_eq!(items[0]["id"], "u1");
        assert_eq!(items[0]["label"], "Alice");
        assert_eq!(items[1]["id"], "u2");
        assert_eq!(items[1]["label"], "Bob");
    }

    #[test]
    fn enrich_nested_fields_recurses_into_layout() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tags (
                id TEXT PRIMARY KEY,
                label TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO tags (id, label, created_at, updated_at)
            VALUES ('t1', 'Rust', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut tags_def = CollectionDefinition::new("tags");
        tags_def.timestamps = true;
        tags_def.fields = vec![make_field("label", FieldType::Text)];
        tags_def.admin.use_as_title = Some("label".to_string());

        let mut registry = Registry::new();
        registry.register_collection(tags_def);

        let mut rel_field = make_field("tag", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig::new("tags", false));
        let row_field = FieldDefinition::builder("row1", FieldType::Row)
            .fields(vec![rel_field])
            .build();

        let field_defs = vec![row_field];
        let mut sub_fields = vec![json!({
            "name": "row1",
            "field_type": "row",
            "sub_fields": [{
                "name": "tag",
                "field_type": "relationship",
                "value": "",
                "relationship_collection": "tags",
            }],
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let row_subs = sub_fields[0]["sub_fields"].as_array().unwrap();
        let items = row_subs[0]["selected_items"]
            .as_array()
            .expect("Nested relationship inside Row should be enriched");
        assert_eq!(
            items.len(),
            0,
            "Empty value should produce empty selected_items"
        );
    }

    /// Regression: block-definition templates (used to render new rows) must
    /// have their upload fields enriched with `selected_items` context.
    #[test]
    fn enrich_nested_fields_blocks_template_gets_upload_options() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                filename TEXT,
                mime_type TEXT,
                url TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, filename, mime_type, url, created_at, updated_at)
            VALUES ('m1', 'photo.jpg', 'image/jpeg', '/uploads/photo.jpg', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let mut media_def = CollectionDefinition::new("media");
        media_def.timestamps = true;
        media_def.fields = vec![
            make_field("filename", FieldType::Text),
            make_field("mime_type", FieldType::Text),
            make_field("url", FieldType::Text),
        ];
        media_def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });

        let mut registry = Registry::new();
        registry.register_collection(media_def);

        let mut upload_field = make_field("image", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig::new("media", false));
        let mut blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        blocks_field.blocks = vec![BlockDefinition::new("image", vec![upload_field])];

        let field_defs = vec![blocks_field];
        let mut sub_fields = vec![json!({
            "name": "content",
            "field_type": "blocks",
            "block_definitions": [{
                "block_type": "image",
                "label": "Image",
                "fields": [{
                    "name": "content[__INDEX__][image]",
                    "field_type": "upload",
                    "value": "",
                    "relationship_collection": "media",
                }],
            }],
            "rows": [],
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let block_defs = sub_fields[0]["block_definitions"].as_array().unwrap();
        let fields = block_defs[0]["fields"].as_array().unwrap();
        let items = fields[0]["selected_items"]
            .as_array()
            .expect("Upload inside block template should have selected_items");
        assert_eq!(
            items.len(),
            0,
            "Empty value should produce empty selected_items"
        );
    }

    /// Regression: array-template sub-fields (used for the new-row UI) must
    /// have upload `selected_items` enriched.
    #[test]
    fn enrich_nested_fields_array_template_gets_upload_options() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                filename TEXT,
                mime_type TEXT,
                url TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, filename, mime_type, url, created_at, updated_at)
            VALUES ('m1', 'doc.pdf', 'application/pdf', '/uploads/doc.pdf', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let mut media_def = CollectionDefinition::new("media");
        media_def.timestamps = true;
        media_def.fields = vec![
            make_field("filename", FieldType::Text),
            make_field("mime_type", FieldType::Text),
            make_field("url", FieldType::Text),
        ];
        media_def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });

        let mut registry = Registry::new();
        registry.register_collection(media_def);

        let mut upload_field = make_field("file", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig::new("media", false));
        let array_field = FieldDefinition::builder("attachments", FieldType::Array)
            .fields(vec![upload_field])
            .build();

        let field_defs = vec![array_field];
        let mut sub_fields = vec![json!({
            "name": "attachments",
            "field_type": "array",
            "sub_fields": [{
                "name": "attachments[__INDEX__][file]",
                "field_type": "upload",
                "value": "",
                "relationship_collection": "media",
            }],
            "rows": [],
        })];

        enrich_nested_fields_values(&mut sub_fields, &field_defs, &conn, &registry, None);

        let template_fields = sub_fields[0]["sub_fields"].as_array().unwrap();
        let items = template_fields[0]["selected_items"]
            .as_array()
            .expect("Upload inside array template should have selected_items");
        assert_eq!(
            items.len(),
            0,
            "Empty value should produce empty selected_items"
        );
    }
}
