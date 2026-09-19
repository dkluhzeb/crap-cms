//! Boolean-only verdicts. Some Lua rules gate a single yes/no decision with
//! no rows behind it — a custom route's access gate, a collection's live
//! broadcast filter — so the filter-table form a collection access rule may
//! return has nothing to narrow. A table from such a rule is a configuration
//! error reported to the author, never silently read as "yes" or "no".

use anyhow::{Result, bail};
use mlua::Value;
use tracing::warn;

/// Interpret the return value of a boolean-only rule: `true` is yes,
/// `false`/`nil` is no, a table is a configuration error (`subject` names
/// the rule's kind in the message, e.g. "custom route"), and any other type
/// is no with a warning — the same fail-closed reading collection access
/// rules apply to an unexpected type.
///
/// # Errors
///
/// Returns an error when the rule returned a table.
pub(crate) fn boolean_verdict(value: &Value, subject: &str) -> Result<bool> {
    match value {
        Value::Boolean(true) => Ok(true),
        Value::Boolean(false) | Value::Nil => Ok(false),
        Value::Table(_) => bail!(
            "{subject} rule returned a table; a {subject} rule decides yes or no and has no \
             rows for a filter to narrow — return true or false instead"
        ),
        other => {
            warn!(
                "{subject} rule returned unexpected type '{}', treating it as false",
                other.type_name()
            );

            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use mlua::Lua;

    use super::*;

    fn verdict(code: &str) -> Result<bool> {
        let lua = Lua::new();
        let value: Value = lua.load(code).eval().unwrap();
        boolean_verdict(&value, "custom route")
    }

    #[test]
    fn true_allows() {
        assert!(verdict("return true").unwrap());
    }

    #[test]
    fn false_and_nil_deny() {
        assert!(!verdict("return false").unwrap());
        assert!(!verdict("return nil").unwrap());
    }

    /// A filter table (the collection-access idiom) is a configuration error
    /// naming the fix — it used to count as an allow on custom routes.
    #[test]
    fn table_is_a_configuration_error() {
        let err = verdict("return { id = 1 }").unwrap_err().to_string();
        assert!(err.contains("custom route"), "{err}");
        assert!(err.contains("true or false"), "{err}");
    }

    #[test]
    fn other_types_deny() {
        assert!(!verdict("return 1").unwrap());
        assert!(!verdict("return 'yes'").unwrap());
    }
}
