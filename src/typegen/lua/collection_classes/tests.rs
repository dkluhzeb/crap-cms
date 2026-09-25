use super::*;
use crate::{
    core::{
        FieldAdmin, FieldDefinition, FieldTab, FieldType, RelationshipConfig, VersionsConfig,
        collection::Auth,
        upload::{CollectionUpload, ImageSizeBuilder},
    },
    typegen::lua::test_helpers::{checkbox_field, class_block, select_field, text_field},
};

/// The generated hook contexts name exactly the operations the runtime
/// passes: collection hooks include `delete` and the state-change
/// `undelete`; `after_read` every live-event operation, `unpublish` and
/// `restore` included.
#[test]
fn hook_context_operations_match_the_runtime() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![text_field("title", true)];
    let mut out = String::new();
    render_collection(&mut out, &col);

    let hook = class_block(&out, "---@class crap.hook.Posts");
    assert!(
            hook.contains(
                r#"---@field operation "create" | "update" | "undelete" | "delete" | "find" | "find_by_id" | "get""#
            ),
            "{hook}"
        );

    let read_hook = class_block(&out, "---@class crap.read_hook.Posts");
    assert!(
            read_hook.contains(
                r#"---@field operation "find" | "find_by_id" | "get" | "create" | "update" | "delete" | "undelete" | "unpublish" | "restore""#
            ),
            "{read_hook}"
        );
}

/// Regression: the typed field-hook context lacked `id`, `locale`,
/// `document` and `options`, and typed `data` as the full document
/// although a nested field's `data` is its group object or row.
#[test]
fn field_hook_context_carries_the_runtime_keys() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![text_field("title", true)];
    let mut out = String::new();
    render_collection(&mut out, &col);

    let block = class_block(&out, "---@class crap.field_hook.Posts");
    for line in [
        "---@field id? string",
        "---@field locale? string",
        "---@field document crap.data.Posts",
        "---@field data crap.data.Posts|table<string, any>",
        "---@field options? table",
    ] {
        assert!(block.contains(line), "{line}: {block}");
    }
}

/// Regression: `after_read` hooks were typed with the stored-shape hook
/// data, although their `ctx.data` is the document as the read returns it
/// (hidden fields stripped, upload sizes folded, references populated).
#[test]
fn after_read_hooks_get_the_read_shape() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![text_field("title", true)];
    let mut out = String::new();
    render_collection(&mut out, &col);

    let read_hook = class_block(&out, "---@class crap.read_hook.Posts");
    assert!(
        read_hook.contains("---@field data crap.doc.Posts"),
        "{read_hook}"
    );
    assert!(
        read_hook.contains(&format!(
            "---@field operation {}",
            literal_union(&collection_read_hook_operations())
        )),
        "{read_hook}"
    );
    assert!(
        out.contains(
            "---@alias crap.read_hook_fn.Posts fun(ctx: crap.read_hook.Posts): crap.read_hook.Posts"
        ),
        "{out}"
    );

    let hook = class_block(&out, "---@class crap.hook.Posts");
    assert!(hook.contains("---@field data crap.data.Posts"), "{hook}");
}

/// A read document may lack any field (a draft, field read access, a
/// select list) and carries the stored system keys the collection has.
#[test]
fn read_document_fields_are_optional_with_system_keys() {
    let mut col = CollectionDefinition::new("events");
    col.soft_delete = true;
    col.versions = Some(VersionsConfig::new(true, 10));
    col.fields = vec![
        text_field("title", true),
        FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);

    let doc = class_block(&out, "---@class crap.doc.Events");
    assert!(doc.contains("---@field title? string"), "{doc}");
    assert!(doc.contains("---@field starts_tz? string"), "{doc}");
    assert!(
        doc.contains(r#"---@field _status? "draft" | "published""#),
        "{doc}"
    );
    assert!(doc.contains("---@field _deleted_at? string"), "{doc}");
}

/// A code field with a language allow-list.
fn code_with_languages(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Code)
        .admin(
            FieldAdmin::builder()
                .languages(vec!["python".to_string()])
                .build(),
        )
        .build()
}

