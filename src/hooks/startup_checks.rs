//! Whole-registry boot gates: the checks that fail the server rather than
//! strand a definition at runtime.
//!
//! - `hook_refs` — every statically-known hook/access/filter/validator ref
//!   must resolve in the init-time Lua VM.
//! - `routes` — custom routes registered via `crap.routes.register`.
//! - `auth_methods` — per-collection `auth.methods` shape.
//! - `default_sort` — admin default-sort fields.
//! - `relation_targets` — relationship/upload/join fields name a registered
//!   (for uploads: upload-enabled) target collection.
//! - `join_limits` — no join lists more than `[pagination] max_limit`.
//! - `richtext_nodes` — rich text fields list only registered custom nodes.
//! - `row_conditions` — no `admin.condition` on a field inside an array or
//!   blocks row.
//! - `run_all` — the boot runs every gate and reports every problem together.
//! - This file — locale/field-name collisions, table-name collisions, job
//!   cron schedules, `required_locales`, and the advisory warnings
//!   (public lifecycle views, MCP reserved-argument shadowing).
//!
//! Distinct from `lifecycle/validation/`, which runs per-write field
//! validation.

mod auth_methods;
mod default_sort;
mod hook_refs;
mod join_limits;
mod pages;
mod relation_targets;
mod richtext_nodes;
mod routes;
mod row_conditions;
mod run_all;

pub use auth_methods::validate_auth_methods;
pub use default_sort::validate_admin_default_sorts;
pub use hook_refs::{validate_admin_access_ref, validate_hook_references};
pub use join_limits::validate_join_limits;
pub use pages::validate_pages;
pub use relation_targets::validate_relation_targets;
pub use richtext_nodes::validate_richtext_nodes;
pub use routes::validate_routes;
pub use row_conditions::validate_row_conditions;
pub(crate) use run_all::run_startup_checks;

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use tracing::warn;

use crate::core::{
    FieldDefinition, FieldType, Registry, RequiredLocales, SchemaStep, walk_all_fields,
};
use crate::db::query::helpers::{
    global_table, join_table, locale_column, prefixed_name, walk_leaf_fields,
};
use crate::scheduler::parse_cron;
use crate::service::op::wire::{self, WireSurfaces};

