use super::*;
use crate::{
    core::{
        BlockDefinition, CollectionDefinition, FieldDefinition, FieldTab, FieldType,
        GlobalDefinition, LocalizedString, RelationshipConfig, SelectOption, VersionsConfig,
    },
    typegen::client::drive,
};

fn render(registry: &Registry) -> String {
    drive(registry, Box::new(GoPrinter::new()))
}

/// Regression: a user field whose name maps onto a system key's member
/// (`deleted_at` → `DeletedAt`) claimed that name first and renamed the
/// system key to `DeletedAt_2` — adding a field silently renamed a system
/// member. The system keys now claim their names before any field does.
#[test]
fn go_system_keys_keep_their_names() {
    let mut col = make_col(
        "posts",
        vec![
            text_field("deleted_at", false),
            text_field("draft_status", false),
        ],
    );
    col.soft_delete = true;
    col.versions = Some(VersionsConfig::new(true, 10));
    let mut out = String::new();
    render_collection(&mut out, &col);

    assert!(
        out.contains("DeletedAt *string `json:\"_deleted_at,omitempty\"`"),
        "{out}"
    );
    assert!(
        out.contains("DeletedAt_2 *string `json:\"deleted_at,omitempty\"`"),
        "{out}"
    );
    assert!(
        out.contains("DraftStatus *string `json:\"_status,omitempty\"`"),
        "{out}"
    );
    assert!(
        out.contains("DraftStatus_2 *string `json:\"draft_status,omitempty\"`"),
        "{out}"
    );
    assert!(
        out.contains("Collection *string `json:\"collection,omitempty\"`"),
        "{out}"
    );
}

/// Regression: the populated `collection` tag claimed the `Collection`
/// member before the user fields, so a field whose member is also
/// `Collection` was renamed by a generated key. The tag yields: the field
/// keeps its name, the tag member is suffixed and keeps its json key.
#[test]
fn go_collection_tag_never_renames_a_field() {
    for field_name in ["Collection", "collection_"] {
        let col = make_col("posts", vec![text_field(field_name, false)]);
        let mut out = String::new();
        render_collection(&mut out, &col);

        assert!(
            out.contains(&format!(
                "Collection *string `json:\"{field_name},omitempty\"`"
            )),
            "{field_name}: {out}"
        );
        assert!(
            out.contains("Collection_2 *string `json:\"collection,omitempty\"`"),
            "{field_name}: {out}"
        );
    }
}

/// Regression: a collection slugged `rel` declared `type Rel struct` beside
/// the prelude's generic `Rel[T]`, and a select constant could take a
/// struct type's name (`PostsStatusMeta`); neither compiled. The type is
/// renamed, every reference follows it, and a constant yields.
#[test]
fn go_package_level_names_never_collide() {
    let rel = make_col("rel", vec![text_field("name", false)]);
    let posts = make_col(
        "posts",
        vec![
            FieldDefinition::builder("status", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Meta".into()),
                    "meta",
                )])
                .build(),
            FieldDefinition::builder("status_meta", FieldType::Group)
                .fields(vec![text_field("note", false)])
                .build(),
            FieldDefinition::builder("owner", FieldType::Relationship)
                .relationship(RelationshipConfig::new("rel", false))
                .build(),
        ],
    );

    let mut registry = Registry::new();
    registry.register_collection(rel);
    registry.register_collection(posts);
    let out = render(&registry);

    assert!(out.contains("type Rel_ struct {"), "{out}");
    assert!(out.contains("*Rel[Rel_]"), "{out}");
    assert!(out.contains("type PostsStatusMeta struct {"), "{out}");
    assert!(
        out.contains("PostsStatusMeta_2 PostsStatus = \"meta\""),
        "{out}"
    );
}

/// A relational array row's `id` is spelled `ID`, as a document's is.
#[test]
fn go_array_rows_spell_their_id_as_id() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
            ])
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);

    assert!(
        out.contains("ID        *string `json:\"id,omitempty\"`"),
        "{out}"
    );
    assert!(!out.contains("\tId "), "{out}");
}