/// A code field with a language allow-list carries its `<name>_lang`
/// companion at the top level, inside a group and inside an array row;
/// one without an allow-list carries none.
#[test]
fn read_document_has_the_code_language_companion() {
    let mut col = CollectionDefinition::new("snippets");
    col.fields = vec![
        code_with_languages("snippet"),
        FieldDefinition::builder("plain", FieldType::Code).build(),
        FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![code_with_languages("example")])
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![code_with_languages("example")])
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);

    let doc = class_block(&out, "---@class crap.doc.Snippets");
    assert!(doc.contains("---@field snippet_lang? string"), "{doc}");

    let meta = class_block(&out, "---@class crap.group.SnippetsMeta");
    assert!(meta.contains("---@field example_lang? string"), "{meta}");

    let items = class_block(&out, "---@class crap.array_row.SnippetsItems");
    assert!(items.contains("---@field example_lang? string"), "{items}");

    assert!(!out.contains("plain_lang"), "{out}");
}

#[test]
fn render_collection_output() {
    let mut col = CollectionDefinition::new("posts");
    col.timestamps = true;
    col.fields = vec![
        text_field("title", true),
        text_field("content", false),
        select_field("status", true, &["draft", "published"]),
        checkbox_field("active"),
    ];

    let mut out = String::new();
    render_collection(&mut out, &col);

    // Hook data may lack any field (an update's before-hooks see only the
    // fields the request sends); the create payload keeps `required`.
    let data = class_block(&out, "---@class crap.data.Posts");
    assert!(data.contains("---@field title? string"), "{data}");
    let input = class_block(&out, "---@class crap.input.Posts");
    assert!(input.contains("---@field title string"), "{input}");
    assert!(out.contains("---@field content? string"));
    assert!(out.contains("---@field status \"draft\" | \"published\""));
    assert!(out.contains("---@field active? boolean"));
    assert!(out.contains("---@class crap.doc.Posts : crap.Document"));
    assert!(out.contains("---@field id string"));
    assert!(out.contains("---@field created_at? string"));
    assert!(out.contains("---@field updated_at? string"));
    assert!(out.contains("---@class crap.hook.Posts"));
    assert!(out.contains("---@field collection \"posts\""));
    assert!(out.contains("---@field data crap.data.Posts"));
    assert!(
        out.contains("---@field hook_depth integer"),
        "hook context should have hook_depth"
    );
    assert!(
        out.contains("---@field draft? boolean"),
        "hook context should have draft"
    );
    assert!(
        out.contains("---@field context table<string, any>"),
        "hook context should have context"
    );
    assert!(out.contains("---@class crap.find_result.Posts"));
    assert!(out.contains("---@field documents crap.doc.Posts[]"));
    assert!(out.contains("---@alias crap.hook_fn.Posts"));
    assert!(out.contains("---@class crap.field_hook.Posts"));
    assert!(out.contains("---@field field_name string"));
    assert!(out.contains("---@class (exact) crap.where.Posts"));
    assert!(out.contains("---@class (exact) crap.query.Posts : crap.FindQuery"));
}

#[test]
fn render_collection_no_timestamps() {
    let mut col = CollectionDefinition::new("tags");
    col.timestamps = false;
    col.fields = vec![text_field("name", true)];

    let mut out = String::new();
    render_collection(&mut out, &col);

    assert!(out.contains("---@class crap.doc.Tags : crap.Document"));
    assert!(!out.contains("created_at"));
}

#[test]
fn hook_context_has_user_and_ui_locale() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![text_field("title", true)];

    let mut out = String::new();
    render_collection(&mut out, &col);

    // Every typed hook context carries the user and UI locale the
    // runtime sets.
    for header in [
        "---@class crap.hook.Posts",
        "---@class crap.read_hook.Posts",
        "---@class crap.field_hook.Posts",
    ] {
        let block = class_block(&out, header);
        assert!(block.contains("---@field user? table"), "{block}");
        assert!(block.contains("---@field ui_locale? string"), "{block}");
    }
}

