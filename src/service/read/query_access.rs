//! Which fields a read query may filter or sort on for the caller.
//!
//! A filter is an oracle: `where = { secret = { like = "a%" } }` recovers a
//! value the read strip would have removed, and a sort exposes its ordering.
//! Every read surface funnels through the service find/count/search, which ask
//! [`unreadable_query_paths`]; the admin list view asks it too.

use crate::{
    core::{CollectionDefinition, Document, FieldDefinition},
    db::FilterClause,
    service::{
        FieldReadStrip, ReadStripArgs, ServiceContext, ServiceError,
        helpers::{collect_api_hidden_field_names, strip_unreadable_docs},
    },
};

use super::query_probe::{ProbeStep, leaf_survives, probe_document, probe_shapes};

/// What a read query references by field: the filter paths plus the sort
/// column. Shared by the unreadable-field check across find/count/search.
pub struct QueryFieldRefs<'a> {
    pub filters: &'a [FilterClause],
    pub order_by: Option<&'a str>,
}

/// Reject a filter or sort on a field the caller may not read.
///
/// `_rank` is the one virtual sort and is skipped. The rule itself is
/// [`unreadable_query_paths`].
///
/// # Errors
///
/// Returns `AccessDenied` naming the first unreadable field, or the error from
/// resolving the context's read hooks / collection definition.
pub(crate) fn reject_unreadable_query_fields(
    ctx: &ServiceContext,
    locale: Option<&str>,
    refs: &QueryFieldRefs<'_>,
) -> Result<(), ServiceError> {
    let paths = query_field_paths(refs);

    match unreadable_query_paths(ctx, locale, &paths)?.first() {
        Some(path) => Err(unreadable(path)),
        None => Ok(()),
    }
}

/// Reject a filter on a field the caller may not read, judged through `strip`
/// — for a write that matches its documents by a filter (`update_many`,
/// `delete_many`) and so carries write hooks rather than read hooks. Its
/// match counts would otherwise probe a read-denied value just as a find
/// would. The rule is [`unreadable_query_paths`].
///
/// # Errors
///
/// Returns `AccessDenied` naming the first unreadable field, or the error from
/// resolving the collection definition.
pub(crate) fn reject_unreadable_filter_fields(
    ctx: &ServiceContext,
    strip: &dyn FieldReadStrip,
    locale: Option<&str>,
    filters: &[FilterClause],
) -> Result<(), ServiceError> {
    let paths = query_field_paths(&QueryFieldRefs {
        filters,
        order_by: None,
    });

    match unreadable_paths_by(ctx, strip, locale, &paths)?.first() {
        Some(path) => Err(unreadable(path)),
        None => Ok(()),
    }
}

/// The subset of `paths` (in order) the caller may not filter or sort on. Two
/// rules:
///
/// - **`hidden` fields** (API-hidden, at any depth) are never filterable or
///   sortable — static, user-independent.
/// - **Fields with an `access.read` rule** — the path's leaf or any container
///   on the way to it (a group, an array or block row, a nested array) — are
///   filterable/sortable only when every rule on the path allows without row
///   data. The path is probed in its real shape (groups as objects, arrays
///   and blocks as one row, one probe per block type holding the field) with a
///   `null` leaf, through the same strip that guards responses; the path is
///   unreadable when its leaf does not survive every probe. A data-dependent
///   rule that needs the row therefore denies (fail-closed).
///
/// The one predicate behind [`reject_unreadable_query_fields`]; the admin list
/// view asks it too, so it never offers a column, sort, or filter the read
/// would refuse.
///
/// # Errors
///
/// Returns the error from resolving the context's read hooks or collection
/// definition.
pub fn unreadable_query_paths(
    ctx: &ServiceContext,
    locale: Option<&str>,
    paths: &[String],
) -> Result<Vec<String>, ServiceError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    unreadable_paths_by(ctx, ctx.read_hooks()?, locale, paths)
}

/// [`unreadable_query_paths`] with the read strip supplied by the caller.
fn unreadable_paths_by(
    ctx: &ServiceContext,
    strip: &dyn FieldReadStrip,
    locale: Option<&str>,
    paths: &[String],
) -> Result<Vec<String>, ServiceError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let fields = ctx.fields()?;
    let readable = probe_readable(ctx, strip, locale, paths)?;

    Ok(paths
        .iter()
        .zip(readable)
        .filter(|(path, readable)| !readable || is_hidden_path(fields, path))
        .map(|(path, _)| path.clone())
        .collect())
}

