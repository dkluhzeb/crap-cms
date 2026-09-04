//! Hook execution: collection-level orchestration, `after_read` pipeline,
//! `before_broadcast` variants, field-level hook walker, and display conditions.

mod after_read;
mod broadcast;
mod display;
mod field_hooks;
mod runtime;

pub use after_read::AfterReadCtx;
pub(crate) use after_read::apply_after_read_inner;
pub(crate) use broadcast::{call_before_broadcast_hook, call_registered_before_broadcast};
pub(crate) use display::call_display_condition_with_lua;
pub(crate) use field_hooks::{has_field_hooks_for_event, run_field_hooks_inner};
pub(crate) use runtime::{
    call_hook_ref, call_registered_hooks, get_hook_refs, has_registered_hooks,
    resolve_hook_function, run_hooks_inner, scan_has_template_data, scan_registered_events,
};
