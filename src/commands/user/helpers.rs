//! Shared helpers for user management commands.

use std::{
    collections::HashMap,
    io::{BufRead, stdin},
    sync::Arc,
};

use anyhow::{Context as _, Result, anyhow, bail};
use dialoguer::{Password, Select};
use serde_json::Value;

use crate::{
    cli::{self, crap_theme},
    commands::cli_find,
    config::LocaleConfig,
    core::{
        Builder, CollectionDefinition, Document, FieldDefinition, FieldType, Registry,
        collection::Auth, flatten_array_sub_fields,
    },
    db::{BoxedConnection, DbPool, FindQuery, LocaleContext, query},
};
#[cfg(not(tarpaulin_include))]
use dialoguer::Input;

/// Read a password from the first line of `reader`, without its line ending.
///
/// # Errors
///
/// Returns an error if reading fails or the line is empty.
pub(super) fn password_from_reader(mut reader: impl BufRead) -> Result<String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .context("Failed to read password from standard input")?;

    let password = line.trim_end_matches(['\n', '\r']);
    if password.is_empty() {
        bail!("No password on standard input");
    }

    Ok(password.to_string())
}

/// Resolve a new password: standard input when `from_stdin`, else the
/// `-p` value (with a visibility warning), else an interactive prompt with
/// confirmation.
///
/// # Errors
///
/// Returns an error if standard input or the prompt cannot be read.
#[cfg(not(tarpaulin_include))]
pub(super) fn resolve_new_password(
    password: Option<String>,
    from_stdin: bool,
    prompt: &str,
) -> Result<String> {
    if from_stdin {
        return password_from_reader(stdin().lock());
    }

    if let Some(p) = password {
        cli::warning(
            "Password provided via command line — it is visible to other local users and kept \
             in shell history; use --password-stdin instead",
        );
        return Ok(p);
    }

    Password::with_theme(&crap_theme())
        .with_prompt(prompt)
        .with_confirmation("Confirm password", "Passwords do not match")
        .interact()
        .context("Failed to read password")
}

/// Extract the email field from a user document, defaulting to "unknown".
pub(super) fn get_user_email(doc: &Document) -> &str {
    doc.fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}

/// Load and validate an auth collection definition from the registry.
/// Returns the cloned definition (lock is released before returning).
pub(super) fn load_auth_collection(
    registry: &Registry,
    collection: &str,
) -> Result<Arc<CollectionDefinition>> {
    let def = registry
        .get_collection(collection)
        .ok_or_else(|| anyhow!("Collection '{collection}' not found"))?;

    if !def.is_auth_collection() {
        bail!("Collection '{collection}' is not an auth collection (auth must be enabled)");
    }

    Ok(def.clone())
}

/// Check that the collection has email verification enabled.
pub(super) fn require_verify_email(def: &CollectionDefinition, collection: &str) -> Result<()> {
    if !def.auth.as_ref().is_some_and(Auth::requires_verify_email) {
        bail!(
            "Collection '{collection}' does not have email verification enabled (verify_email must be true)"
        );
    }

    Ok(())
}

/// What every user subcommand needs to find the user it operates on.
/// `locale` is required because a LOCALIZED auth collection's rows can only
/// be selected under a locale context (bare column names don't exist there).
/// Without `email` or `id` the user is picked interactively.
#[derive(Builder)]
pub struct UserLookup<'a> {
    #[builder(required)]
    pub pool: &'a DbPool,
    #[builder(required)]
    pub registry: &'a Registry,
    #[builder(required)]
    pub collection: &'a str,
    pub email: Option<String>,
    pub id: Option<String>,
    #[builder(required)]
    pub locale: &'a LocaleConfig,
}