/// Validate every job's cron `schedule`.
///
/// An unparseable expression is a boot failure, not a runtime warning: the
/// scheduler can only skip such a job, which looks identical to a job that
/// simply has not come due yet, so the typo would hide for as long as the
/// deployment runs.
///
/// # Errors
///
/// Returns an aggregated error naming every job whose schedule does not parse.
pub fn validate_job_schedules(registry: &Registry) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    for (slug, def) in &registry.jobs {
        let Some(schedule) = def.schedule.as_deref() else {
            continue;
        };

        if let Err(e) = parse_cron(schedule) {
            errors.push(format!("job '{slug}': schedule '{schedule}': {e}"));
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    bail!(
        "Invalid cron schedule(s):\n  - {}\n\n\
         Schedules use standard crontab syntax: 5 fields (minute hour day-of-month \
         month day-of-week), or 6 with a leading seconds field. Day-of-week is 0-6 \
         with Sunday = 0 (7 also means Sunday); names such as MON-FRI also work.",
        errors.join("\n  - ")
    );
}

/// Warn when `default_deny = false` leaves a draft or trash view ungated. With
/// `default_deny = false`, a view whose access resolves to `None` (no own key,
/// and no `update` fallback) is allowed for *everyone* — so a collection with
/// `drafts`/`soft_delete` enabled but no `draft`/`trash`/`update` rule exposes
/// its unpublished and soft-deleted rows to unauthenticated callers. Globals
/// have no soft-delete, but the same footgun applies to their `draft` view.
/// `read` (published) is intentionally not flagged: published content being
/// public is the expected default; the footgun is drafts/trash silently joining
/// it. Advisory only (the config is valid, just dangerous).
pub fn warn_public_lifecycle_views(registry: &Registry, default_deny: bool) {
    if default_deny {
        return;
    }

    for (slug, def) in &registry.collections {
        for view in def.publicly_exposed_lifecycle_views(default_deny) {
            let documents = match view {
                "trash" => "soft-deleted",
                _ => view,
            };

            warn!(
                "Collection '{slug}': access.default_deny is false and the '{view}' view has \
                 no access.{view} (or access.update) rule — {documents} documents are visible \
                 to EVERYONE, including unauthenticated callers. Add an access.{view} (or \
                 access.update) rule, or set access.default_deny = true."
            );
        }
    }

    for (slug, def) in &registry.globals {
        if def.draft_view_publicly_exposed(default_deny) {
            warn!(
                "Global '{slug}': access.default_deny is false and the 'draft' view has \
                 no access.draft (or access.update) rule — draft documents are visible \
                 to EVERYONE, including unauthenticated callers. Add an access.draft (or \
                 access.update) rule, or set access.default_deny = true."
            );
        }
    }
}

/// Warn when a field name collides with a reserved MCP tool argument.
///
/// The write tools carry their meta-options as top-level arguments
/// (`locale`, `draft`, `events`, `id`, `password`, `queue`, …), so a field of
/// the same name is shadowed: its schema property is overwritten by the
/// option and the write path skips it as a reserved key. The collection still
/// works on every other surface, hence a warning rather than a refusal — but
/// silently dropping one field's data on one surface is exactly the kind of
/// thing an operator should hear about at boot.
pub fn warn_mcp_reserved_field_shadowing(registry: &Registry, mcp_enabled: bool) {
    if !mcp_enabled {
        return;
    }

    // The union of every meta-argument the MCP surface spells, plus the two
    // that are added per-op rather than declared in the wire model.
    let reserved: HashSet<&str> = wire::COLLECTION_OPS
        .iter()
        .chain(wire::GLOBAL_OPS)
        .flat_map(|op| op.fields)
        .filter(|f| f.surfaces.contains(WireSurfaces::MCP))
        .map(|f| f.name)
        .chain(["id", "password"])
        .collect();

    for (slug, def) in &registry.collections {
        for field in &def.fields {
            if reserved.contains(field.name.as_str()) {
                warn!(
                    "Collection '{slug}': field '{}' shadows a reserved MCP tool argument — \
                     over MCP that name is the option, and the field's value is dropped. \
                     Rename the field, or exclude the collection from MCP.",
                    field.name
                );
            }
        }
    }
}

/// Reject field names that collide with the generated locale-suffixed column
/// pattern `{field}__{locale}`. If a user defines a literal field named
/// `title__en` while `en` is a configured locale, the generated localized
/// column for `title` would be `title__en` — a silent collision. Fail startup.
///
/// # Errors
///
/// Returns an aggregated error naming every colliding field.
pub fn validate_locale_field_collisions(registry: &Registry, locales: &[String]) -> Result<()> {
    if locales.is_empty() {
        return Ok(());
    }

    let mut collisions: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        walk_fields_for_collisions(
            &def.fields,
            locales,
            &format!("collection '{slug}'"),
            &mut collisions,
        );
    }

    for (slug, def) in &registry.globals {
        walk_fields_for_collisions(
            &def.fields,
            locales,
            &format!("global '{slug}'"),
            &mut collisions,
        );
    }

    if collisions.is_empty() {
        return Ok(());
    }

    let body = collisions.join("\n  - ");
    bail!(
        "Field name collides with locale-suffixed column pattern '{{name}}__{{locale}}':\n  - {body}\n\n\
         Rename the field (or change its locale suffix) to avoid a silent collision \
         with the generated localized column."
    );
}

fn walk_fields_for_collisions(
    fields: &[FieldDefinition],
    locales: &[String],
    source: &str,
    out: &mut Vec<String>,
) {
    walk_all_fields(fields, &mut Vec::new(), &mut |f, _| {
        for loc in locales {
            // The column form of the locale, as the generated column spells it.
            let Ok(suffix) = locale_column("", loc) else {
                continue;
            };
            if f.name.ends_with(&suffix) && f.name.len() > suffix.len() {
                out.push(format!(
                    "{source} field '{}': ends with locale suffix '{}'",
                    f.name, suffix
                ));
            }
        }
    });
}

