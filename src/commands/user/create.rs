//! `user create` — create a new user in an auth collection.

use std::{collections::HashMap, path::Path, slice, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use dialoguer::Input;
use serde_json::Value;

use crate::{
    cli::{self, crap_theme},
    commands::cli_infra,
    config::CrapConfig,
    core::{CollectionDefinition, Document, DocumentFields, Registry, find_field},
    db::{DbPool, LocaleContext},
    hooks::lifecycle::is_valid_email_format,
    service::{self, AppInfra, CreateManyItem, CreateManyOptions, ServiceContext, ServiceError},
};

use super::helpers::{
    load_auth_collection, prompt_required_fields, resolve_new_password, takes_list_value,
    takes_structured_value,
};

/// Validate an email address string for CLI input.
/// Returns a human-readable error referencing the offending email.
fn validate_email_input(email: &str) -> Result<()> {
    if is_valid_email_format(email) {
        return Ok(());
    }

    Err(anyhow!(
        "invalid email address '{email}': must contain '@', non-empty local and domain parts, and a dot in the domain"
    ))
}

/// The typed value of one `-f key=value` pair for `def`.
///
/// The key is looked up through layout wrappers (Row/Collapsible/Tabs are
/// transparent), so a field inside one is typed like a top-level field.
///
/// Every `-f` value arrives as text. An array, blocks or group field needs
/// its rows or sub-fields, so its value must be JSON; a list field (a
/// has-many relationship, upload or scalar) takes a JSON array as the list,
/// and otherwise its text — a has-many relationship also accepts comma-
/// separated ids. Every other value stays text; the service write coerces it
/// to the field's type and validates it.
fn typed_value(def: &CollectionDefinition, key: &str, raw: String) -> Result<Value> {
    let Some(field) = find_field(key, &def.fields) else {
        return Ok(Value::String(raw));
    };

    if takes_structured_value(field) {
        return serde_json::from_str(&raw).with_context(|| {
            format!("-f {key}: a {} field takes JSON", field.field_type.as_str())
        });
    }

    if takes_list_value(field)
        && let Ok(list @ Value::Array(_)) = serde_json::from_str(&raw)
    {
        return Ok(list);
    }

    Ok(Value::String(raw))
}

/// Type every entered field value for `def` (see [`typed_value`]).
fn typed_fields(
    def: &CollectionDefinition,
    data: HashMap<String, String>,
) -> Result<DocumentFields> {
    data.into_iter()
        .map(|(key, raw)| {
            let value = typed_value(def, &key, raw)?;

            Ok((key, value))
        })
        .collect()
}

/// Args for [`user_create`]. Bundles the dispatch-time inputs from the
/// `UserAction::Create` clap variant alongside the open project's handles.
pub struct UserCreateParams<'a> {
    pub pool: &'a DbPool,
    pub registry: &'a Arc<Registry>,
    pub config: &'a CrapConfig,
    pub config_dir: &'a Path,
    pub collection: &'a str,
    pub email: Option<String>,
    pub password: Option<String>,
    pub password_stdin: bool,
    pub fields: Vec<(String, String)>,
}

/// Create the user through the service write, like every other surface:
/// validation, the password policy, join-table data (has-many and array
/// values), version snapshot, reference counting and locking, search index,
/// live event and — on a `verify_email` collection — the verification email
/// all apply. Lifecycle hooks don't run (a bootstrap tool: the first user is
/// created before any hook can rely on one); the live event is published like
/// any write's, so its `live` filter and `before_broadcast` hooks do. Collection
/// access rules don't apply to the operator's CLI.
fn create_through_service(
    infra: &AppInfra,
    def: &CollectionDefinition,
    item: &CreateManyItem,
    config: &CrapConfig,
) -> Result<Document> {
    let ctx = ServiceContext::collection(&def.slug, def)
        .infra(infra)
        .override_access(true)
        .build();

    // One item through the bulk create: the create path whose options can
    // switch lifecycle hooks off while keeping validation.
    let opts = CreateManyOptions {
        run_hooks: false,
        locale_ctx: LocaleContext::default_for(&config.locale),
        ..CreateManyOptions::default()
    };

    let mut result = service::create_many(&ctx, slice::from_ref(item), &opts)
        .map_err(ServiceError::into_anyhow)
        .context("Failed to create user")?;

    result
        .documents
        .pop()
        .ok_or_else(|| anyhow!("The create returned no document"))
}

