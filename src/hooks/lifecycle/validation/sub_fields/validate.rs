use std::collections::HashMap;

use mlua::Lua;
use serde_json::{Map as JsonMap, Value};
use tracing::warn;

use crate::{
    core::{FieldDefinition, FieldType, Registry, validate::FieldError},
    hooks::lifecycle::validation::{
        checks::{self, is_valid_date_format},
        custom::{ValidateCtxSource, run_required_condition_inner, run_validate_function_inner},
        is_empty_value,
        richtext_attrs::{RichtextValidationCtx, validate_richtext_node_attrs},
    },
};

/// Stable context shared across recursive validation calls for a single row.
struct RowValidationCtx<'a> {
    lua: &'a Lua,
    row_obj: &'a JsonMap<String, Value>,
    row_data: &'a HashMap<String, Value>,
    parent_name: &'a str,
    idx: usize,
    table: &'a str,
    registry: Option<&'a Registry>,
    is_draft: bool,
    /// Content locale this write targets — exposed to custom validators as
    /// `ctx.locale`. `None` when localization is disabled.
    locale: Option<&'a str>,
    /// `"create"` or `"update"` — exposed to sub-field validators as `ctx.operation`.
    operation: &'a str,
    /// The parent document id on `update`; `None` on `create`.
    id: Option<&'a str>,
    /// The full parent document (so sub-field validators can cross-reference
    /// fields outside their row).
    document: &'a HashMap<String, Value>,
}

/// Per-sub-field call target — the field being validated and its qualified
/// name for error reporting. Threaded through the leaf/nested-row helpers
/// so they don't take 5+ positional args.
struct SubFieldCall<'a> {
    sf: &'a FieldDefinition,
    qualified: &'a str,
}

/// Parameters for sub-field validation within a single array/blocks row.
pub(in crate::hooks::lifecycle::validation) struct SubFieldParams<'a> {
    pub lua: &'a Lua,
    pub parent_name: &'a str,
    pub idx: usize,
    pub table: &'a str,
    pub registry: Option<&'a Registry>,
    pub is_draft: bool,
    /// Content locale this write targets — exposed to custom validators as
    /// `ctx.locale`. `None` when localization is disabled.
    pub locale: Option<&'a str>,
    /// `"create"` or `"update"`.
    pub operation: &'a str,
    /// The parent document id on `update`; `None` on `create`.
    pub id: Option<&'a str>,
    /// The full parent document.
    pub document: &'a HashMap<String, Value>,
}

/// Validate sub-fields within a single array/blocks row (inner, no mutex).
pub(in crate::hooks::lifecycle::validation) fn validate_sub_fields_inner(
    params: &SubFieldParams<'_>,
    sub_fields: &[FieldDefinition],
    row_obj: &JsonMap<String, Value>,
    errors: &mut Vec<FieldError>,
) {
    let row_data: HashMap<String, Value> = row_obj
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let ctx = RowValidationCtx {
        lua: params.lua,
        row_obj,
        row_data: &row_data,
        parent_name: params.parent_name,
        idx: params.idx,
        table: params.table,
        registry: params.registry,
        is_draft: params.is_draft,
        locale: params.locale,
        operation: params.operation,
        id: params.id,
        document: params.document,
    };

    validate_children_recursive(&ctx, sub_fields, "", errors);
}