#[test]
fn lua_polymorphic_comment_emitted() {
    let mut rc = RelationshipConfig::new("posts", false);
    rc.polymorphic = vec!["posts".into(), "pages".into()];
    let mut col = CollectionDefinition::new("comments");
    col.fields = vec![
        FieldDefinition::builder("subject", FieldType::Relationship)
            .required(true)
            .relationship(rc)
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("Polymorphic relationship"),
        "should have polymorphic comment: {out}"
    );
    assert!(
        out.contains("posts"),
        "comment should list target collections: {out}"
    );
    assert!(
        out.contains("pages"),
        "comment should list target collections: {out}"
    );
}

#[test]
fn render_collection_with_array_subtype() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label", true), text_field("desc", false)])
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("---@class crap.array_row.PostsItems\n---@field id? string"));
    assert!(out.contains("---@field label string"));
    assert!(out.contains("---@field desc? string"));
}

#[test]
fn lua_row_collapsible_tabs_promote_subfields() {
    let mut col = CollectionDefinition::new("items");
    col.fields = vec![
        FieldDefinition::builder("layout_row", FieldType::Row)
            .fields(vec![
                text_field("first_name", true),
                text_field("last_name", false),
            ])
            .build(),
        FieldDefinition::builder("details", FieldType::Collapsible)
            .fields(vec![text_field("bio", false)])
            .build(),
        FieldDefinition::builder("sections", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Tab1",
                vec![text_field("tab_field", true)],
            )])
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);
    // Row sub-fields promoted — "layout_row" should not appear as a @field
    assert!(
        !out.contains("@field layout_row"),
        "row field name should not appear: {out}"
    );
    assert!(
        out.contains("---@field first_name string"),
        "row required sub-field promoted: {out}"
    );
    assert!(
        out.contains("---@field last_name? string"),
        "row optional sub-field promoted: {out}"
    );
    // Collapsible sub-fields promoted
    assert!(
        !out.contains("@field details"),
        "collapsible field name should not appear: {out}"
    );
    assert!(
        out.contains("---@field bio? string"),
        "collapsible sub-field promoted: {out}"
    );
    // Tabs sub-fields promoted
    assert!(
        !out.contains("@field sections"),
        "tabs field name should not appear: {out}"
    );
    assert!(
        out.contains("---@field tab_field string"),
        "tabs sub-field promoted: {out}"
    );
}

#[test]
fn lua_group_subtype_emitted_in_collection() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                text_field("title", true),
                text_field("description", false),
            ])
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("---@class crap.group.PostsSeo"),
        "group sub-type class should be emitted with collection prefix: {out}"
    );
    assert!(
        out.contains("---@field title string"),
        "group sub-field: {out}"
    );
    assert!(
        out.contains("---@field description? string"),
        "group sub-field optional: {out}"
    );
}

/// Regression: two collections with identically-named array fields must
/// produce distinct sub-type class names (prefixed with the collection's
/// `PascalCase` name) so they don't collide.
#[test]
fn array_subtype_names_prefixed_with_collection_name() {
    let mut posts = CollectionDefinition::new("posts");
    posts.fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label", true)])
            .build(),
    ];

    let mut pages = CollectionDefinition::new("pages");
    pages.fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("url", true)])
            .build(),
    ];

    let mut posts_out = String::new();
    render_collection(&mut posts_out, &posts);

    let mut pages_out = String::new();
    render_collection(&mut pages_out, &pages);

    assert!(
        posts_out.contains("---@class crap.array_row.PostsItems"),
        "posts sub-type should be PostsItems, got:\n{posts_out}"
    );
    assert!(
        pages_out.contains("---@class crap.array_row.PagesItems"),
        "pages sub-type should be PagesItems, got:\n{pages_out}"
    );
    assert!(
        !posts_out.contains("PagesItems"),
        "posts must not declare the pages class"
    );
}