/// Validate every `required_locales` setting against the configured locales, so
/// a typo (`"de-DE"` when only `"de"` is configured) fails to boot instead of
/// silently making every non-draft write to that field error at runtime.
///
/// Checks the collection-level default and every field-level override (recursing
/// through groups, blocks, tabs). `All` is always valid (it expands to the
/// configured set). A `List` code must be a configured locale; setting any
/// `required_locales` while localization is disabled is also rejected.
///
/// # Errors
///
/// Returns `Err` with a single aggregated message listing every offending
/// setting and its source.
pub fn validate_required_locales(registry: &Registry, locales: &[String]) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        check_required_locales_value(
            def.required_locales.as_ref(),
            locales,
            &format!("collection '{slug}' (collection-level required_locales)"),
            &mut errors,
        );
        walk_fields_for_required_locales(
            &def.fields,
            locales,
            &format!("collection '{slug}'"),
            &mut errors,
        );
    }

    // Globals have no collection-level `required_locales`; only field overrides.
    for (slug, def) in &registry.globals {
        walk_fields_for_required_locales(
            &def.fields,
            locales,
            &format!("global '{slug}'"),
            &mut errors,
        );
    }

    if errors.is_empty() {
        return Ok(());
    }

    let body = errors.join("\n  - ");
    bail!(
        "Invalid `required_locales` configuration:\n  - {body}\n\n\
         `required_locales` may only be set on locale-scoped fields, and every \
         listed locale must appear in `[locale].locales`."
    );
}

/// Validate a single `required_locales` value against the configured locales.
fn check_required_locales_value(
    rl: Option<&RequiredLocales>,
    locales: &[String],
    source: &str,
    out: &mut Vec<String>,
) {
    let Some(rl) = rl else {
        return;
    };

    if locales.is_empty() {
        out.push(format!(
            "{source} sets required_locales but localization is not enabled \
             (no `[locale].locales` configured)"
        ));
        return;
    }

    if let RequiredLocales::List(list) = rl {
        for code in list {
            if !locales.iter().any(|l| l == code) {
                out.push(format!(
                    "{source}: unknown locale '{code}' (configured: {})",
                    locales.join(", ")
                ));
            }
        }
    }
}

/// Walk a field tree validating every `required_locales` override.
///
/// Fields nested inside Array/Blocks rows are skipped: completeness treats
/// those as opaque leaf join targets, so a `required_locales` nested inside
/// one is inert and not validated here. Localization scope matches the
/// completeness check (and the migration DDL): only enclosing *groups* carry
/// `localized` down — layout wrappers don't.
fn walk_fields_for_required_locales(
    fields: &[FieldDefinition],
    locales: &[String],
    source: &str,
    out: &mut Vec<String>,
) {
    walk_all_fields(fields, &mut Vec::new(), &mut |f, path| {
        let inside_join_subtree = path
            .iter()
            .any(|s| matches!(s, SchemaStep::Field(p) if p.field_type.has_rows()));
        if inside_join_subtree {
            return;
        }

        let Some(rl) = f.required_locales.as_ref() else {
            return;
        };

        let ctx = format!("{source} field '{}'", f.name);

        // `required_locales` is only meaningful on a locale-scoped field —
        // one that is `localized`, or inherits localization from an
        // enclosing group. This honors inheritance (the per-field parser
        // can't), matching how the completeness check resolves scope.
        let inherited_localized = path.iter().any(|s| {
            matches!(s, SchemaStep::Field(p)
                if p.field_type == FieldType::Group && p.localized)
        });
        if !f.is_locale_scoped(inherited_localized) {
            out.push(format!(
                "{ctx}: required_locales only applies to localized fields \
                 (mark the field — or its enclosing group — localized)"
            ));
        }

        check_required_locales_value(Some(rl), locales, &ctx, out);
    });
}