/// Create a new user in an auth collection.
///
/// # Errors
///
/// Returns an error if the collection isn't an auth collection, email or
/// password validation fails, a configured Redis can't be reached, a `-f`
/// value is invalid for its field, or the service write fails.
#[cfg(not(tarpaulin_include))]
pub fn user_create(p: UserCreateParams<'_>) -> Result<()> {
    let def = load_auth_collection(p.registry, p.collection)?;

    let email = resolve_email(p.email)?;
    validate_email_input(&email)?;

    // Built — and a configured Redis reached — before the password prompt, so
    // a create that couldn't reach `serve` fails before anything is typed.
    let infra = cli_infra(p.config_dir, p.registry, p.config, p.pool)?;

    let password = resolve_new_password(p.password, p.password_stdin, "Password")?;

    let mut data: HashMap<String, String> = p.fields.into_iter().collect();
    data.insert("email".to_string(), email);

    prompt_required_fields(&def, &mut data)?;

    let item = CreateManyItem {
        data: typed_fields(&def, data)?,
        password: Some(password),
    };

    let doc = create_through_service(&infra, &def, &item, p.config)?;

    cli::success(&format!("Created user {} in '{}'", doc.id, p.collection));

    Ok(())
}

/// Resolve email from CLI flag or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_email(email: Option<String>) -> Result<String> {
    match email {
        Some(e) => Ok(e),
        None => Input::with_theme(&crap_theme())
            .with_prompt("Email")
            .interact_text()
            .context("Failed to read email"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{FieldDefinition, FieldTab, FieldType, RelationshipConfig};

    #[test]
    fn cli_user_create_rejects_malformed_email() {
        let err = validate_email_input("not-an-email").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not-an-email"),
            "error should reference offending value: {msg}"
        );
        assert!(
            msg.contains("invalid email"),
            "error should say invalid email: {msg}"
        );

        assert!(validate_email_input("user@nodot").is_err());
        assert!(validate_email_input("@nolocal.com").is_err());
        assert!(validate_email_input("user@").is_err());
        assert!(validate_email_input("user@example.com").is_ok());
    }

    fn users_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("users");
        def.fields = vec![
            FieldDefinition::builder("name", FieldType::Text).build(),
            FieldDefinition::builder("roles", FieldType::Select)
                .has_many(true)
                .build(),
            FieldDefinition::builder("teams", FieldType::Relationship)
                .relationship(RelationshipConfig::new("teams", true))
                .build(),
            FieldDefinition::builder("links", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("url", FieldType::Text).build(),
                ])
                .build(),
        ];

        def
    }

    /// Regression: every `-f` value was stored as text, so an array's rows
    /// and a list's elements were dropped. Structured and list fields now
    /// take JSON.
    #[test]
    fn field_values_are_typed_by_their_field() {
        let def = users_def();

        assert_eq!(
            typed_value(&def, "name", "Ada".into()).unwrap(),
            json!("Ada")
        );
        assert_eq!(
            typed_value(&def, "roles", r#"["admin","editor"]"#.into()).unwrap(),
            json!(["admin", "editor"])
        );
        assert_eq!(
            typed_value(&def, "teams", "t1,t2".into()).unwrap(),
            json!("t1,t2"),
            "a has-many relationship keeps comma-separated ids"
        );
        assert_eq!(
            typed_value(&def, "links", r#"[{"url":"https://a.example"}]"#.into()).unwrap(),
            json!([{ "url": "https://a.example" }])
        );
    }

    /// An array value that isn't JSON is an error naming the field, not a
    /// silently dropped value.
    #[test]
    fn a_structured_field_rejects_non_json() {
        let err = typed_value(&users_def(), "links", "not json".into()).unwrap_err();

        assert!(format!("{err:#}").contains("-f links"), "{err:#}");
    }

    /// A text value that happens to look like a JSON array stays text on a
    /// field that isn't a list.
    #[test]
    fn a_scalar_field_keeps_text_that_looks_like_json() {
        assert_eq!(
            typed_value(&users_def(), "name", "[1]".into()).unwrap(),
            json!("[1]")
        );
    }

    /// Regression: the `-f` key was looked up among top-level fields only, so
    /// an array or list inside a Row, Collapsible or Tabs wrapper had its JSON
    /// kept as text and its rows dropped.
    #[test]
    fn field_values_inside_layout_wrappers_are_typed_by_their_field() {
        let mut def = CollectionDefinition::new("users");
        def.fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("roles", FieldType::Select)
                        .has_many(true)
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Links",
                    vec![
                        FieldDefinition::builder("links", FieldType::Array)
                            .fields(vec![
                                FieldDefinition::builder("url", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];

        assert_eq!(
            typed_value(&def, "roles", r#"["admin"]"#.into()).unwrap(),
            json!(["admin"])
        );
        assert_eq!(
            typed_value(&def, "links", r#"[{"url":"https://a.example"}]"#.into()).unwrap(),
            json!([{ "url": "https://a.example" }])
        );
        assert!(
            typed_value(&def, "links", "not json".into()).is_err(),
            "a wrapped array still requires JSON"
        );
    }
}