/// Regression: a boolean and a single group were plain values on the read
/// structs, so an absent field decoded like `false` or an empty group.
#[test]
fn go_optional_bool_and_group_are_pointers() {
    let mut col = CollectionDefinition::new("posts");
    col.fields = vec![
        FieldDefinition::builder("active", FieldType::Checkbox).build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("enabled", FieldType::Checkbox).build(),
            ])
            .build(),
    ];
    let mut out = String::new();
    render_collection(&mut out, &col);

    assert!(out.contains("*bool `json:\"active,omitempty\"`"), "{out}");
    assert!(out.contains("*PostsSeo `json:\"seo,omitempty\"`"), "{out}");
    assert!(out.contains("*bool `json:\"enabled,omitempty\"`"), "{out}");
}

fn render_collection(out: &mut String, col: &CollectionDefinition) {
    let mut r = Registry::new();
    r.register_collection(col.clone());
    out.push_str(&render(&r));
}

fn render_global(out: &mut String, global: &GlobalDefinition) {
    let mut r = Registry::new();
    r.register_global(global.clone());
    out.push_str(&render(&r));
}

fn text_field(name: &str, required: bool) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text)
        .required(required)
        .build()
}

fn make_col(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.timestamps = true;
    def.fields = fields;
    def
}

/// Identifier safety: a digit-leading slug (→ uppercase-start type), two
/// distinct names that `PascalCase` to the same Go identifier, and a field
/// that collides with the fixed `ID` — all disambiguated; json tags keep the
/// raw wire keys.
#[test]
fn go_sanitizes_collisions_and_leading_digit() {
    let col = make_col(
        "2fa",
        vec![
            text_field("first_name", true),
            text_field("firstName", false),
            text_field("iD", false),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);

    assert!(
        out.contains("type N2fa struct {"),
        "leading-digit type: {out}"
    );
    assert!(
        out.contains("\tFirstName "),
        "first_name → FirstName: {out}"
    );
    assert!(
        out.contains("FirstName_2 "),
        "collision disambiguated: {out}"
    );
    assert!(out.contains("ID_2 "), "collides with fixed ID: {out}");
    assert!(
        out.contains("json:\"firstName"),
        "wire key preserved: {out}"
    );
}

#[test]
fn go_collection_output() {
    let col = make_col(
        "posts",
        vec![text_field("title", true), text_field("content", false)],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);

    assert!(out.contains("type Posts struct {"));
    assert!(out.contains("ID        string  `json:\"id\"`"));
    assert!(out.contains("Title     *string `json:\"title,omitempty\"`"));
    assert!(out.contains("Content   *string `json:\"content,omitempty\"`"));
    assert!(out.contains("CreatedAt *string `json:\"created_at,omitempty\"`"));
}

#[test]
fn go_relationship_field_has_one() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("Author    *Rel[Users] `json:\"author,omitempty\"`"),
        "populated relationship uses Rel[T], optional on read: {out}"
    );
}

#[test]
fn go_polymorphic_has_one() {
    let mut rc = RelationshipConfig::new("posts", false);
    rc.polymorphic = vec!["posts".into(), "pages".into()];
    let col = make_col(
        "comments",
        vec![
            FieldDefinition::builder("subject", FieldType::Relationship)
                .required(true)
                .relationship(rc)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    // Go has no union type, so a polymorphic value decodes to interface{}.
    assert!(
        out.contains("Subject   interface{} `json:\"subject,omitempty\"`"),
        "poly has-one interface{{}}: {out}"
    );
    assert!(out.contains("Polymorphic relationship"), "comment: {out}");
}

#[test]
fn go_polymorphic_has_many() {
    let mut rc = RelationshipConfig::new("articles", true);
    rc.polymorphic = vec!["articles".into(), "videos".into()];
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(rc)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("[]interface{}"),
        "poly has-many []interface{{}}: {out}"
    );
    assert!(out.contains("Polymorphic relationship"), "comment: {out}");
}

#[test]
fn go_relationship_field_has_many() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("[]Rel[Tags]"), "has-many populated rel: {out}");
}

#[test]
fn go_number_field() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("price", FieldType::Number)
                .required(true)
                .build(),
            FieldDefinition::builder("discount", FieldType::Number).build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("Price     *float64 `json:\"price,omitempty\"`"));
    assert!(out.contains("*float64"));
}

#[test]
fn go_checkbox_field() {
    let col = make_col(
        "items",
        vec![FieldDefinition::builder("active", FieldType::Checkbox).build()],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("bool"));
}

#[test]
fn go_json_field() {
    let col = make_col(
        "items",
        vec![FieldDefinition::builder("metadata", FieldType::Json).build()],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("interface{}"));
}