/// Reject definitions whose generated DB table names collide.
///
/// Every collection produces a main table named after its slug; every global a
/// `_global_{slug}` table; and array / blocks / has-many-relationship fields
/// produce join tables named `{slug}_{field}` (with `{group}__` prefixes for
/// nested groups). Because slugs may themselves contain `_`, a collection
/// slugged `posts_tags` collides with the `tags` array field of a `posts`
/// collection — and the migration layer would then silently ALTER one table as
/// if it were the other. Reject the collision up front instead.
///
/// # Errors
///
/// Returns an aggregated error naming every colliding table name.
pub fn validate_table_name_collisions(registry: &Registry) -> Result<()> {
    let mut owners: HashMap<String, String> = HashMap::new();
    let mut conflicts: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        claim_table(
            &mut owners,
            &mut conflicts,
            slug.to_string(),
            format!("collection '{slug}'"),
        );
        collect_field_tables(slug, &def.fields, &mut owners, &mut conflicts);
    }

    for slug in registry.globals.keys() {
        // A global never shares a TABLE with a collection (`_global_` prefix),
        // but it must not share a SLUG either: the MCP surface keys exposure,
        // gating, and description by slug.
        if registry.collections.contains_key(slug) {
            conflicts.push(format!(
                "slug '{slug}' is used by both a collection and a global — slugs are unique \
                 across both"
            ));
        }
        claim_table(
            &mut owners,
            &mut conflicts,
            global_table(slug),
            format!("global '{slug}'"),
        );
    }

    if conflicts.is_empty() {
        return Ok(());
    }

    let body = conflicts.join("\n  - ");
    bail!(
        "Generated table name collision(s):\n  - {body}\n\n\
         Rename the offending collection slug or field so each generated table \
         name is unique."
    );
}

/// Record `table` as owned by `owner`; if a different owner already claimed it,
/// push a conflict message instead.
fn claim_table(
    owners: &mut HashMap<String, String>,
    conflicts: &mut Vec<String>,
    table: String,
    owner: String,
) {
    match owners.get(&table) {
        Some(existing) if existing != &owner => {
            conflicts.push(format!(
                "'{table}' is generated by both {existing} and {owner}"
            ));
        }
        Some(_) => {}
        None => {
            owners.insert(table, owner);
        }
    }
}

