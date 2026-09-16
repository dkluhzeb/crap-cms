//! The inputs an auth collection's form adds on top of its declared fields.
//!
//! `password` and `_locked` are not field definitions — no schema declares
//! them — so every form that shows them has to synthesize their contexts. They
//! are built here once, so the create form, the edit form and the error
//! re-render can't show three different sets.

use serde_json::{Map, Value};

use crate::admin::context::field::{
    BaseFieldData, CheckboxField, ConditionData, FieldContext, TextField, ValidationAttrs,
};

/// The [`BaseFieldData`] for an input that has no field definition behind it.
fn auth_field_base(
    name: &str,
    label: &str,
    description: Option<&str>,
    required: bool,
) -> BaseFieldData {
    BaseFieldData {
        name: name.to_string(),
        field_name: name.to_string(),
        label: label.to_string(),
        required,
        value: Value::String(String::new()),
        placeholder: None,
        description: description.map(str::to_string),
        readonly: false,
        localized: false,
        locale_locked: false,
        position: None,
        template: None,
        extra: Map::new(),
        error: None,
        validation: ValidationAttrs::default(),
        condition: ConditionData::default(),
    }
}

/// The password input an auth-collection form shows.
///
/// On create a password is required; on edit it is optional and a blank box
/// keeps the stored one. The box is always blank — a submitted password is
/// never echoed back into the HTML, not even when re-rendering the form the
/// user just submitted.
pub(in crate::admin::handlers::collections) fn password_field(creating: bool) -> FieldContext {
    let description = if creating {
        "set_password_description"
    } else {
        "leave_blank_keep_password"
    };

    FieldContext::Password(TextField {
        base: auth_field_base("password", "password", Some(description), creating),
        has_many: None,
        tags: None,
    })
}

/// The lock checkbox an auth-collection edit form shows.
///
/// The update path reads an absent `_locked` as an explicit unlock, so any form
/// that can post an update has to render this box — leaving it out of a
/// re-render would unlock the account on the next save.
pub(in crate::admin::handlers::collections) fn locked_field(locked: bool) -> FieldContext {
    FieldContext::Checkbox(CheckboxField {
        base: auth_field_base("_locked", "account_locked", Some("prevent_login"), false),
        checked: locked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_password_box_is_required_only_on_create() {
        let creating = password_field(true);
        assert!(creating.base().required);
        assert_eq!(
            creating.base().description.as_deref(),
            Some("set_password_description")
        );

        let editing = password_field(false);
        assert!(!editing.base().required);
        assert_eq!(
            editing.base().description.as_deref(),
            Some("leave_blank_keep_password")
        );
    }

    /// A submitted password is never rendered back into the form.
    #[test]
    fn the_password_box_is_always_blank() {
        for creating in [true, false] {
            let blank = Value::String(String::new());
            assert_eq!(password_field(creating).base().value, blank);
        }
    }

    #[test]
    fn the_lock_box_reflects_the_lock_state() {
        let FieldContext::Checkbox(locked) = locked_field(true) else {
            panic!("expected a checkbox")
        };
        assert!(locked.checked);
        assert_eq!(locked.base.name, "_locked");

        let FieldContext::Checkbox(unlocked) = locked_field(false) else {
            panic!("expected a checkbox")
        };
        assert!(!unlocked.checked);
    }
}
