//! Tests for the embedded-document read pass.

use anyhow::{Result, bail};
use serde_json::json;

use super::*;
use crate::{
    core::{
        CollectionDefinition, FieldType, HookRef, Hooks, JoinConfig, RelationshipConfig, ReqContext,
    },
    db::{AccessResult, query::populate::document_to_json},
    hooks::{
        AccessCheckInput,
        lifecycle::{AfterReadCtx, access::strip_read_access_data_aware},
    },
    service::FieldReadStrip,
};

/// Mock that denies reads on any field carrying an `access.read` hook —
/// mirroring the real `field_read_denied`, but without a live Lua VM.
struct DenyAccessReadHooks;

impl ReadHooks for DenyAccessReadHooks {
    fn before_read(
        &self,
        _hooks: &Hooks,
        _slug: &str,
        _op: &str,
        _locale: Option<&str>,
    ) -> Result<ReqContext> {
        Ok(ReqContext::new())
    }

    fn after_read_one(&self, _ctx: &AfterReadCtx, doc: Document) -> Document {
        doc
    }

    fn check_access(&self, _input: &AccessCheckInput<'_>) -> Result<AccessResult> {
        Ok(AccessResult::Allowed)
    }
}

impl FieldReadStrip for DenyAccessReadHooks {
    /// Data-aware strip mock: deny every field carrying an `access.read`
    /// hook, at any depth (mirrors the real walker with a constant-deny rule).
    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        _document: &DocumentFields,
        _collection: &str,
        _user: Option<&Document>,
        _locale: Option<&str>,
    ) {
        strip_read_access_data_aware(fields, level, &|_hook, _data| true);
    }
}

fn text(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text).build()
}

/// A text field denied to readers via an `access.read` hook.
fn denied(name: &str) -> FieldDefinition {
    let mut f = FieldDefinition::builder(name, FieldType::Text).build();
    f.access.read = Some(HookRef::new("deny"));
    f
}

fn rel(name: &str, target: &str, has_many: bool) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Relationship)
        .relationship(RelationshipConfig::new(target, has_many))
        .build()
}

fn collection(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.fields = fields;
    def
}

/// `authors` has a reader-denied `secret`; `posts.author` points at it.
fn posts_and_authors() -> Registry {
    let authors = collection(
        "authors",
        vec![
            text("name"),
            denied("secret"),
            rel("mentor", "authors", false),
        ],
    );
    let posts = collection(
        "posts",
        vec![text("title"), rel("author", "authors", false)],
    );

    let mut registry = Registry::new();
    registry.register_collection(authors);
    registry.register_collection(posts);
    registry
}

/// The pass for an anonymous `find` read.
fn pass<'a>(registry: &'a Registry, hooks: &'a dyn ReadHooks) -> EmbeddedDocPass<'a> {
    EmbeddedDocPass::new(EmbeddedReader::builder(registry, hooks, "find").build())
}

fn strip_post(registry: &Registry, doc: &mut Document) {
    let hooks = DenyAccessReadHooks;
    pass(registry, &hooks).process(
        doc,
        &registry.get_collection("posts").unwrap().fields.clone(),
    );
}

#[test]
fn strips_denied_field_from_populated_has_one_target() {
    let registry = posts_and_authors();
    let mut doc = Document::new("p1".to_string());
    doc.fields.insert(
        "author".into(),
        json!({ "id": "a1", "collection": "authors", "name": "Ada", "secret": "xyz" }),
    );

    strip_post(&registry, &mut doc);

    let author = doc.fields.get("author").unwrap();
    assert_eq!(author.get("name").and_then(|v| v.as_str()), Some("Ada"));
    assert!(
        author.get("secret").is_none(),
        "reader-denied field on the target collection must not leak via populate"
    );
}

#[test]
fn strips_denied_field_from_every_populated_has_many_target() {
    let registry = posts_and_authors();
    // Reuse the has-one fixture but as a has-many `author` value.
    let mut doc = Document::new("p1".to_string());
    doc.fields.insert(
        "author".into(),
        json!([
            { "id": "a1", "collection": "authors", "name": "Ada", "secret": "x" },
            "a2",
            { "id": "a3", "collection": "authors", "name": "Bo", "secret": "y" }
        ]),
    );

    strip_post(&registry, &mut doc);

    let arr = doc.fields.get("author").unwrap().as_array().unwrap();
    assert!(arr[0].get("secret").is_none(), "first target stripped");
    assert_eq!(arr[1].as_str(), Some("a2"), "unpopulated id left untouched");
    assert!(arr[2].get("secret").is_none(), "third target stripped");
}

