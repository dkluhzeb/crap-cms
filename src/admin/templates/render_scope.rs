//! The database access level in force for the render running on this thread.
//!
//! `before_render` receives its [`RenderCrud`] as an argument, because the
//! admin handler calls it directly. A `crap.template_data` function cannot:
//! it is invoked by the `{{data "name"}}` Handlebars helper from deep inside
//! `Handlebars::render`, and the helper is registered once at startup with no
//! per-request state to carry an access level in.
//!
//! So the authenticated render path enters a [`RenderScope`] around the whole
//! render, and the helper reads it back out. A thread-local rather than a
//! `tokio` task-local because that render runs on a `spawn_blocking` thread,
//! outside the task the request lives in — the same reason the slot helper
//! tracks its recursion guard per thread.
//!
//! **Fails closed.** A render with no scope entered — an error page, an
//! unauthenticated page, a unit test rendering a template directly — reports
//! [`RenderCrud::none`]: no identity, no database.

use std::cell::RefCell;

use crate::hooks::lifecycle::RenderCrud;

thread_local! {
    /// Access level for the render in progress on this thread, if any.
    static ACTIVE: RefCell<Option<RenderCrud>> = const { RefCell::new(None) };
}

/// RAII scope: installs `crud` for the current thread and puts back whatever
/// was there before on drop, including when the render unwinds. Without the
/// restore, a blocking thread returned to the pool would hand the next,
/// possibly unauthenticated, render the previous viewer's identity.
///
/// Save-and-restore rather than clear-on-drop, matching the stack discipline
/// [`TxContextGuard`](crate::hooks::lifecycle::TxContextGuard) uses: nothing
/// nests scopes today, but a future nested render must not tear down its
/// parent's access level when it finishes.
pub(crate) struct RenderScope(Option<RenderCrud>);

impl RenderScope {
    /// Enter a render scope on this thread.
    pub(crate) fn enter(crud: RenderCrud) -> Self {
        Self(ACTIVE.with_borrow_mut(|active| active.replace(crud)))
    }
}

impl Drop for RenderScope {
    fn drop(&mut self) {
        let previous = self.0.take();

        ACTIVE.with_borrow_mut(|active| *active = previous);
    }
}

/// The access level for the render in progress, or [`RenderCrud::none`] when
/// this thread is not inside a [`RenderScope`].
pub(crate) fn current() -> RenderCrud {
    ACTIVE
        .with_borrow(Clone::clone)
        .unwrap_or_else(RenderCrud::none)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_a_scope_there_is_no_access() {
        assert!(
            matches!(current(), RenderCrud::None { user: None, .. }),
            "an unscoped render must get no identity and no database"
        );
    }

    #[test]
    fn a_scope_is_visible_while_it_is_held_and_gone_after() {
        {
            let _scope = RenderScope::enter(RenderCrud::None {
                user: None,
                ui_locale: Some("de".to_string()),
            });

            let RenderCrud::None { ui_locale, .. } = current() else {
                panic!("the entered scope should be the active one");
            };
            assert_eq!(ui_locale.as_deref(), Some("de"));
        }

        assert!(
            matches!(
                current(),
                RenderCrud::None {
                    ui_locale: None,
                    ..
                }
            ),
            "the scope must be cleared on drop so a pooled thread leaks nothing"
        );
    }

    #[test]
    fn a_nested_scope_restores_its_parent_rather_than_clearing_it() {
        let _outer = RenderScope::enter(RenderCrud::None {
            user: None,
            ui_locale: Some("outer".to_string()),
        });

        {
            let _inner = RenderScope::enter(RenderCrud::None {
                user: None,
                ui_locale: Some("inner".to_string()),
            });

            let RenderCrud::None { ui_locale, .. } = current() else {
                unreachable!()
            };
            assert_eq!(ui_locale.as_deref(), Some("inner"));
        }

        let RenderCrud::None { ui_locale, .. } = current() else {
            unreachable!()
        };
        assert_eq!(
            ui_locale.as_deref(),
            Some("outer"),
            "the inner scope must restore its parent, not clear the slot"
        );
    }

    /// A render that panics must not leave its viewer behind for whatever
    /// renders on this thread next.
    #[test]
    fn an_unwinding_render_clears_its_scope() {
        let panicked = std::panic::catch_unwind(|| {
            let _scope = RenderScope::enter(RenderCrud::None {
                user: None,
                ui_locale: Some("de".to_string()),
            });

            panic!("render blew up");
        });

        assert!(panicked.is_err());
        assert!(
            matches!(
                current(),
                RenderCrud::None {
                    ui_locale: None,
                    ..
                }
            ),
            "the scope must be cleared on the unwind path too"
        );
    }
}