/// Resolve a user by --email or --id. Returns (def, document).
/// Untestable: interactive fallback uses `dialoguer::Select` for user selection.
#[cfg(not(tarpaulin_include))]
pub(super) fn resolve_user(
    lookup: &UserLookup<'_>,
) -> Result<(Arc<CollectionDefinition>, Document)> {
    let UserLookup {
        pool,
        registry,
        collection,
        email,
        id,
        locale,
    } = lookup;
    let (email, id) = (email.clone(), id.clone());
    let def = load_auth_collection(registry, collection)?;
    let conn = pool.get().context("Failed to get database connection")?;
    let locale_ctx = LocaleContext::default_for(locale);

    if let Some(email) = email {
        // Admin tooling reaches soft-deleted users too (e.g. password recovery
        // for a trashed account) — unlike the HTTP login/reset paths, which
        // exclude them.
        let doc = query::find_by_email(&conn, collection, &def, &email, true, locale_ctx.as_ref())?
            .ok_or_else(|| anyhow!("No user found with email '{email}' in '{collection}'"))?;

        return Ok((def, doc));
    }

    if let Some(id) = id {
        let doc = query::find_by_id(&conn, collection, &def, &id, locale_ctx.as_ref())?
            .ok_or_else(|| anyhow!("No user found with id '{id}' in '{collection}'"))?;

        return Ok((def, doc));
    }

    // Interactive: select from existing users
    select_user_interactive(&conn, collection, &def, locale)
}

/// Interactively select a user from the collection.
#[cfg(not(tarpaulin_include))]
fn select_user_interactive(
    conn: &BoxedConnection,
    collection: &str,
    def: &Arc<CollectionDefinition>,
    locale: &LocaleConfig,
) -> Result<(Arc<CollectionDefinition>, Document)> {
    let users = cli_find(conn, def, &FindQuery::default(), locale)?;

    if users.is_empty() {
        bail!("No users in '{collection}'");
    }

    let labels: Vec<String> = users
        .iter()
        .map(|u| {
            let email = u
                .fields
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            format!("{} — {}", email, u.id)
        })
        .collect();

    if users.len() == 1 {
        cli::info(&format!("Auto-selected only user: {}", labels[0]));
        let doc = users.into_iter().next().expect("guarded by len == 1");

        return Ok((def.clone(), doc));
    }

    let selection = Select::with_theme(&crap_theme())
        .with_prompt("Select user")
        .items(&labels)
        .interact()
        .context("Failed to read user selection")?;

    let user = users
        .into_iter()
        .nth(selection)
        .context("Selected user index out of bounds")?;

    Ok((def.clone(), user))
}

/// Convert a JSON default value to a string.
pub(super) fn default_value_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Whether a field's value is structured (rows or sub-fields), so a value
/// entered for it must be JSON.
pub(super) fn takes_structured_value(field: &FieldDefinition) -> bool {
    matches!(
        field.field_type,
        FieldType::Array | FieldType::Blocks | FieldType::Group
    )
}

/// Whether a field holds a list, so a value that is a JSON array is taken as
/// the list rather than as one text value.
pub(super) fn takes_list_value(field: &FieldDefinition) -> bool {
    field.has_many || field.relationship.as_ref().is_some_and(|rc| rc.has_many)
}

/// The input format a prompt for `field` names, when plain text is not it.
fn value_hint(field: &FieldDefinition) -> Option<&'static str> {
    match field.field_type {
        FieldType::Array | FieldType::Blocks => Some("JSON array of rows"),
        FieldType::Group => Some("JSON object"),
        _ if takes_list_value(field) => Some("JSON array"),
        _ => None,
    }
}

/// Whether the create can't succeed without a value for `field`: it is
/// required, or it is a group holding a required sub-field (a group is filled
/// as one JSON object, so it is prompted as a whole).
///
/// A checkbox never needs one — absent means `false`.
fn needs_value(field: &FieldDefinition) -> bool {
    if field.field_type == FieldType::Checkbox {
        return false;
    }

    if field.required {
        return true;
    }

    field.field_type == FieldType::Group
        && flatten_array_sub_fields(&field.fields)
            .into_iter()
            .any(needs_value)
}