#[test]
fn strips_denied_field_at_depth_two() {
    let registry = posts_and_authors();
    let mut doc = Document::new("p1".to_string());
    doc.fields.insert(
        "author".into(),
        json!({
            "id": "a1", "collection": "authors", "name": "Ada", "secret": "x",
            "mentor": { "id": "a2", "collection": "authors", "name": "Bo", "secret": "deep" }
        }),
    );

    strip_post(&registry, &mut doc);

    let author = doc.fields.get("author").unwrap();
    assert!(author.get("secret").is_none(), "depth-1 secret stripped");
    let mentor = author.get("mentor").unwrap();
    assert_eq!(mentor.get("name").and_then(|v| v.as_str()), Some("Bo"));
    assert!(
        mentor.get("secret").is_none(),
        "depth-2 nested populated target's secret must also be stripped"
    );
}

/// Regression: a JOIN-typed field's populated targets must also be stripped.
/// Join was missing from the strip set, so a reader-denied field on the join
/// target leaked while the identical field on a relationship target did not.
#[test]
fn strips_denied_field_from_populated_join_target() {
    let posts = collection(
        "posts",
        vec![
            text("title"),
            denied("secret"),
            rel("author", "authors", false),
        ],
    );
    let mut authors = collection("authors", vec![text("name")]);
    authors.fields.push(
        FieldDefinition::builder("recent_posts", FieldType::Join)
            .join(JoinConfig::new("posts", "author"))
            .build(),
    );
    let mut registry = Registry::new();
    registry.register_collection(posts);
    registry.register_collection(authors);

    let mut doc = Document::new("au1".to_string());
    doc.fields.insert(
        "recent_posts".into(),
        json!([
            { "id": "p1", "collection": "posts", "title": "A", "secret": "x", "author": "au1" },
            { "id": "p2", "collection": "posts", "title": "B", "secret": "y", "author": "au1" }
        ]),
    );

    let hooks = DenyAccessReadHooks;
    pass(&registry, &hooks).process(
        &mut doc,
        &registry.get_collection("authors").unwrap().fields.clone(),
    );

    let arr = doc.fields.get("recent_posts").unwrap().as_array().unwrap();
    assert_eq!(arr[0].get("title").and_then(|v| v.as_str()), Some("A"));
    assert!(
        arr[0].get("secret").is_none(),
        "reader-denied field on a JOIN target must not leak via populate"
    );
    assert!(
        arr[1].get("secret").is_none(),
        "every join target must be stripped"
    );
}

/// Security: a Join nested inside a Group (now populated) must still have
/// its target's reader-denied fields stripped — the strip pass descends the
/// group and treats the Join leaf as an embedding type.
#[test]
fn strips_denied_field_from_join_nested_in_group() {
    let posts = collection(
        "posts",
        vec![
            text("title"),
            denied("secret"),
            rel("author", "authors", false),
        ],
    );
    let mut authors = collection("authors", vec![]);
    let mut group = FieldDefinition::builder("section", FieldType::Group).build();
    group.fields = vec![
        FieldDefinition::builder("recent", FieldType::Join)
            .join(JoinConfig::new("posts", "author"))
            .build(),
    ];
    authors.fields.push(group);
    let mut registry = Registry::new();
    registry.register_collection(posts);
    registry.register_collection(authors);

    let mut doc = Document::new("au1".to_string());
    doc.fields.insert(
        "section".into(),
        json!({
            "recent": [
                {
                    "id": "p1", "collection": "posts", "title": "A", "secret": "x",
                    "author": "au1"
                }
            ]
        }),
    );

    let hooks = DenyAccessReadHooks;
    pass(&registry, &hooks).process(
        &mut doc,
        &registry.get_collection("authors").unwrap().fields.clone(),
    );

    let arr = doc
        .fields
        .get("section")
        .and_then(|s| s.get("recent"))
        .and_then(|v| v.as_array())
        .expect("nested join array present");
    assert_eq!(arr[0].get("title").and_then(|v| v.as_str()), Some("A"));
    assert!(
        arr[0].get("secret").is_none(),
        "a denied field on a Join target nested in a group must be stripped"
    );
}

