//! The `ctx.operation` values each hook context carries — the one list the
//! runtime passes and the generated Lua types (`crap.HookContext`,
//! `crap.hook.*`, `crap.read_hook.*`) are built from.

use crate::core::event::EventOperation;

/// A collection write's hooks (`before_validate`, `before_change`,
/// `after_change`): the field writes, and an undelete restoring a trashed
/// document. Unpublishing is an `update` to a hook.
pub const COLLECTION_WRITE_OPERATIONS: &[&str] = &["create", "update", "undelete"];

/// A delete's hooks (`before_delete`, `after_delete`).
pub const DELETE_OPERATION: &str = "delete";

/// A collection read's hooks (`before_read`, and `after_read` outside a live
/// event).
pub const COLLECTION_READ_OPERATIONS: &[&str] = &["find", "find_by_id"];

/// A global write's hooks — every global write, unpublish included, is an
/// `update` to a hook.
pub const GLOBAL_WRITE_OPERATION: &str = "update";

/// A global read's hooks.
pub const GLOBAL_READ_OPERATION: &str = "get";

/// The `on_init` system hooks.
pub const INIT_OPERATION: &str = "init";

/// Append each value of `values` not yet in `out`, keeping first-seen order.
fn push_unique<'a>(out: &mut Vec<&'a str>, values: impl IntoIterator<Item = &'a str>) {
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
}

/// The spellings of `ops`.
fn event_names(ops: &[EventOperation]) -> impl Iterator<Item = &'static str> + '_ {
    ops.iter().map(EventOperation::as_str)
}

/// Every operation a collection's write, delete and `before_read` hooks see
/// (`crap.hook.<Slug>`).
#[must_use]
pub fn collection_hook_operations() -> Vec<&'static str> {
    let mut out = Vec::new();

    push_unique(&mut out, COLLECTION_WRITE_OPERATIONS.iter().copied());
    push_unique(&mut out, [DELETE_OPERATION]);
    push_unique(&mut out, COLLECTION_READ_OPERATIONS.iter().copied());

    out
}

/// Every operation a collection's `after_read` sees (`crap.read_hook.<Slug>`):
/// the reads, and the operation of every live event it shapes.
#[must_use]
pub fn collection_read_hook_operations() -> Vec<&'static str> {
    let mut out = Vec::new();

    push_unique(&mut out, COLLECTION_READ_OPERATIONS.iter().copied());
    push_unique(&mut out, event_names(&EventOperation::ALL));

    out
}

/// Every operation a global's write and `before_read` hooks see
/// (`crap.hook.global_<slug>`).
#[must_use]
pub fn global_hook_operations() -> Vec<&'static str> {
    vec![GLOBAL_WRITE_OPERATION, GLOBAL_READ_OPERATION]
}

/// Every operation a global's `after_read` sees
/// (`crap.read_hook.global_<slug>`): the read, and the operation of every
/// live event a global publishes.
#[must_use]
pub fn global_read_hook_operations() -> Vec<&'static str> {
    let mut out = vec![GLOBAL_READ_OPERATION];

    push_unique(&mut out, event_names(&EventOperation::GLOBAL));

    out
}

/// Every operation any hook context can carry (`crap.HookContext`): the
/// collection and global hooks, `before_broadcast` (every live event) and
/// `on_init`.
#[cfg(test)]
#[must_use]
pub fn hook_context_operations() -> Vec<&'static str> {
    let mut out = collection_hook_operations();

    push_unique(&mut out, event_names(&EventOperation::ALL));
    push_unique(&mut out, [GLOBAL_READ_OPERATION, INIT_OPERATION]);

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A collection hook names its writes, the delete and its reads; the
    /// state-change undelete is among the writes.
    #[test]
    fn collection_hooks_see_every_write_and_read() {
        assert_eq!(
            collection_hook_operations(),
            [
                "create",
                "update",
                "undelete",
                "delete",
                "find",
                "find_by_id"
            ]
        );
    }

    /// `after_read` also shapes live events, so it names every event
    /// operation — `unpublish` and `restore` included.
    #[test]
    fn read_hooks_include_every_event_operation() {
        let collection = collection_read_hook_operations();
        let global = global_read_hook_operations();

        for op in EventOperation::ALL {
            assert!(collection.contains(&op.as_str()), "{op:?}");
        }

        for op in EventOperation::GLOBAL {
            assert!(global.contains(&op.as_str()), "{op:?}");
        }

        assert_eq!(global, ["get", "update", "unpublish", "restore"]);
    }

    /// The static context names every operation every narrower context does.
    #[test]
    fn hook_context_covers_every_narrower_list() {
        let all = hook_context_operations();

        for op in collection_hook_operations()
            .into_iter()
            .chain(collection_read_hook_operations())
            .chain(global_hook_operations())
            .chain(global_read_hook_operations())
        {
            assert!(all.contains(&op), "{op}");
        }

        assert!(all.contains(&INIT_OPERATION));
    }
}