/// Recursively validate fields within an array/blocks row, handling arbitrary
/// nesting of layout containers (Group, Row, Collapsible, Tabs).
///
/// `group_prefix` accumulates the `__`-separated group path for data key lookups
/// (e.g. `""` at top level, `"meta__"` inside group `meta`).
fn validate_children_recursive(
    ctx: &RowValidationCtx<'_>,
    fields: &[FieldDefinition],
    group_prefix: &str,
    errors: &mut Vec<FieldError>,
) {
    for sf in fields {
        match sf.field_type {
            FieldType::Group => {
                // Navigate into the Group's nested object and validate children.
                // Form parser stores Group data as nested objects (e.g., {"meta": {"title": "..."}}).
                let data_key = format!("{}{}", group_prefix, sf.name);
                let qualified = format!("{}[{}][{}]", ctx.parent_name, ctx.idx, data_key);
                let params = SubFieldParams {
                    lua: ctx.lua,
                    parent_name: &qualified,
                    idx: 0,
                    table: ctx.table,
                    registry: ctx.registry,
                    is_draft: ctx.is_draft,
                    locale: ctx.locale,
                    operation: ctx.operation,
                    id: ctx.id,
                    document: ctx.document,
                };

                // Rows are submitted whole, so an absent group means all its
                // children are absent — validate them against an empty object
                // so `required` sub-fields still fire. A present non-object
                // (non-null) value is a malformed row, not a skip.
                let group_val = ctx.row_obj.get(&data_key);

                if let Some(v) = group_val
                    && !v.is_null()
                    && v.as_object().is_none()
                {
                    errors.push(
                        FieldError::with_key(
                            qualified.clone(),
                            format!("{} must be an object", sf.name),
                            "validation.invalid_row_type",
                        )
                        .with_param("field", sf.name.clone()),
                    );
                } else {
                    let empty = serde_json::Map::new();
                    let group_obj = group_val.and_then(Value::as_object).unwrap_or(&empty);
                    validate_sub_fields_inner(&params, &sf.fields, group_obj, errors);
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                validate_children_recursive(ctx, &sf.fields, group_prefix, errors);
            }
            FieldType::Tabs => {
                for tab in &sf.tabs {
                    validate_children_recursive(ctx, &tab.fields, group_prefix, errors);
                }
            }
            FieldType::Array | FieldType::Blocks => {
                let data_key = format!("{}{}", group_prefix, sf.name);
                let qualified = format!("{}[{}][{}]", ctx.parent_name, ctx.idx, data_key);
                let call = SubFieldCall {
                    sf,
                    qualified: &qualified,
                };

                validate_leaf_sub_field(ctx, &call, ctx.row_obj.get(&data_key), errors);

                if let Some(Value::Array(nested_rows)) = ctx.row_obj.get(&data_key) {
                    validate_nested_rows(ctx, &call, nested_rows, errors);
                }
            }
            // A Join is a virtual reverse-relationship with no row data; it
            // carries nothing to validate. Skip it explicitly, mirroring the
            // top-level dispatch (`recursive/dispatch.rs`) — otherwise the
            // `_ =>` leaf arm would push a spurious "is required" error for a
            // `required` Join nested in an array/blocks row.
            FieldType::Join => {}
            _ => {
                let data_key = format!("{}{}", group_prefix, sf.name);
                let qualified = format!("{}[{}][{}]", ctx.parent_name, ctx.idx, data_key);
                let call = SubFieldCall {
                    sf,
                    qualified: &qualified,
                };

                validate_leaf_sub_field(ctx, &call, ctx.row_obj.get(&data_key), errors);
            }
        }
    }
}

/// Recurse into nested array/blocks rows, resolving block type fields and validating each row.
fn validate_nested_rows(
    ctx: &RowValidationCtx<'_>,
    call: &SubFieldCall<'_>,
    nested_rows: &[Value],
    errors: &mut Vec<FieldError>,
) {
    for (nested_idx, nested_row) in nested_rows.iter().enumerate() {
        let Some(nested_obj) = nested_row.as_object() else {
            // Mirror the top-level array/blocks walker: a non-object row is a
            // malformed payload and must surface an error, not be silently
            // skipped (which would let a bad row escape validation entirely).
            errors.push(
                FieldError::with_key(
                    format!("{}[{nested_idx}]", call.qualified),
                    format!("{} row {nested_idx} must be an object", call.sf.name),
                    "validation.invalid_row_type",
                )
                .with_param("field", call.sf.name.clone())
                .with_param("index", nested_idx.to_string()),
            );

            continue;
        };

        let sub_fields: &[FieldDefinition] = if call.sf.field_type == FieldType::Blocks {
            let bt = nested_obj
                .get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let Some(bd) = call.sf.blocks.iter().find(|b| b.block_type == bt) else {
                // Mirror the top-level blocks walker: an unknown (or missing)
                // block type is a malformed row and must error, not be
                // silently skipped into the stored JSON.
                errors.push(
                    FieldError::with_key(
                        format!("{}[{nested_idx}]", call.qualified),
                        format!(
                            "{} row {nested_idx} has unknown block type '{bt}'",
                            call.sf.name
                        ),
                        "validation.unknown_block_type",
                    )
                    .with_param("field", call.sf.name.clone())
                    .with_param("index", nested_idx.to_string())
                    .with_param("block_type", bt.to_string()),
                );

                continue;
            };

            &bd.fields
        } else {
            &call.sf.fields
        };

        let params = SubFieldParams {
            lua: ctx.lua,
            parent_name: call.qualified,
            idx: nested_idx,
            table: ctx.table,
            registry: ctx.registry,
            is_draft: ctx.is_draft,
            locale: ctx.locale,
            operation: ctx.operation,
            id: ctx.id,
            document: ctx.document,
        };

        validate_sub_fields_inner(&params, sub_fields, nested_obj, errors);
    }
}

/// Whether a sub-field is required for this row. `true` when it's statically
/// `required`, or when its `required_when` predicate returns truthy for the row
/// (`ctx.data`) and parent document (`ctx.document`). Returns `false` on drafts
/// (required is not enforced there, and a predicate error must not block a
/// draft); a predicate that errors pushes a field error and is treated as not
/// required.
fn sub_field_required(
    ctx: &RowValidationCtx<'_>,
    sf: &FieldDefinition,
    qualified: &str,
    errors: &mut Vec<FieldError>,
) -> bool {
    if ctx.is_draft {
        return false;
    }
    if sf.required {
        return true;
    }

    let Some(required_when) = sf.required_when.as_ref() else {
        return false;
    };

    let func_ref = required_when.reference();

    match run_required_condition_inner(
        ctx.lua,
        func_ref,
        &ValidateCtxSource {
            data: ctx.row_data,
            document: ctx.document,
            collection: ctx.table,
            field_name: &sf.name,
            locale: ctx.locale,
            operation: ctx.operation,
            id: ctx.id,
            options: required_when.options(),
        },
    ) {
        Ok(req) => req,
        Err(e) => {
            errors.push(FieldError::new(
                qualified.to_owned(),
                format!("required_when predicate '{func_ref}' failed: {e}"),
            ));
            false
        }
    }
}

/// Validate a single leaf sub-field inside an array/blocks row container (Group, Row,
/// Collapsible, or Tabs). Runs the required check, date format check, custom Lua
/// validate function, and richtext node attr validation.
fn validate_leaf_sub_field(
    ctx: &RowValidationCtx<'_>,
    call: &SubFieldCall<'_>,
    value: Option<&Value>,
    errors: &mut Vec<FieldError>,
) {
    let SubFieldCall { sf, qualified } = *call;

    let is_empty = is_empty_value(value);

    // An empty array counts as absent for `required` (matching the top-level
    // `is_value_present`), but NOT for the other checks — `min_rows` etc.
    // must still see the empty list to enforce count bounds.
    let is_missing_for_required =
        is_empty || matches!(value, Some(Value::Array(arr)) if arr.is_empty());

    // 1. Required check (skip for Checkbox — absent/false is valid, and skipped
    //    on drafts via `sub_field_required`).
    if sub_field_required(ctx, sf, qualified, errors)
        && is_missing_for_required
        && sf.field_type != FieldType::Checkbox
    {
        errors.push(
            FieldError::with_key(
                qualified.to_owned(),
                format!("{} is required", sf.name),
                "validation.required",
            )
            .with_param("field", sf.name.clone()),
        );
    }

    // 2. Date format check
    if sf.field_type == FieldType::Date
        && !is_empty
        && let Some(Value::String(s)) = value
        && !is_valid_date_format(s)
    {
        errors.push(
            FieldError::with_key(
                qualified.to_owned(),
                format!("{} is not a valid date format", sf.name),
                "validation.invalid_date",
            )
            .with_param("field", sf.name.clone()),
        );
    }

    // 3. Custom Lua validate function
    if let Some(ref validate) = sf.validate
        && let Some(val) = value
    {
        let validate_ref = validate.reference();

        match run_validate_function_inner(
            ctx.lua,
            validate_ref,
            val,
            &ValidateCtxSource {
                data: ctx.row_data,
                document: ctx.document,
                collection: ctx.table,
                field_name: &sf.name,
                locale: ctx.locale,
                operation: ctx.operation,
                id: ctx.id,
                options: validate.options(),
            },
        ) {
            Ok(Some(err_msg)) => {
                errors.push(FieldError::new(qualified.to_owned(), err_msg));
            }
            Ok(None) => {}
            Err(e) => {
                warn!("Validate function '{}' error: {}", validate_ref, e);

                errors.push(
                    FieldError::with_key(
                        qualified.to_owned(),
                        format!("Validation failed (internal error in '{validate_ref}')"),
                        "validation.custom_error",
                    )
                    .with_param("field", sf.name.clone()),
                );
            }
        }
    }

    // 4. Length bounds (min_length / max_length)
    checks::check_length_bounds(sf, qualified, value, is_empty, errors);

    // 5. Numeric bounds (min / max)
    checks::check_numeric_bounds(sf, qualified, value, is_empty, errors);

    // 6. Email format validation
    checks::check_email_format(sf, qualified, value, is_empty, errors);

    // 7. Select/radio option validation
    checks::check_option_valid(sf, qualified, value, is_empty, errors);

    // 8. Has-many element validation (per-element length/numeric bounds, row counts)
    checks::check_has_many_elements(sf, qualified, value, is_empty, errors);

    // 8b. Polymorphic relationship allowlist — a forged target collection on a
    //     relationship/upload nested inside an array/blocks row must be rejected
    //     here too, exactly as at the top level. The save path descends into
    //     nested composites, so skipping this would let an attacker reference a
    //     collection the field author never allowed.
    checks::check_polymorphic_allowlist(sf, qualified, value, errors);

    // 8c. Row bounds (min_rows / max_rows) for a nested Array/Blocks sub-field.
    //     The top-level walker runs this for every field (a no-op unless bounds
    //     are set); without it here, an array-in-array / array-in-blocks /
    //     blocks-in-group `min_rows`/`max_rows` would never be enforced.
    //     `is_update: false` — rows are always submitted whole, so an absent
    //     nested field genuinely has zero rows (no partial-update exemption).
    checks::check_row_bounds(sf, qualified, value, ctx.is_draft, false, errors);

    // 9. Richtext node attr validation
    if sf.field_type == FieldType::Richtext
        && !is_empty
        && !sf.admin.nodes.is_empty()
        && let Some(registry) = ctx.registry
        && let Some(Value::String(content)) = value
    {
        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(ctx.lua, registry, ctx.table)
                .draft(ctx.is_draft)
                .locale(ctx.locale)
                .operation(ctx.operation)
                .id(ctx.id)
                .build(),
            content,
            qualified,
            sf,
            errors,
        );
    }
}
