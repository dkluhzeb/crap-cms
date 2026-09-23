//! The per-collection classes: hook data, the write-shape input classes, the
//! read-shape document class, the hook contexts and the function aliases.

use crate::{
    core::{
        CollectionDefinition,
        upload::{read_shape_fields, writable_fields, write_shape_fields},
    },
    typegen::helpers::{COLLECTION_TAG_KEY, declares_collection_tag, to_pascal_case, w},
};

use super::{
    accessor::{render_collection_accessor, render_collection_typing_factories},
    classes::{
        render_sub_type_classes, write_fields, write_hook_context_tail, write_system_fields,
    },
    field::LuaShape,
    query_classes::render_query_classes,
};

/// The operations an `after_read` hook of a collection runs for: the reads,
/// and the write that produced a live event.
const READ_HOOK_OPERATIONS: &str = r#""find" | "find_by_id" | "create" | "update" | "delete""#;

/// `crap.input.*`, `crap.partial_many.*` and `crap.partial.*` — what `create`,
/// `update_many` and `update` accept: the write shape (no virtual `Join`, no
/// server-derived upload column), each reference as its id, plus an auth
/// collection's `password` where the operation takes one.
fn render_input_classes(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    let fields = write_shape_fields(col);
    let password = col.is_auth_collection();

    // crap.input.* — the `create` / `create_many` / `validate` payload; a
    // required field stays required.
    w!(out, "---@class crap.input.{pascal}");
    write_fields(out, &fields, pascal, LuaShape::Input);
    if password {
        w!(
            out,
            "---@field password? string Hashed on write, never read back"
        );
    }
    out.push('\n');

    // crap.partial_many.* — the `update_many` payload: only the fields the
    // caller wants to change, so every one is optional. Never a password —
    // `update_many` refuses one.
    w!(out, "---@class crap.partial_many.{pascal}");
    write_fields(out, &fields, pascal, LuaShape::Partial);
    out.push('\n');

    // crap.partial.* — the single `update` payload: the same fields, plus
    // an auth collection's password.
    w!(
        out,
        "---@class crap.partial.{pascal} : crap.partial_many.{pascal}"
    );
    if password {
        w!(
            out,
            "---@field password? string Replaces the stored password (empty keeps it)"
        );
    }
    out.push('\n');
}

/// `crap.doc.*` — the returned document. Inherits from `crap.Document` so
/// functions annotated `@return crap.Document` (e.g.
/// `crap.collections.find_by_id`, custom auth strategies) accept the
/// per-collection types without union-mismatch diagnostics from
/// lua-language-server. The read shape: hidden fields stripped, an upload's
/// per-size columns folded into `sizes`, every field optional (a draft may
/// lack required values, and field read access and `select` drop keys), a
/// reference populated at the default depth.
fn render_doc_class(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    let read_fields = read_shape_fields(col);

    w!(out, "---@class crap.doc.{pascal} : crap.Document");
    w!(out, "---@field id string");
    write_fields(out, &read_fields, pascal, LuaShape::Read);
    write_system_fields(out, col.has_drafts(), col.soft_delete);
    if declares_collection_tag(&read_fields) {
        w!(
            out,
            "---@field {COLLECTION_TAG_KEY}? \"{}\" Set when embedded as a populated relationship",
            col.slug
        );
    }
    if col.timestamps {
        w!(out, "---@field created_at? string");
        w!(out, "---@field updated_at? string");
    }
    out.push('\n');
}

/// `crap.data.*` — hook `ctx.data`: the stored fields. `id` and timestamps
/// are emitted as OPTIONAL because the table is reused across hooks where
/// they may or may not be populated (e.g. `before_validate` on create has no
/// id yet; `after_change` does). Optional types let user code reach for
/// `data.id` without `LuaLS` complaining, while still flagging unconditional
/// dereferences without a nil check.
fn render_data_class(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    w!(out, "---@class crap.data.{pascal}");
    w!(out, "---@field id? string");
    write_fields(out, &col.fields, pascal, LuaShape::Input);
    if col.timestamps {
        w!(out, "---@field created_at? string");
        w!(out, "---@field updated_at? string");
    }
    out.push('\n');
}