#[test]
fn lua_array_nested_in_row_emits_subtype() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![text_field("val", true)])
                    .build(),
            ])
            .build(),
        FieldDefinition::builder("tabs", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "T",
                vec![
                    FieldDefinition::builder("tab_items", FieldType::Array)
                        .fields(vec![text_field("name", true)])
                        .build(),
                ],
            )])
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("---@class crap.array_row.PostsItems"),
        "array inside Row should emit sub-type with collection prefix: {out}"
    );
    assert!(
        out.contains("---@class crap.array_row.PostsTabItems"),
        "array inside Tabs should emit sub-type with collection prefix: {out}"
    );
}

/// A read of an upload collection folds the per-size columns into one
/// nested `sizes` object, so the returned-document class must describe
/// `sizes` and never the columns it replaced. The hook-data class keeps
/// describing the stored columns a hook sees.
#[test]
fn upload_read_class_describes_sizes_not_the_per_size_columns() {
    let mut upload = CollectionUpload::new();
    upload.image_sizes = vec![
        ImageSizeBuilder::new("thumb")
            .width(200)
            .height(200)
            .build(),
    ];

    let mut col = CollectionDefinition::new("media");
    col.fields = upload
        .size_columns()
        .into_iter()
        .map(|(name, ty)| FieldDefinition::builder(name, ty).build())
        .collect();
    col.fields.push(text_field("alt", false));
    col.upload = Some(upload);

    let mut out = String::new();
    render_collection(&mut out, &col);

    let doc = class_block(&out, "---@class crap.doc.Media");
    assert!(doc.contains("---@field sizes?"), "{doc}");
    assert!(
        !doc.contains("thumb_url"),
        "a read never carries the per-size columns: {doc}"
    );

    let data = class_block(&out, "---@class crap.data.Media");
    assert!(
        data.contains("thumb_url"),
        "the hook-data class describes the stored columns: {data}"
    );
}

/// Regression: the create payload class declared the server-derived
/// upload columns (stripped from every untrusted write) and the virtual
/// joins (never stored). The input classes follow the write shape; the
/// hook-data class keeps the stored fields a hook sees.
#[test]
fn input_classes_follow_the_write_shape() {
    let mut col = CollectionDefinition::new("media");
    col.fields = vec![
        text_field("filename", true),
        text_field("url", false),
        FieldDefinition::builder("focal_x", FieldType::Number).build(),
        text_field("alt", true),
        FieldDefinition::builder("mentions", FieldType::Join).build(),
    ];
    col.upload = Some(CollectionUpload::new());

    let mut out = String::new();
    render_collection(&mut out, &col);

    let input = class_block(&out, "---@class crap.input.Media");
    assert!(input.contains("---@field alt string"), "{input}");
    assert!(input.contains("---@field focal_x? number"), "{input}");
    for absent in ["filename", "url", "mentions"] {
        assert!(!input.contains(absent), "{absent}: {input}");
    }

    let partial = class_block(&out, "---@class crap.partial_many.Media");
    assert!(partial.contains("---@field alt? string"), "{partial}");
    assert!(!partial.contains("url"), "{partial}");

    let data = class_block(&out, "---@class crap.data.Media");
    assert!(data.contains("---@field url? string"), "{data}");
}

/// Regression: the nested row and group classes the input classes
/// reference were built from the raw fields, so a virtual `Join` inside a
/// group or array row was declared writable.
#[test]
fn nested_joins_are_not_writable() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![
                text_field("note", false),
                FieldDefinition::builder("mentions", FieldType::Join).build(),
            ])
            .build(),
    ];

    let mut out = String::new();
    render_collection(&mut out, &col);

    let group = class_block(&out, "---@class crap.group.PostsMeta");
    assert!(group.contains("---@field note? string"), "{group}");
    assert!(!group.contains("mentions"), "{group}");

    let read_group = class_block(&out, "---@class crap.doc_group.PostsMeta");
    assert!(read_group.contains("mentions"), "{read_group}");
}