#[test]
fn go_array_field_with_subfields() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label", true), text_field("value", false)])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("type PostsItems struct {"));
    assert!(out.contains("Label     *string `json:\"label,omitempty\"`"));
    assert!(out.contains("[]PostsItems"));
}

#[test]
fn go_array_field_without_subfields() {
    let col = make_col(
        "posts",
        vec![FieldDefinition::builder("data", FieldType::Array).build()],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("[]map[string]interface{}"));
}

#[test]
fn go_group_field_empty() {
    let col = make_col(
        "posts",
        vec![FieldDefinition::builder("seo", FieldType::Group).build()],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("map[string]interface{}"));
}

#[test]
fn go_group_field_with_subfields() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    text_field("title", true),
                    text_field("description", false),
                ])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("type PostsSeo struct {"), "sub-type: {out}");
    assert!(out.contains("Title"), "sub-field: {out}");
    assert!(out.contains("PostsSeo"), "reference: {out}");
}

#[test]
fn go_array_nested_in_row() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("items", FieldType::Array)
                        .fields(vec![text_field("label", true)])
                        .build(),
                ])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("type PostsItems struct {"),
        "array nested in Row should emit sub-type: {out}"
    );
}

#[test]
fn go_global_with_subtypes() {
    let mut global = GlobalDefinition::new("settings");
    global.fields = vec![
        FieldDefinition::builder("nav", FieldType::Array)
            .fields(vec![text_field("label", true)])
            .build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![text_field("title", true)])
            .build(),
    ];
    let mut out = String::new();
    render_global(&mut out, &global);
    assert!(
        out.contains("type SettingsNav struct {"),
        "array sub: {out}"
    );
    assert!(
        out.contains("type SettingsSeo struct {"),
        "group sub: {out}"
    );
}

#[test]
fn go_blocks_field() {
    let mut bd = BlockDefinition::new("text", vec![text_field("body", true)]);
    bd.label = Some(LocalizedString::Plain("Text".to_string()));
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![bd])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("[]map[string]interface{}"));
}

#[test]
fn go_upload_field() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .required(true)
                .build(),
            FieldDefinition::builder("thumb", FieldType::Upload).build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    // Single upload is optional on read (populate can null it) → *string.
    assert!(
        out.contains("Image     *string `json:\"image,omitempty\"`"),
        "got: {out}"
    );
    assert!(out.contains("*string"));
}

#[test]
fn upload_has_many_generates_array_type() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("images", FieldType::Upload)
                .required(true)
                .relationship(RelationshipConfig::new("", true))
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("[]string"),
        "has-many upload → []string: {out}"
    );
}

#[test]
fn go_select_field() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("status", FieldType::Select)
                .required(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Draft".into()), "draft"),
                    SelectOption::new(LocalizedString::Plain("Published".into()), "published"),
                ])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    // Select references the generated named string type.
    assert!(
        out.contains("Status    *PostsStatus `json:\"status,omitempty\"`"),
        "select → named type: {out}"
    );
    assert!(out.contains("type PostsStatus string"), "{out}");
    assert!(
        out.contains("PostsStatusDraft PostsStatus = \"draft\""),
        "const for option value: {out}"
    );
    assert!(
        out.contains("PostsStatusPublished PostsStatus = \"published\""),
        "{out}"
    );
}

#[test]
fn go_global_output() {
    let mut global = GlobalDefinition::new("site_settings");
    global.fields = vec![text_field("site_name", true)];
    let mut out = String::new();
    render_global(&mut out, &global);
    assert!(out.contains("type SiteSettings struct {"));
    assert!(out.contains("ID        string  `json:\"id\"`"));
    assert!(out.contains("SiteName  *string `json:\"site_name,omitempty\"`"));
    assert!(out.contains("CreatedAt *string `json:\"created_at,omitempty\"`"));
    assert!(out.contains("UpdatedAt *string `json:\"updated_at,omitempty\"`"));
}

#[test]
fn go_full_render_with_globals() {
    let mut registry = Registry::new();
    registry.register_collection(make_col("posts", vec![text_field("title", true)]));
    let mut settings = GlobalDefinition::new("settings");
    settings.fields = vec![text_field("name", true)];
    registry.register_global(settings);
    let out = render(&registry);
    assert!(out.contains("package types"));
    assert!(out.contains("type Posts struct {"));
    assert!(out.contains("type Settings struct {"));
}

