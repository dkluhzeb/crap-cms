//! `make hook` command — generate hook Lua files.

mod generator;

pub use generator::{ConditionFieldInfo, HookType, MakeHookOptions, make_hook};

#[cfg(test)]
mod tests;