/// Read hooks whose strip denies a `public_only` rule unless the field's
/// own level holds `public = true`, and every other rule outright.
struct PublicOnlyReadHooks;

impl ReadHooks for PublicOnlyReadHooks {
    fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
        Ok(ReqContext::new())
    }

    fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
        doc
    }

    fn check_access(&self, _: &AccessCheckInput<'_>) -> Result<AccessResult> {
        Ok(AccessResult::Allowed)
    }
}

impl FieldReadStrip for PublicOnlyReadHooks {
    fn strip_read_access_map(
        &self,
        fields: &[FieldDefinition],
        level: &mut Map<String, Value>,
        _document: &DocumentFields,
        _collection: &str,
        _user: Option<&Document>,
        _locale: Option<&str>,
    ) {
        strip_read_access_data_aware(fields, level, &|hook, data| {
            hook.reference() != "public_only" || data.get("public") != Some(&json!(true))
        });
    }
}

/// `authors.recent_posts` joins `posts` on `author`, whose read rule is
/// `rule` (or which is `hidden` when `rule` is `None`).
fn join_on_gated_author(rule: Option<&str>) -> Registry {
    let mut author = rel("author", "authors", false);
    match rule {
        Some(rule) => author.access.read = Some(HookRef::new(rule)),
        None => author.hidden = true,
    }

    let posts = collection("posts", vec![text("title"), text("public"), author]);
    let mut authors = collection("authors", vec![text("name")]);
    authors.fields.push(
        FieldDefinition::builder("recent_posts", FieldType::Join)
            .join(JoinConfig::new("posts", "author"))
            .build(),
    );

    let mut registry = Registry::new();
    registry.register_collection(posts);
    registry.register_collection(authors);
    registry
}