/// For each of `paths`, whether its leaf survives the read strip in every
/// shape the path takes. All probes are stripped in one batch, so the hooks
/// evaluate them in one pass.
fn probe_readable(
    ctx: &ServiceContext,
    strip: &dyn FieldReadStrip,
    locale: Option<&str>,
    paths: &[String],
) -> Result<Vec<bool>, ServiceError> {
    let fields = ctx.fields()?;

    let shapes: Vec<Vec<Vec<ProbeStep>>> = paths
        .iter()
        .map(|path| probe_shapes(fields, path))
        .collect();

    let mut probes: Vec<Document> = shapes
        .iter()
        .flatten()
        .map(Vec::as_slice)
        .map(probe_document)
        .collect();

    let args = ReadStripArgs::builder(fields, ctx.slug)
        .user(ctx.user)
        .locale(locale)
        .build();

    strip_unreadable_docs(strip, &args, &mut probes);

    // Every probe of a path is consumed, so the next path reads its own.
    let mut stripped = probes.iter();

    Ok(shapes
        .iter()
        .map(|path_shapes| {
            let mut readable = true;

            for shape in path_shapes {
                readable &= stripped.next().is_some_and(|doc| leaf_survives(doc, shape));
            }

            readable
        })
        .collect())
}

/// Whether `path` is, or lies beneath, a `hidden` field — never filterable or
/// sortable for anyone. The static half of [`unreadable_query_paths`], exposed
/// for definition-time checks (an `admin.default_sort` on a hidden field).
#[must_use]
pub fn is_hidden_query_path(def: &CollectionDefinition, path: &str) -> bool {
    is_hidden_path(&def.fields, path)
}

/// Whether `path` is, or lies beneath, a `hidden` field of `fields`.
fn is_hidden_path(fields: &[FieldDefinition], path: &str) -> bool {
    collect_api_hidden_field_names(fields, "")
        .iter()
        .any(|d| denial_covers(&d.display_path(), path))
}

/// Every field path a query references: filter leaves (recursively through
/// AND/OR groups) and the sort column (minus the `-` prefix and `_rank`).
#[must_use]
pub fn query_field_paths(refs: &QueryFieldRefs<'_>) -> Vec<String> {
    let mut paths = Vec::new();
    for clause in refs.filters {
        collect_filter_paths(clause, &mut paths);
    }

    if let Some(order) = refs.order_by {
        let col = order.strip_prefix('-').unwrap_or(order);
        if col != "_rank" {
            paths.push(col.to_string());
        }
    }

    paths
}

fn collect_filter_paths(clause: &FilterClause, out: &mut Vec<String>) {
    match clause {
        FilterClause::Single(f) => out.push(f.field.clone()),
        FilterClause::And(subs) | FilterClause::Or(subs) => {
            for sub in subs {
                collect_filter_paths(sub, out);
            }
        }
    }
}

/// A denial at `denied` covers `path` when they name the same field or `path`
/// lies beneath it. A group's part of a path is spelled `seo.title` or
/// `seo__title` alike — a denial carries the flat form (`seo__links.url` for
/// an array inside a group), a query either — so both compare with every
/// group separator read as `.` (no field name holds `__`).
fn denial_covers(denied: &str, path: &str) -> bool {
    let denied = denied.replace("__", ".");
    let path = path.replace("__", ".");

    path == denied
        || path
            .strip_prefix(denied.as_str())
            .is_some_and(|rest| rest.starts_with('.'))
}

