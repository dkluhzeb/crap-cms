//! Access outcomes on a global: allow/deny only.

use crate::{db::AccessResult, service::ServiceError};

/// The error a global's access hook raises by returning a filter table. A
/// global is a single row, so there is nothing a row constraint could select:
/// every surface treats it as a configuration error rather than guess.
#[must_use]
pub(crate) fn reject_global_filter(slug: &str) -> ServiceError {
    ServiceError::HookError(format!(
        "Access hook for global '{slug}' returned a filter table; globals don't support \
         filter-based access — return true/false based on ctx.user fields instead."
    ))
}

/// Whether `access` — a global `slug`'s access outcome — allows the
/// operation. The one mapping every global surface (service and admin page
/// alike) applies, so none can read a filter table as "allowed".
///
/// # Errors
///
/// Returns [`reject_global_filter`]'s error for a `Constrained` outcome.
pub fn global_access_allowed(access: &AccessResult, slug: &str) -> Result<bool, ServiceError> {
    match access {
        AccessResult::Allowed => Ok(true),
        AccessResult::Denied => Ok(false),
        AccessResult::Constrained(_) => Err(reject_global_filter(slug)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Filter, FilterClause, FilterOp};

    #[test]
    fn a_filter_table_is_a_configuration_error_not_an_allow() {
        let constrained = AccessResult::Constrained(vec![FilterClause::Single(Filter {
            field: "id".to_string(),
            op: FilterOp::Equals("default".to_string()),
        })]);

        assert!(global_access_allowed(&AccessResult::Allowed, "site").unwrap());
        assert!(!global_access_allowed(&AccessResult::Denied, "site").unwrap());

        let err = global_access_allowed(&constrained, "site").unwrap_err();
        assert!(matches!(err, ServiceError::HookError(ref m) if m.contains("'site'")));
    }
}