/// Strip `au1`'s populated `recent_posts` (children `p1` public, `p2` not)
/// and return the ids left in the join.
fn joined_ids_after_strip(registry: &Registry, hooks: &dyn ReadHooks) -> Vec<String> {
    let mut doc = Document::new("au1".to_string());
    doc.fields.insert(
        "recent_posts".into(),
        json!([
            { "id": "p1", "collection": "posts", "title": "A", "public": true,
              "author": "au1" },
            { "id": "p2", "collection": "posts", "title": "B", "public": false,
              "author": "au1" }
        ]),
    );

    pass(registry, hooks).process(
        &mut doc,
        &registry.get_collection("authors").unwrap().fields.clone(),
    );

    doc.fields
        .get("recent_posts")
        .and_then(Value::as_array)
        .expect("join array present")
        .iter()
        .filter_map(|child| child.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Regression: a join listed its target documents even when the viewer
/// could not read their `on` field — the strip removed the value, but the
/// child's membership in the join still revealed it.
#[test]
fn join_drops_children_whose_on_field_is_read_denied() {
    let registry = join_on_gated_author(Some("deny"));

    assert!(joined_ids_after_strip(&registry, &DenyAccessReadHooks).is_empty());
}

#[test]
fn join_drops_children_whose_on_field_is_hidden() {
    let registry = join_on_gated_author(None);

    assert!(joined_ids_after_strip(&registry, &PublicOnlyReadHooks).is_empty());
}

/// The `on` rule is judged per child, on that child's data.
#[test]
fn join_keeps_exactly_the_children_whose_on_field_is_readable() {
    let registry = join_on_gated_author(Some("public_only"));

    assert_eq!(
        joined_ids_after_strip(&registry, &PublicOnlyReadHooks),
        vec!["p1".to_string()]
    );
}

#[test]
fn join_child_readable_follows_the_on_key() {
    let join = JoinConfig::new("posts", "author");

    let with_on: Map<String, Value> = json!({ "id": "p1", "author": "au1" })
        .as_object()
        .cloned()
        .unwrap();
    let without_on: Map<String, Value> = json!({ "id": "p1" }).as_object().cloned().unwrap();

    assert!(join_child_readable(&join, &with_on));
    assert!(!join_child_readable(&join, &without_on));
}

#[test]
fn embedded_doc_without_read_controls_is_left_intact() {
    let authors = collection("authors", vec![text("name")]);
    let posts = collection("posts", vec![rel("author", "authors", false)]);
    let mut registry = Registry::new();
    registry.register_collection(authors);
    registry.register_collection(posts);

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert(
        "author".into(),
        json!({ "id": "a1", "collection": "authors", "name": "Ada" }),
    );

    strip_post(&registry, &mut doc);

    assert_eq!(
        doc.fields.get("author"),
        Some(&json!({ "id": "a1", "collection": "authors", "name": "Ada" }))
    );
}

/// `products` with a reader-denied `cost_price` and a `hidden` `internal`.
fn products() -> CollectionDefinition {
    let mut internal = text("internal");
    internal.hidden = true;

    collection(
        "products",
        vec![text("title"), denied("cost_price"), internal],
    )
}

/// `orders.product` → `products`, plus `authors` (no read controls).
fn orders_and_products() -> Registry {
    let orders = collection("orders", vec![rel("product", "products", false)]);

    let mut registry = Registry::new();
    registry.register_collection(products());
    registry.register_collection(orders);
    registry.register_collection(collection("authors", vec![text("name")]));
    registry
}

fn process_order(registry: &Registry, doc: &mut Document) {
    let hooks = DenyAccessReadHooks;
    pass(registry, &hooks).process(
        doc,
        &registry.get_collection("orders").unwrap().fields.clone(),
    );
}

/// Regression: a target document with its own `collection` field
/// overwrote the populate tag, and the strip read the target collection
/// from that value — a value naming no collection skipped the strip, one
/// naming another collection stripped against the wrong rules. The tag is
/// the server's and the collection comes from the field definition.
#[test]
fn a_collection_value_in_the_target_cannot_redirect_the_strip() {
    let registry = orders_and_products();

    for value in ["summer", "authors"] {
        let mut product = Document::new("pr1".to_string());
        product.fields.insert("collection".into(), json!(value));
        product.fields.insert("title".into(), json!("Hat"));
        product.fields.insert("cost_price".into(), json!(3));
        product.fields.insert("internal".into(), json!("x"));

        let mut doc = Document::new("o1".to_string());
        doc.fields.insert(
            "product".into(),
            document_to_json(&product, Some("products")),
        );

        process_order(&registry, &mut doc);

        let product = doc.fields.get("product").unwrap();
        assert_eq!(product["collection"], json!("products"), "{value}");
        assert_eq!(product["title"], json!("Hat"), "{value}");
        assert!(product.get("cost_price").is_none(), "{value}: read-denied");
        assert!(product.get("internal").is_none(), "{value}: hidden");
    }
}

/// An embedded document tagged with a collection the field does not
/// target is dropped (fail closed), never passed through unstripped.
#[test]
fn an_embedded_doc_outside_the_field_targets_is_dropped() {
    let registry = orders_and_products();

    let mut doc = Document::new("o1".to_string());
    doc.fields.insert(
        "product".into(),
        json!({ "id": "a1", "collection": "authors", "cost_price": 3 }),
    );

    process_order(&registry, &mut doc);

    assert_eq!(doc.fields.get("product"), Some(&Value::Null));

    let mut doc = Document::new("o2".to_string());
    doc.fields.insert(
        "product".into(),
        json!({ "id": "x1", "collection": "nowhere", "cost_price": 3 }),
    );

    process_order(&registry, &mut doc);

    assert_eq!(doc.fields.get("product"), Some(&Value::Null));
}

/// A polymorphic target resolves within the field's allowlist: each item is
/// stripped against its own collection, and an item tagged outside the
/// list is dropped.
#[test]
fn polymorphic_targets_resolve_within_the_field_allowlist() {
    let mut related = rel("related", "products", true);
    if let Some(rc) = related.relationship.as_mut() {
        rc.polymorphic = vec!["products".into(), "pages".into()];
    }

    let mut registry = Registry::new();
    registry.register_collection(products());
    registry.register_collection(collection("pages", vec![text("title"), denied("draft")]));
    registry.register_collection(collection("authors", vec![text("name")]));
    registry.register_collection(collection("entries", vec![related]));

    let mut doc = Document::new("e1".to_string());
    doc.fields.insert(
        "related".into(),
        json!([
            { "id": "pr1", "collection": "products", "title": "Hat", "cost_price": 3 },
            { "id": "pg1", "collection": "pages", "title": "About", "draft": "wip" },
            { "id": "a1", "collection": "authors", "name": "Ada" },
            "pages/pg2"
        ]),
    );

    let hooks = DenyAccessReadHooks;
    pass(&registry, &hooks).process(
        &mut doc,
        &registry.get_collection("entries").unwrap().fields.clone(),
    );

    let items = doc.fields.get("related").unwrap().as_array().unwrap();
    assert_eq!(items.len(), 3, "the out-of-list item is dropped");
    assert_eq!(items[0]["title"], json!("Hat"));
    assert!(items[0].get("cost_price").is_none());
    assert_eq!(items[1]["title"], json!("About"));
    assert!(items[1].get("draft").is_none());
    assert_eq!(items[2], json!("pages/pg2"), "unpopulated ref untouched");
}

/// Read hooks that stamp every `after_read` document with the collection
/// and operation its hooks ran for, and whose `before_read` refuses reads
/// of the `refused` collection.
struct RecordingReadHooks {
    refused: &'static str,
}

impl ReadHooks for RecordingReadHooks {
    fn before_read(&self, _: &Hooks, slug: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
        if slug == self.refused {
            bail!("reads of {slug} refused");
        }

        Ok(ReqContext::new())
    }

    fn after_read_one(&self, ctx: &AfterReadCtx, mut doc: Document) -> Document {
        doc.fields.insert(
            "read_as".into(),
            json!(format!("{}:{}", ctx.collection, ctx.operation)),
        );
        doc
    }

    fn check_access(&self, _: &AccessCheckInput<'_>) -> Result<AccessResult> {
        Ok(AccessResult::Allowed)
    }
}

impl FieldReadStrip for RecordingReadHooks {
    fn strip_read_access_map(
        &self,
        _: &[FieldDefinition],
        _: &mut Map<String, Value>,
        _: &DocumentFields,
        _: &str,
        _: Option<&Document>,
        _: Option<&str>,
    ) {
    }
}

/// A post whose author (and the author's mentor) are populated.
fn post_with_author_chain() -> Document {
    let mut doc = Document::new("p1".to_string());
    doc.fields.insert(
        "author".into(),
        json!({
            "id": "a1", "collection": "authors", "name": "Ada",
            "mentor": { "id": "a2", "collection": "authors", "name": "Bo" }
        }),
    );
    doc
}

/// Regression: a populated document skipped its own collection's read
/// hooks — an `after_read` transform (or computed field) was missing from
/// every embedded copy. They run with the embedding read's operation, at
/// every depth, and the tag survives them.
#[test]
fn target_after_read_runs_on_embedded_docs_at_every_depth() {
    let registry = posts_and_authors();
    let hooks = RecordingReadHooks { refused: "" };
    let mut doc = post_with_author_chain();

    pass(&registry, &hooks).process(
        &mut doc,
        &registry.get_collection("posts").unwrap().fields.clone(),
    );

    let author = doc.fields.get("author").unwrap();
    assert_eq!(author["read_as"], json!("authors:find"));
    assert_eq!(author["collection"], json!("authors"));
    assert_eq!(author["mentor"]["read_as"], json!("authors:find"));
    assert_eq!(author["mentor"]["id"], json!("a2"));
}

/// Regression: a `before_read` that aborts reads of a collection was
/// side-stepped by any relationship pointing into it. Its embedded
/// documents are now hidden, as a denied read hides them.
#[test]
fn target_before_read_refusal_hides_its_embedded_docs() {
    let registry = posts_and_authors();
    let hooks = RecordingReadHooks { refused: "authors" };
    let mut doc = post_with_author_chain();

    pass(&registry, &hooks).process(
        &mut doc,
        &registry.get_collection("posts").unwrap().fields.clone(),
    );

    assert_eq!(doc.fields.get("author"), Some(&Value::Null));
}

/// An `authors` chain `levels` deep: each author's `mentor` is the next one,
/// every one carrying a reader-denied `secret`.
fn mentor_chain(levels: usize) -> Value {
    (0..levels).rev().fold(Value::Null, |mentor, level| {
        json!({
            "id": format!("a{level}"), "collection": "authors",
            "name": "n", "secret": "s", "mentor": mentor
        })
    })
}

/// Regression: past the pass's recursion bound the embedded documents were
/// returned untouched — their denied fields included — to a read populated
/// deeper than the bound (`[depth] max_depth` has no ceiling). An embedded
/// document the pass cannot process is dropped instead.
#[test]
fn embedded_docs_beyond_the_recursion_bound_are_dropped_not_passed_through() {
    let registry = posts_and_authors();
    let mut doc = Document::new("p1".to_string());
    doc.fields
        .insert("author".into(), mentor_chain(MAX_EMBED_DEPTH + 3));

    strip_post(&registry, &mut doc);

    let mut level = doc.fields.get("author").unwrap();
    let mut processed = 0;

    while level.is_object() {
        assert!(level.get("secret").is_none(), "level {processed} stripped");
        processed += 1;
        level = &level["mentor"];
    }

    assert_eq!(processed, MAX_EMBED_DEPTH);
    assert_eq!(
        level,
        &Value::Null,
        "the first unprocessable level is dropped"
    );
}

/// [`RecordingReadHooks`] that also logs every `after_read_many` batch as
/// `(collection, size)`, and reports `after_read` hooks only when `hooked`.
struct BatchLog {
    inner: RecordingReadHooks,
    hooked: bool,
    batches: RefCell<Vec<(String, usize)>>,
}

impl BatchLog {
    fn new(hooked: bool) -> Self {
        Self {
            inner: RecordingReadHooks { refused: "" },
            hooked,
            batches: RefCell::new(Vec::new()),
        }
    }
}

impl ReadHooks for BatchLog {
    fn before_read(&self, h: &Hooks, slug: &str, op: &str, l: Option<&str>) -> Result<ReqContext> {
        self.inner.before_read(h, slug, op, l)
    }

    fn after_read_one(&self, ctx: &AfterReadCtx, doc: Document) -> Document {
        self.inner.after_read_one(ctx, doc)
    }

    fn after_read_many(&self, ctx: &AfterReadCtx, docs: Vec<Document>) -> Vec<Document> {
        self.batches
            .borrow_mut()
            .push((ctx.collection.to_string(), docs.len()));

        docs.into_iter()
            .map(|doc| self.after_read_one(ctx, doc))
            .collect()
    }

    fn wants_after_read(&self, _: &Hooks, _: &[FieldDefinition]) -> bool {
        self.hooked
    }

    fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
        self.inner.check_access(input)
    }
}

impl FieldReadStrip for BatchLog {
    fn strip_read_access_map(
        &self,
        _: &[FieldDefinition],
        _: &mut Map<String, Value>,
        _: &DocumentFields,
        _: &str,
        _: Option<&Document>,
        _: Option<&str>,
    ) {
    }
}

/// Regression: every embedded target ran its collection's `after_read` in a
/// call of its own — one Lua VM lease per target. They now run one batch per
/// collection per level across the whole read, deepest level first, with the
/// same result per target as before.
#[test]
fn embedded_after_read_runs_one_batch_per_collection_per_level() {
    let registry = posts_and_authors();
    let hooks = BatchLog::new(true);
    let mut docs = vec![post_with_author_chain(), post_with_author_chain()];

    pass(&registry, &hooks).process_many(
        &mut docs,
        &registry.get_collection("posts").unwrap().fields.clone(),
    );

    assert_eq!(
        *hooks.batches.borrow(),
        vec![("authors".to_string(), 2), ("authors".to_string(), 2)],
        "the mentors' batch, then the authors'"
    );

    for doc in &docs {
        let author = doc.fields.get("author").unwrap();
        assert_eq!(author["read_as"], json!("authors:find"));
        assert_eq!(author["mentor"]["read_as"], json!("authors:find"));
        assert_eq!(author["mentor"]["id"], json!("a2"));
        assert_eq!(author["collection"], json!("authors"));
    }
}

/// A read whose embedded collections have no `after_read` hooks runs no
/// batch and leaves the targets as processed.
#[test]
fn embedded_targets_without_after_read_hooks_run_no_batch() {
    let registry = posts_and_authors();
    let hooks = BatchLog::new(false);
    let mut doc = post_with_author_chain();

    pass(&registry, &hooks).process(
        &mut doc,
        &registry.get_collection("posts").unwrap().fields.clone(),
    );

    assert!(hooks.batches.borrow().is_empty());
    assert_eq!(doc.fields.get("author").unwrap()["name"], json!("Ada"));
}
