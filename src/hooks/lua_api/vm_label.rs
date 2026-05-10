//! Per-VM identification label, stored in `Lua::app_data` so logging can
//! attribute output to the right VM (init / pool worker N).

/// Label stored in `Lua::app_data` to identify which VM is logging.
/// Init VM uses `"init"`, pool VMs use `"vm-1"`, `"vm-2"`, etc.
pub struct VmLabel(pub String);