/// The typed hook contexts: `crap.hook.*` (the write and `before_read`
/// events, `ctx.data` the stored shape) and `crap.read_hook.*` (`after_read`,
/// whose `ctx.data` is the document as the read returns it), plus the
/// `crap.find_result.*` wrapper.
fn render_hook_classes(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    w!(out, "---@class crap.hook.{pascal}");
    w!(out, "---@field collection \"{}\"", col.slug);
    w!(
        out,
        "---@field operation \"create\" | \"update\" | \"delete\" | \"find\" | \"find_by_id\""
    );
    w!(out, "---@field data crap.data.{pascal}");
    write_hook_context_tail(out);

    w!(out, "---@class crap.read_hook.{pascal}");
    w!(out, "---@field collection \"{}\"", col.slug);
    w!(out, "---@field operation {READ_HOOK_OPERATIONS}");
    w!(out, "---@field data crap.doc.{pascal}");
    write_hook_context_tail(out);

    // crap.find_result.* — MUST extend `crap.FindResult` (mirrors
    // `crap.doc.X : crap.Document`). Empirically, LuaLS narrows
    // `---@overload` returns reliably when every variant shares a
    // common parent — `find_by_id` narrows because every variant
    // returns `crap.doc.X : crap.Document`; `find` only narrows once
    // every variant returns `crap.find_result.X : crap.FindResult`.
    // Without inheritance, LuaLS treats the variants as ad-hoc
    // siblings and unions them at the call site.
    w!(out, "---@class crap.find_result.{pascal} : crap.FindResult");
    w!(out, "---@field documents crap.doc.{pascal}[]");
    out.push('\n');
}

/// The one-liner `---@type` function aliases for the collection's hooks and
/// display conditions.
fn render_fn_aliases(out: &mut String, pascal: &str) {
    w!(
        out,
        "---@alias crap.hook_fn.{pascal} fun(ctx: crap.hook.{pascal}): crap.hook.{pascal}"
    );
    w!(
        out,
        "---@alias crap.read_hook_fn.{pascal} fun(ctx: crap.read_hook.{pascal}): crap.read_hook.{pascal}"
    );

    // Field-hook function alias — typed per-collection so field hooks
    // can replace `@param value any` + `@param context …` + `@return`
    // with a single `---@type crap.field_hook_fn.<Pascal>` cast.
    w!(
        out,
        "---@alias crap.field_hook_fn.{pascal} fun(value: any, context: crap.field_hook.{pascal}): any"
    );

    // Display-condition function alias — returns either a boolean
    // (server-evaluated) or a condition table (client-evaluated).
    // See `docs/src/admin-ui/guides/display-conditions.md`.
    w!(
        out,
        "---@alias crap.display_condition_fn.{pascal} fun(data: crap.data.{pascal}, ctx: crap.ConditionContext): boolean | table"
    );
    out.push('\n');
}

/// Render type definitions for a single collection.
pub(super) fn render_collection(out: &mut String, col: &CollectionDefinition) {
    let pascal = to_pascal_case(&col.slug);

    // Two families of sub-type classes: the stored write shape a write and a
    // hook's `ctx.data` hold (`crap.array_row.*` / `crap.group.*` — a
    // virtual `Join` is never stored, at any depth), and the read shape a
    // returned document carries (`crap.doc_row.*` / `crap.doc_group.*`).
    render_sub_type_classes(out, &writable_fields(&col.fields), &pascal, LuaShape::Input);
    render_sub_type_classes(out, &read_shape_fields(col), &pascal, LuaShape::Read);

    render_data_class(out, col, &pascal);
    render_input_classes(out, col, &pascal);
    render_doc_class(out, col, &pascal);
    render_hook_classes(out, col, &pascal);
    render_fn_aliases(out, &pascal);

    // crap.field_hook.* — typed FieldHookContext (data = full document)
    w!(out, "---@class crap.field_hook.{pascal}");
    w!(out, "---@field field_name string");
    w!(out, "---@field collection \"{}\"", col.slug);
    w!(out, "---@field operation string");
    w!(out, "---@field data crap.data.{pascal}");
    w!(out, "---@field user? table");
    w!(out, "---@field ui_locale? string");
    out.push('\n');

    render_query_classes(out, col, &pascal);
    render_collection_accessor(out, &col.slug, &pascal);
    render_collection_typing_factories(out, col, &pascal);
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{checkbox_field, class_block, select_field, text_field};
    use super::*;
    use crate::core::{
        FieldAdmin, FieldDefinition, FieldTab, FieldType, RelationshipConfig, VersionsConfig,
        collection::Auth,
        upload::{CollectionUpload, ImageSizeBuilder},
    };

    /// The generated hook contexts name exactly the operations the runtime
    /// passes: collection hooks include `delete`.
    #[test]
    fn hook_context_operations_match_the_runtime() {
        let mut col = CollectionDefinition::new("posts");
        col.fields = vec![text_field("title", true)];
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(
            out.contains(
                r#"---@field operation "create" | "update" | "delete" | "find" | "find_by_id""#
            ),
            "{out}"
        );
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
            read_hook.contains(&format!("---@field operation {READ_HOOK_OPERATIONS}")),
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

        assert!(out.contains("---@class crap.data.Posts"));
        assert!(out.contains("---@field title string"));
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
}