/// Regression: `crap.data.*` was rendered from every field while the group
/// classes came from the stored ones, so a group holding only a join named a
/// `crap.group.*` class nothing declared (and a join, never stored, was typed
/// as hook data). Hook data now takes the stored set the classes come from.
#[test]
fn hook_data_names_only_declared_group_classes() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        FieldDefinition::builder("links", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("mentions", FieldType::Join).build(),
            ])
            .build(),
        FieldDefinition::builder("authored", FieldType::Join).build(),
    ];

    let mut out = String::new();
    render_collection(&mut out, &col);

    let data = class_block(&out, "---@class crap.data.Posts");
    assert!(!data.contains("crap.group.PostsLinks"), "{data}");
    assert!(!out.contains("---@class crap.group.PostsLinks"), "{out}");
    assert!(!data.contains("authored"), "{data}");
    assert!(!data.contains("mentions"), "{data}");
}

/// An auth collection's create and single update take a `password`
/// beside the fields; `update_many` refuses one, so its payload class has
/// none. A password is never hook data or read back.
#[test]
fn auth_input_classes_take_a_password() {
    let mut col = CollectionDefinition::new("users");
    col.auth = Some(Auth::new(true));
    col.fields = vec![text_field("email", true)];

    let mut out = String::new();
    render_collection(&mut out, &col);

    let input = class_block(&out, "---@class crap.input.Users");
    assert!(input.contains("---@field password? string"), "{input}");
    let partial = class_block(&out, "---@class crap.partial.Users");
    assert!(partial.contains("---@field password? string"), "{partial}");
    assert!(
        partial.contains(": crap.partial_many.Users"),
        "the single-update payload extends the bulk one: {partial}"
    );

    let many = class_block(&out, "---@class crap.partial_many.Users");
    assert!(many.contains("---@field email? string"), "{many}");
    assert!(!many.contains("password"), "{many}");

    let data = class_block(&out, "---@class crap.data.Users");
    assert!(!data.contains("password"), "{data}");
    let doc = class_block(&out, "---@class crap.doc.Users");
    assert!(!doc.contains("password"), "{doc}");
}

/// Regression: the returned-document class typed a relationship as its id
/// although the default read depth populates it, declared `hidden` fields
/// every read strips, and lacked the `collection` tag a populated copy
/// carries. Nested rows and groups get the read-shape classes.
#[test]
fn doc_class_follows_the_read_shape() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", false))
            .build(),
        FieldDefinition::builder("secret", FieldType::Text)
            .hidden(true)
            .build(),
        FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("editor", FieldType::Relationship)
                    .relationship(RelationshipConfig::new("users", false))
                    .build(),
            ])
            .build(),
    ];

    let mut out = String::new();
    render_collection(&mut out, &col);

    let doc = class_block(&out, "---@class crap.doc.Posts");
    assert!(
        doc.contains("---@field author? string|crap.doc.Users"),
        "{doc}"
    );
    assert!(
        doc.contains("---@field meta? crap.doc_group.PostsMeta"),
        "{doc}"
    );
    assert!(doc.contains(r#"---@field collection? "posts""#), "{doc}");
    assert!(!doc.contains("secret"), "{doc}");

    let read_group = class_block(&out, "---@class crap.doc_group.PostsMeta");
    assert!(
        read_group.contains("---@field editor? string|crap.doc.Users"),
        "{read_group}"
    );

    let input_group = class_block(&out, "---@class crap.group.PostsMeta");
    assert!(
        input_group.contains("---@field editor? string"),
        "{input_group}"
    );
    assert!(!input_group.contains("crap.doc"), "{input_group}");

    let input = class_block(&out, "---@class crap.input.Posts");
    assert!(input.contains("---@field secret? string"), "{input}");
    assert!(input.contains("---@field author? string"), "{input}");
    assert!(!input.contains("crap.doc"), "{input}");
}

/// Regression: there was no `locale = "all"` read class, so a typed
/// all-locales read typed each localized field as its scalar. A localized
/// column reads as a per-locale table (its companion too), a group holding
/// one as its localized class, an unlocalized field and an array row as
/// in `crap.doc.*`.
#[test]
fn all_locales_read_classes_type_localized_fields_per_locale() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        text_field("title", true),
        FieldDefinition::builder("summary", FieldType::Date)
            .timezone(true)
            .localized(true)
            .build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .localized(true)
            .fields(vec![text_field("meta_title", false)])
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .localized(true)
            .fields(vec![text_field("label", false)])
            .build(),
    ];

    let mut out = String::new();
    render_collection(&mut out, &col);

    let doc = class_block(&out, "---@class crap.doc_localized.Posts : crap.Document");
    for line in [
        "---@field title? string\n",
        "---@field summary? table<string, string>",
        "---@field summary_tz? table<string, string>",
        "---@field seo? crap.doc_group_localized.PostsSeo",
        "---@field items? crap.doc_row.PostsItems[]",
    ] {
        assert!(doc.contains(line), "{line}: {doc}");
    }

    let seo = class_block(&out, "---@class crap.doc_group_localized.PostsSeo");
    assert!(
        seo.contains("---@field meta_title? table<string, string>"),
        "{seo}"
    );

    assert!(
        out.contains("---@class crap.find_result_localized.Posts : crap.FindResult"),
        "{out}"
    );
    assert!(
        out.contains(
            "---@class crap.query_all_locales.Posts : crap.query.Posts\n---@field locale \"all\""
        ),
        "{out}"
    );

    let mut plain = CollectionDefinition::new("tags");
    plain.fields = vec![text_field("name", true)];
    let mut out = String::new();
    render_collection(&mut out, &plain);
    assert!(
        !out.contains("localized") && !out.contains("all_locales"),
        "{out}"
    );
}