/// How one field the operator didn't pass is filled before the create.
enum FieldFill<'a> {
    /// Optional with a default: filled with the default, silently.
    Default(&'a FieldDefinition, String),
    /// Needed: prompted for, prefilled with the default when there is one.
    Prompt(&'a FieldDefinition, Option<String>),
}

/// The fill for one field, or `None` when it can stay absent.
fn field_fill(field: &FieldDefinition) -> Option<FieldFill<'_>> {
    let default = field.default_value.as_ref().map(default_value_string);

    if needs_value(field) {
        return Some(FieldFill::Prompt(field, default));
    }

    if field.field_type == FieldType::Checkbox {
        return None;
    }

    default.map(|value| FieldFill::Default(field, value))
}

/// Every field not already in `data` the create fills — in definition order,
/// with layout wrappers (Row/Collapsible/Tabs) transparent, so a required
/// field inside one is found just like a top-level one. Email is handled
/// separately; virtual (`Join`) fields are never written.
fn plan_field_fills<'a>(
    def: &'a CollectionDefinition,
    data: &HashMap<String, String>,
) -> Vec<FieldFill<'a>> {
    flatten_array_sub_fields(&def.fields)
        .into_iter()
        .filter(|f| f.name != "email" && f.field_type.is_writable())
        .filter(|f| !data.contains_key(&f.name))
        .filter_map(field_fill)
        .collect()
}

/// Prompt for one needed field, naming the JSON format a structured or list
/// field takes.
#[cfg(not(tarpaulin_include))]
fn prompt_field(field: &FieldDefinition, default: Option<String>) -> Result<String> {
    let label = match value_hint(field) {
        Some(hint) => format!("{} (required, {hint})", field.name),
        None => format!("{} (required)", field.name),
    };

    let theme = crap_theme();
    let mut input = Input::<String>::with_theme(&theme).with_prompt(label);

    if let Some(default) = default {
        input = input.default(default);
    }

    let entered = input
        .interact_text()
        .with_context(|| format!("Failed to read {}", field.name))?;

    if entered.is_empty() {
        bail!("{} is required", field.name);
    }

    Ok(entered)
}