#[test]
fn go_collection_slug_consts() {
    let mut registry = Registry::new();
    registry.register_collection(make_col("posts", vec![text_field("title", true)]));
    registry.register_collection(make_col("pages", vec![text_field("body", true)]));
    let out = render(&registry);
    assert!(out.contains("type CollectionSlug string"), "got: {out}");
    assert!(
        out.contains("CollectionSlugPosts CollectionSlug = \"posts\""),
        "got: {out}"
    );
    assert!(
        out.contains("CollectionSlugPages CollectionSlug = \"pages\""),
        "got: {out}"
    );
}

#[test]
fn go_no_collection_slug_when_empty() {
    let out = render(&Registry::new());
    assert!(!out.contains("CollectionSlug"));
}

#[test]
fn go_no_timestamps_collection() {
    let mut col = make_col("tags", vec![text_field("name", true)]);
    col.timestamps = false;
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("type Tags struct {"));
    assert!(!out.contains("CreatedAt"));
    assert!(!out.contains("UpdatedAt"));
}

#[test]
fn go_text_has_many() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .required(true)
                .build(),
            FieldDefinition::builder("labels", FieldType::Text)
                .has_many(true)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("[]string"), "has-many text → []string: {out}");
    assert!(out.contains("Tags      []string"), "required field: {out}");
    assert!(out.contains("Labels    []string"), "optional field: {out}");
}

#[test]
fn go_number_has_many() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .required(true)
                .build(),
            FieldDefinition::builder("weights", FieldType::Number)
                .has_many(true)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("[]float64"), "has-many number: {out}");
    assert!(out.contains("Scores    []float64"), "required field: {out}");
    assert!(out.contains("Weights   []float64"), "optional field: {out}");
}

#[test]
fn go_email_date_richtext_textarea_fields() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("contact", FieldType::Email)
                .required(true)
                .build(),
            FieldDefinition::builder("published_at", FieldType::Date).build(),
            FieldDefinition::builder("body", FieldType::Richtext)
                .required(true)
                .build(),
            FieldDefinition::builder("notes", FieldType::Textarea).build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("Contact   *string `json:\"contact,omitempty\"`"));
    assert!(out.contains("*string `json:\"published_at,omitempty\"`"));
    assert!(out.contains("Body      *string `json:\"body,omitempty\"`"));
    assert!(out.contains("*string `json:\"notes,omitempty\"`"));
}

#[test]
fn go_relationship_no_config_optional() {
    let col = make_col(
        "posts",
        vec![FieldDefinition::builder("ref", FieldType::Relationship).build()],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("*string"));
}

#[test]
fn go_code_join_radio_fields() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("snippet", FieldType::Code)
                .required(true)
                .build(),
            FieldDefinition::builder("refs", FieldType::Join).build(),
            FieldDefinition::builder("color", FieldType::Radio)
                .required(true)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("Snippet   *string `json:\"snippet,omitempty\"`"),
        "code → string: {out}"
    );
    assert!(out.contains("[]map[string]interface{}"), "join: {out}");
    assert!(
        out.contains("Color     *string `json:\"color,omitempty\"`"),
        "radio → string: {out}"
    );
}

#[test]
fn go_select_has_many() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .required(true)
                .build(),
            FieldDefinition::builder("sizes", FieldType::Radio)
                .has_many(true)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("Tags      []string"),
        "required select has-many: {out}"
    );
    assert!(
        out.contains("Sizes     []string"),
        "optional radio has-many: {out}"
    );
}

#[test]
fn go_polymorphic_has_one_optional() {
    let mut rc = RelationshipConfig::new("pages", false);
    rc.polymorphic = vec!["pages".into(), "posts".into()];
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .required(false)
                .relationship(rc)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("Related   interface{} `json:\"related,omitempty\"`"),
        "poly has-one → interface{{}}: {out}"
    );
}

#[test]
fn go_row_collapsible_tabs_promote_subfields() {
    let col = make_col(
        "items",
        vec![
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
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(!out.contains("LayoutRow"), "row name not a field: {out}");
    assert!(out.contains("FirstName *string"), "row sub: {out}");
    assert!(out.contains("LastName  *string"), "row optional sub: {out}");
    assert!(
        !out.contains("Details"),
        "collapsible name not a field: {out}"
    );
    assert!(out.contains("Bio       *string"), "collapsible sub: {out}");
    assert!(!out.contains("Sections"), "tabs name not a field: {out}");
    assert!(out.contains("TabField  *string"), "tabs sub: {out}");
}