/// Regression: a partial update typed a group with its create class, so
/// `update(id, { seo = { summary = "x" } })` was a `missing-fields`
/// diagnostic for the group's required `title` — though the update keeps
/// every sub-field it does not send. The partial classes (and hook data)
/// take `crap.group_partial.*`, every sub-field optional, nested groups
/// too; the create class keeps `crap.group.*`, and a group inside an array
/// row — written whole with its row — keeps it as well.
#[test]
fn a_partial_write_takes_the_partial_group_class() {
    let social = FieldDefinition::builder("social", FieldType::Group)
        .fields(vec![text_field("handle", true)])
        .build();
    let seo = FieldDefinition::builder("seo", FieldType::Group)
        .fields(vec![
            text_field("title", true),
            text_field("summary", false),
            social,
        ])
        .build();
    let meta = FieldDefinition::builder("meta", FieldType::Group)
        .fields(vec![text_field("key", true)])
        .build();
    let items = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![meta])
        .build();

    let mut col = CollectionDefinition::new("pages");
    col.fields = vec![seo, items];
    let mut out = String::new();
    render_collection(&mut out, &col);

    for class in [
        "---@class crap.partial_many.Pages\n",
        "---@class crap.data.Pages\n",
    ] {
        let block = class_block(&out, class);
        assert!(
            block.contains("---@field seo? crap.group_partial.PagesSeo\n"),
            "{block}"
        );
    }
    let input = class_block(&out, "---@class crap.input.Pages\n");
    assert!(
        input.contains("---@field seo? crap.group.PagesSeo\n"),
        "{input}"
    );

    let partial = class_block(&out, "---@class crap.group_partial.PagesSeo\n");
    for line in [
        "---@field title? string|crap.Null",
        "---@field summary? string|crap.Null",
        "---@field social? crap.group_partial.PagesSeoSocial",
    ] {
        assert!(partial.contains(line), "{line}: {partial}");
    }

    let nested = class_block(&out, "---@class crap.group_partial.PagesSeoSocial\n");
    assert!(
        nested.contains("---@field handle? string|crap.Null"),
        "{nested}"
    );

    // `class_block` ends before the block's final newline, so match whole lines.
    let row = class_block(&out, "---@class crap.array_row.PagesItems\n");
    assert!(
        row.lines()
            .any(|line| line == "---@field meta? crap.group.PagesItemsMeta"),
        "{row}"
    );
    assert!(!out.contains("crap.group_partial.PagesItemsMeta"), "{out}");
}