/// Fill every field not already present in the data map (see
/// [`plan_field_fills`]): defaults silently, needed fields by prompting.
#[cfg(not(tarpaulin_include))]
pub(super) fn prompt_required_fields(
    def: &CollectionDefinition,
    data: &mut HashMap<String, String>,
) -> Result<()> {
    for fill in plan_field_fills(def, data) {
        let (field, value) = match fill {
            FieldFill::Default(field, value) => (field, value),
            FieldFill::Prompt(field, default) => (field, prompt_field(field, default)?),
        };

        data.insert(field.name.clone(), value);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, io::Cursor};

    use serde_json::{Value, json};

    use super::*;
    use crate::core::{FieldTab, RelationshipConfig, document::DocumentBuilder};

    #[test]
    fn get_user_email_returns_email_or_unknown() {
        let map: HashMap<String, Value> =
            serde_json::from_value(json!({ "email": "a@example.com" })).unwrap();
        let with_email = DocumentBuilder::new("u1").fields(map).build();
        assert_eq!(get_user_email(&with_email), "a@example.com");

        let without = DocumentBuilder::new("u2").build();
        assert_eq!(get_user_email(&without), "unknown");
    }

    #[test]
    fn default_value_string_unquotes_strings_but_renders_others_as_json() {
        assert_eq!(default_value_string(&json!("hello")), "hello"); // no surrounding quotes
        assert_eq!(default_value_string(&json!(42)), "42");
        assert_eq!(default_value_string(&json!(true)), "true");
        assert_eq!(default_value_string(&Value::Null), "null");
    }

    /// Only the first line is the password; its line ending is not part of it.
    #[test]
    fn password_from_reader_takes_the_first_line_without_its_ending() {
        let read = |input: &str| password_from_reader(Cursor::new(input.as_bytes()));

        assert_eq!(read("s3cret pass\n").unwrap(), "s3cret pass");
        assert_eq!(read("s3cret\r\nignored\n").unwrap(), "s3cret");
        assert_eq!(read("no-newline").unwrap(), "no-newline");
        assert!(read("\n").is_err());
        assert!(read("").is_err());
    }

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn required_text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .required(true)
            .build()
    }

    fn users_with(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("users");
        def.fields = fields;

        def
    }

    /// The names of the fields a plan prompts for.
    fn prompted(def: &CollectionDefinition, data: &HashMap<String, String>) -> Vec<String> {
        plan_field_fills(def, data)
            .into_iter()
            .filter_map(|fill| match fill {
                FieldFill::Prompt(field, _) => Some(field.name.clone()),
                FieldFill::Default(..) => None,
            })
            .collect()
    }

    /// Regression: only top-level fields were considered, so a required field
    /// inside a Row, Collapsible or Tabs wrapper was never prompted for and
    /// the create — including `init`'s first user — failed validation with no
    /// way to supply it interactively.
    #[test]
    fn required_fields_inside_layout_wrappers_are_prompted() {
        let def = users_with(vec![
            FieldDefinition::builder("email", FieldType::Email)
                .required(true)
                .build(),
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![required_text("first_name"), text("nickname")])
                .build(),
            FieldDefinition::builder("more", FieldType::Collapsible)
                .fields(vec![required_text("last_name")])
                .build(),
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new("Profile", vec![required_text("bio")])])
                .build(),
        ]);

        assert_eq!(
            prompted(&def, &HashMap::new()),
            vec!["first_name", "last_name", "bio"]
        );
    }

    /// A field already passed with `-f` is not prompted again, whether it sits
    /// at the top level or inside a wrapper.
    #[test]
    fn a_passed_field_is_not_prompted() {
        let def = users_with(vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![required_text("first_name")])
                .build(),
        ]);
        let data = HashMap::from([("first_name".to_string(), "Ada".to_string())]);

        assert!(prompted(&def, &data).is_empty());
    }

    /// A group holding a required sub-field is prompted as one JSON object;
    /// a group with only optional sub-fields, a checkbox, and a virtual join
    /// field are not.
    #[test]
    fn a_group_with_a_required_sub_field_is_prompted_whole() {
        let def = users_with(vec![
            FieldDefinition::builder("address", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("row", FieldType::Row)
                        .fields(vec![required_text("city")])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![text("note")])
                .build(),
            FieldDefinition::builder("agreed", FieldType::Checkbox)
                .required(true)
                .build(),
            FieldDefinition::builder("posts", FieldType::Join)
                .required(true)
                .build(),
        ]);

        assert_eq!(prompted(&def, &HashMap::new()), vec!["address"]);
    }

    /// An optional field with a default is filled silently, even inside a
    /// wrapper.
    #[test]
    fn an_optional_default_inside_a_wrapper_is_filled_silently() {
        let def = users_with(vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("role", FieldType::Text)
                        .default_value(json!("editor"))
                        .build(),
                ])
                .build(),
        ]);

        let fills = plan_field_fills(&def, &HashMap::new());

        assert!(
            matches!(
                fills.as_slice(),
                [FieldFill::Default(field, value)] if field.name == "role" && value == "editor"
            ),
            "the default is filled without a prompt"
        );
    }

    /// Structured and list fields name the JSON format their prompt takes.
    #[test]
    fn prompts_name_the_json_format_of_structured_and_list_fields() {
        let array = FieldDefinition::builder("links", FieldType::Array).build();
        let group = FieldDefinition::builder("address", FieldType::Group).build();
        let tags = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();
        let roles = FieldDefinition::builder("roles", FieldType::Select)
            .has_many(true)
            .build();

        assert_eq!(value_hint(&array), Some("JSON array of rows"));
        assert_eq!(value_hint(&group), Some("JSON object"));
        assert_eq!(value_hint(&tags), Some("JSON array"));
        assert_eq!(value_hint(&roles), Some("JSON array"));
        assert_eq!(value_hint(&text("name")), None);
    }
}