fn unreadable(path: &str) -> ServiceError {
    ServiceError::AccessDenied(format!(
        "Cannot filter or sort on '{path}': the field is not readable in this context"
    ))
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use serde_json::{Map, Value};

    use super::*;
    use crate::{
        core::{
            BlockDefinition, DocumentFields, FieldAccess, FieldDefinition, FieldType, HookRef,
            RelationshipConfig, ReqContext, collection::Hooks,
        },
        db::{AccessResult, Filter, FilterOp},
        hooks::lifecycle::{AccessCheckInput, AfterReadCtx, access::strip_read_access_data_aware},
        service::{FieldReadStrip, ReadHooks},
    };

    /// Read hooks whose field-read strip denies a `"deny"` rule outright and a
    /// `"public_only"` rule unless the field's own level (`ctx.data`) holds
    /// `public = true` — the data-aware strip, without a Lua VM.
    struct RuleStrip;

    impl ReadHooks for RuleStrip {
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

    impl FieldReadStrip for RuleStrip {
        fn strip_read_access_map(
            &self,
            fields: &[FieldDefinition],
            level: &mut Map<String, Value>,
            _document: &DocumentFields,
            _collection: &str,
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) {
            strip_read_access_data_aware(fields, level, &|hook, data| match hook.reference() {
                "deny" => true,
                "public_only" => data.get("public") != Some(&Value::Bool(true)),
                _ => false,
            });
        }
    }

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn hidden_text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .hidden(true)
            .build()
    }

    fn gated(mut field: FieldDefinition, rule: &str) -> FieldDefinition {
        field.access = FieldAccess {
            read: Some(HookRef::new(rule)),
            ..Default::default()
        };

        field
    }

    fn container(
        name: &str,
        field_type: FieldType,
        fields: Vec<FieldDefinition>,
    ) -> FieldDefinition {
        FieldDefinition::builder(name, field_type)
            .fields(fields)
            .build()
    }

    fn schema() -> CollectionDefinition {
        let inner = container(
            "inner",
            FieldType::Array,
            vec![gated(text("secret"), "deny")],
        );

        let items = container(
            "items",
            FieldType::Array,
            vec![
                text("name"),
                text("public"),
                gated(text("secret"), "deny"),
                gated(text("teaser"), "public_only"),
                inner,
            ],
        );

        let seo = container(
            "seo",
            FieldType::Group,
            vec![text("title"), gated(text("secret"), "deny")],
        );

        let content = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new("hero", vec![gated(text("body"), "deny")]),
                BlockDefinition::new("text", vec![text("body"), text("caption")]),
            ])
            .build();

        let locked = gated(
            container("locked", FieldType::Array, vec![text("name")]),
            "deny",
        );

        let tags = gated(
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            "deny",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            text("title"),
            gated(text("token"), "deny"),
            seo,
            items,
            content,
            locked,
            tags,
        ];

        def
    }

    fn unreadable_of(paths: &[&str]) -> Vec<String> {
        let def = schema();
        let ctx = ServiceContext::collection("posts", &def)
            .read_hooks(&RuleStrip)
            .build();
        let paths: Vec<String> = paths.iter().map(|p| (*p).to_string()).collect();

        unreadable_query_paths(&ctx, None, &paths).unwrap()
    }

    fn assert_unreadable(path: &str) {
        assert_eq!(unreadable_of(&[path]), vec![path.to_string()], "{path}");
    }

    fn assert_readable(path: &str) {
        assert!(unreadable_of(&[path]).is_empty(), "{path}");
    }

    #[test]
    fn a_top_level_rule_decides_its_field() {
        assert_unreadable("token");
        assert_readable("title");
    }

    /// Regression: a denied group sub-field was filterable — the probe held
    /// only the group's key, so the sub-field's rule was never evaluated.
    #[test]
    fn a_denied_group_sub_field_is_unreadable_in_both_forms() {
        assert_unreadable("seo__secret");
        assert_unreadable("seo.secret");
        assert_readable("seo__title");
        assert_readable("seo.title");
    }

    /// Regression: `where items.secret like 'a%'` probed a value the response
    /// strips from every row.
    #[test]
    fn a_denied_array_row_sub_field_is_unreadable() {
        assert_unreadable("items.secret");
        assert_readable("items.name");
        assert_readable("items.id");
    }

    /// Regression: a block sub-field denied in one block type stays unreadable
    /// even though another block type holds a readable field of that name.
    #[test]
    fn a_block_sub_field_denied_in_one_block_type_is_unreadable() {
        assert_unreadable("content.body");
        assert_readable("content.caption");
        assert_readable("content._block_type");
        assert_readable("content.id");
    }

    #[test]
    fn a_denied_field_in_a_nested_array_is_unreadable() {
        assert_unreadable("items.inner.secret");
    }

    #[test]
    fn a_denied_container_makes_every_path_beneath_it_unreadable() {
        assert_unreadable("locked.name");
        assert_unreadable("locked.id");
    }

    /// A relationship's `.id` is judged by the relationship's own rule.
    #[test]
    fn a_denied_relationship_id_path_is_unreadable() {
        assert_unreadable("tags.id");
        assert_unreadable("tags");
    }

    /// A rule that needs the row's data cannot allow a query, which has none.
    #[test]
    fn a_data_aware_rule_denies_filtering() {
        assert_unreadable("items.teaser");
    }

    /// Each path is judged on its own probes; order is kept.
    #[test]
    fn mixed_paths_report_only_the_unreadable_ones_in_order() {
        assert_eq!(
            unreadable_of(&[
                "title",
                "items.secret",
                "seo__title",
                "content.body",
                "token"
            ]),
            vec![
                "items.secret".to_string(),
                "content.body".to_string(),
                "token".to_string(),
            ]
        );
    }

    /// A group holding an array, blocks, a has-many relationship and a nested
    /// group with an array — each with a denied sub-field — beside a denied
    /// group holding a readable array, all inside a layout row.
    fn group_container_schema() -> CollectionDefinition {
        let links = container(
            "links",
            FieldType::Array,
            vec![text("url"), gated(text("secret"), "deny")],
        );
        let sections = FieldDefinition::builder("sections", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "part",
                vec![text("title"), gated(text("body"), "deny")],
            )])
            .build();
        let tags = gated(
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            "deny",
        );
        let rows = container(
            "rows",
            FieldType::Array,
            vec![text("name"), gated(text("secret"), "deny")],
        );
        let inner = container("inner", FieldType::Group, vec![rows]);
        let seo = container("seo", FieldType::Group, vec![links, sections, tags, inner]);
        let private = gated(
            container(
                "private",
                FieldType::Group,
                vec![container("links", FieldType::Array, vec![text("url")])],
            ),
            "deny",
        );
        let layout = container("layout", FieldType::Row, vec![seo, private]);

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![layout];

        def
    }

    fn unreadable_in(def: &CollectionDefinition, paths: &[&str]) -> Vec<String> {
        let ctx = ServiceContext::collection("posts", def)
            .read_hooks(&RuleStrip)
            .build();
        let paths: Vec<String> = paths.iter().map(|p| (*p).to_string()).collect();

        unreadable_query_paths(&ctx, None, &paths).unwrap()
    }

    /// A join-table field inside groups is filtered through the groups in
    /// either spelling; every rule on the way — the row's sub-field, the
    /// relationship's, an enclosing group's — decides the path in both.
    #[test]
    fn paths_into_containers_inside_groups_are_judged_by_every_rule_on_the_way() {
        let def = group_container_schema();

        let readable = [
            "seo.links.url",
            "seo__links.url",
            "seo.links.id",
            "seo.sections.title",
            "seo__sections._block_type",
            "seo.inner.rows.name",
            "seo__inner__rows.name",
            "seo.inner__rows.name",
        ];
        let unreadable = [
            "seo.links.secret",
            "seo__links.secret",
            "seo.sections.body",
            "seo__sections.body",
            "seo.tags.id",
            "seo__tags.id",
            "seo.inner.rows.secret",
            "seo__inner__rows.secret",
            "seo.inner__rows.secret",
            "private.links.url",
            "private__links.url",
        ];

        assert!(unreadable_in(&def, &readable).is_empty());

        for path in unreadable {
            assert_eq!(
                unreadable_in(&def, &[path]),
                vec![path.to_string()],
                "{path}"
            );
        }
    }

    /// Regression: a `hidden` field inside an array inside a group was
    /// matched only in the flat spelling its denial carries
    /// (`seo__links.url`), so `seo.links.url` filtered on it; a hidden group
    /// value likewise only as `seo__secret`. Both spellings are one path.
    #[test]
    fn a_hidden_field_is_unfilterable_in_every_spelling_of_its_path() {
        let links = container(
            "links",
            FieldType::Array,
            vec![text("title"), hidden_text("url")],
        );
        let seo = container(
            "seo",
            FieldType::Group,
            vec![text("title"), hidden_text("secret"), links],
        );
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![seo];

        for path in [
            "seo.links.url",
            "seo__links.url",
            "seo.secret",
            "seo__secret",
        ] {
            assert_eq!(
                unreadable_in(&def, &[path]),
                vec![path.to_string()],
                "{path}"
            );
        }

        assert!(unreadable_in(&def, &["seo.links.title", "seo.title"]).is_empty());
    }

    fn equals(field: &str) -> Vec<FilterClause> {
        vec![FilterClause::Single(Filter {
            field: field.to_string(),
            op: FilterOp::Equals("x".to_string()),
        })]
    }

    /// The bulk-write form judges through the strip it is handed: a write
    /// context carries no read hooks.
    #[test]
    fn a_bulk_filter_is_judged_through_the_given_strip() {
        let def = schema();
        let ctx = ServiceContext::collection("posts", &def).build();

        let err = reject_unreadable_filter_fields(&ctx, &RuleStrip, None, &equals("seo.secret"))
            .expect_err("a denied group sub-field");
        assert!(matches!(err, ServiceError::AccessDenied(_)), "{err:?}");

        reject_unreadable_filter_fields(&ctx, &RuleStrip, None, &equals("title"))
            .expect("a plain field");
        reject_unreadable_filter_fields(&ctx, &RuleStrip, None, &[]).expect("no filter");
    }
}