/// Walk a field tree collecting the join-table names it generates, mirroring
/// the migration orchestrator's traversal (`sync_join_tables_inner`). The
/// flat-column walk ([`walk_leaf_fields`]) handles Group `__`-prefixing and
/// transparent layout wrappers; Array/Blocks/has-many fields are the leaf
/// columns that own join tables.
fn collect_field_tables(
    slug: &str,
    fields: &[FieldDefinition],
    owners: &mut HashMap<String, String>,
    conflicts: &mut Vec<String>,
) {
    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        let full_name = prefixed_name(prefix, &field.name);

        let is_has_many_ref = field.field_type.is_reference()
            && field.relationship.as_ref().is_some_and(|rc| rc.has_many);

        if is_has_many_ref {
            claim_table(
                owners,
                conflicts,
                join_table(slug, &full_name),
                format!("has-many field '{full_name}' of collection '{slug}'"),
            );
        } else if field.field_type.has_rows() {
            claim_table(
                owners,
                conflicts,
                join_table(slug, &full_name),
                format!("field '{full_name}' of collection '{slug}'"),
            );
        }

        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use crate::core::{CollectionDefinition, GlobalDefinition, job::JobDefinition};

    use super::*;

    /// Regression: an unparseable `schedule` used to be a per-tick `warn!`
    /// only — the job silently never ran for the life of the deployment.
    #[test]
    fn validate_job_schedules_rejects_an_unparseable_expression() {
        let registry = Registry::shared();
        registry.write().unwrap().register_job(
            JobDefinition::builder("digest", "jobs.digest.run")
                .schedule("every monday")
                .build(),
        );

        let err = validate_job_schedules(&registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("job 'digest'"), "expected job slug: {msg}");
        assert!(msg.contains("every monday"), "expected expression: {msg}");
    }

    /// Crontab spellings the raw `cron` crate rejects — Sunday as `0` — must
    /// pass the boot gate, and a job without a schedule is never checked.
    #[test]
    fn validate_job_schedules_accepts_crontab_syntax() {
        let registry = Registry::shared();
        {
            let mut w = registry.write().unwrap();
            w.register_job(
                JobDefinition::builder("weekly", "jobs.weekly.run")
                    .schedule("0 3 * * 0")
                    .build(),
            );
            w.register_job(
                JobDefinition::builder("workdays", "jobs.workdays.run")
                    .schedule("0 8 * * MON-FRI")
                    .build(),
            );
            w.register_job(JobDefinition::builder("manual", "jobs.manual.run").build());
        }

        validate_job_schedules(&registry.read().unwrap()).expect("crontab schedules must parse");
    }

    /// A field literally named `{name}__{locale}` collides with the generated
    /// localized column — reject at startup.
    #[test]
    fn locale_config_rejects_field_name_collision() {
        let mut def = CollectionDefinition::new("posts");
        // `title__en` collides with the generated locale suffix for `en`.
        def.fields = vec![FieldDefinition::builder("title__en", FieldType::Text).build()];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let locales = vec!["en".to_string(), "de".to_string()];
        let err =
            validate_locale_field_collisions(&registry.read().unwrap(), &locales).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("title__en"), "expected field name: {msg}");
        assert!(msg.contains("__en"), "expected locale suffix: {msg}");
        assert!(msg.contains("posts"), "expected slug: {msg}");
    }

    /// Regression: the collision check spelled the suffix with the raw locale
    /// code (`__de-DE`) while locale columns use its column form (`__de_DE`),
    /// so a collision under a hyphenated locale was never found.
    #[test]
    fn locale_field_collisions_use_the_column_form_of_a_locale() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title__de_DE", FieldType::Text).build()];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let locales = vec!["en".to_string(), "de-DE".to_string()];
        let err =
            validate_locale_field_collisions(&registry.read().unwrap(), &locales).unwrap_err();
        assert!(format!("{err:#}").contains("title__de_DE"));
    }

    /// Locale collisions are skipped entirely when no locales are configured.
    #[test]
    fn locale_field_collisions_noop_when_no_locales() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title__en", FieldType::Text).build()];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        validate_locale_field_collisions(&registry.read().unwrap(), &[])
            .expect("no locales = no check");
    }

    /// Unrelated suffixes are fine.
    #[test]
    fn locale_field_collisions_allows_unrelated_names() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("title__fr", FieldType::Text).build(),
        ];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        // `fr` is not in the list, so `title__fr` is just a literal name.
        let locales = vec!["en".to_string(), "de".to_string()];
        validate_locale_field_collisions(&registry.read().unwrap(), &locales)
            .expect("no collision when suffix does not match an enabled locale");
    }

    /// A collection slug that equals another collection's array join-table
    /// name (`posts` + array `tags` → `posts_tags`) must be rejected.
    #[test]
    fn table_name_collision_rejected() {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("tags", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];
        let posts_tags = CollectionDefinition::new("posts_tags");

        let registry = Registry::shared();
        registry.write().unwrap().register_collection(posts);
        registry.write().unwrap().register_collection(posts_tags);

        let err = validate_table_name_collisions(&registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("posts_tags"),
            "expected colliding table: {msg}"
        );
    }

    /// The reserved-argument set the shadowing warning uses must be derived
    /// from the wire model, so a new MCP option is covered automatically.
    #[test]
    fn mcp_reserved_arguments_come_from_the_wire_model() {
        let reserved: HashSet<&str> = wire::COLLECTION_OPS
            .iter()
            .chain(wire::GLOBAL_OPS)
            .flat_map(|op| op.fields)
            .filter(|f| f.surfaces.contains(WireSurfaces::MCP))
            .map(|f| f.name)
            .collect();

        for expected in ["locale", "draft", "where", "depth"] {
            assert!(
                reserved.contains(expected),
                "'{expected}' is an MCP tool argument and must be in the reserved set"
            );
        }
        assert!(
            !reserved.contains("title"),
            "an ordinary field name is not reserved"
        );
    }

    /// A global and a collection never share a table, but they must not share
    /// a slug: the MCP surface keys exposure and gating by slug alone.
    #[test]
    fn shared_collection_and_global_slug_rejected() {
        let registry = Registry::shared();
        registry
            .write()
            .unwrap()
            .register_collection(CollectionDefinition::new("settings"));
        registry
            .write()
            .unwrap()
            .register_global(GlobalDefinition::new("settings"));

        let err = validate_table_name_collisions(&registry.read().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("slug 'settings' is used by both"),
            "expected the shared-slug conflict: {msg}"
        );
    }

    /// Distinct collection and field names generate distinct tables — no error.
    #[test]
    fn table_name_collision_allows_distinct_names() {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("tags", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];
        let authors = CollectionDefinition::new("authors");

        let registry = Registry::shared();
        registry.write().unwrap().register_collection(posts);
        registry.write().unwrap().register_collection(authors);

        validate_table_name_collisions(&registry.read().unwrap())
            .expect("distinct names must not collide");
    }

    // ── required_locales validation ──────────────────────────────────────

    fn en_de() -> Vec<String> {
        vec!["en".to_string(), "de".to_string()]
    }

    /// Register a `posts` collection with one field and run the check against
    /// the given locales, returning the result.
    fn check_field(field: FieldDefinition, locales: &[String]) -> Result<()> {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![field];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);
        validate_required_locales(&registry.read().unwrap(), locales)
    }

    #[test]
    fn required_locales_unknown_code_is_rejected() {
        let err = check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(RequiredLocales::List(vec!["de-DE".to_string()]))
                .build(),
            &en_de(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("de-DE"),
            "error should name the bad code: {err}"
        );
        assert!(err.contains("title"), "error should name the field: {err}");
    }

    #[test]
    fn required_locales_valid_list_passes() {
        check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(RequiredLocales::List(vec![
                    "en".to_string(),
                    "de".to_string(),
                ]))
                .build(),
            &en_de(),
        )
        .expect("configured codes must pass");
    }

    #[test]
    fn required_locales_all_passes() {
        check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(RequiredLocales::All)
                .build(),
            &en_de(),
        )
        .expect("`all` is always valid");
    }

    #[test]
    fn required_locales_none_passes() {
        check_field(
            FieldDefinition::builder("title", FieldType::Text).build(),
            &en_de(),
        )
        .expect("unset is fine");
    }

    #[test]
    fn required_locales_rejected_when_localization_disabled() {
        let err = check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(RequiredLocales::All)
                .build(),
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("localization is not enabled"),
            "should reject required_locales with no configured locales: {err}"
        );
    }

    #[test]
    fn required_locales_collection_level_is_validated() {
        let mut def = CollectionDefinition::new("posts");
        def.required_locales = Some(RequiredLocales::List(vec!["fr".to_string()]));
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .build(),
        ];
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let err = validate_required_locales(&registry.read().unwrap(), &en_de())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("fr"),
            "collection-level bad code must be caught: {err}"
        );
        assert!(
            err.contains("collection-level"),
            "error should point at the collection-level setting: {err}"
        );
    }

    #[test]
    fn required_locales_validated_inside_groups() {
        let err = check_field(
            FieldDefinition::builder("seo", FieldType::Group)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text)
                        .required(true)
                        .required_locales(RequiredLocales::List(vec!["xx".to_string()]))
                        .build(),
                ])
                .build(),
            &en_de(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("xx"), "nested bad code must be caught: {err}");
        assert!(
            err.contains("meta_title"),
            "should name the nested field: {err}"
        );
    }

    /// A required sub-field that inherits localization from a localized group
    /// (without its own `localized = true`) is locale-scoped, so valid
    /// `required_locales` must pass — the per-field parser used to reject this.
    #[test]
    fn required_locales_on_group_inherited_subfield_passes() {
        check_field(
            FieldDefinition::builder("seo", FieldType::Group)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text)
                        .required(true)
                        .required_locales(RequiredLocales::List(vec![
                            "en".to_string(),
                            "de".to_string(),
                        ]))
                        .build(),
                ])
                .build(),
            &en_de(),
        )
        .expect("required_locales on a group-inherited sub-field with valid codes must pass");
    }

    /// `required_locales` on a field that is neither localized nor inside a
    /// localized group is meaningless and must be rejected (this check moved
    /// from the per-field parser to startup so it can honor group inheritance).
    #[test]
    fn required_locales_on_non_locale_scoped_field_rejected() {
        let err = check_field(
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .required_locales(RequiredLocales::List(vec!["en".to_string()]))
                .build(),
            &en_de(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("only applies to localized"),
            "non-locale-scoped required_locales must be rejected: {err}"
        );
    }
}
