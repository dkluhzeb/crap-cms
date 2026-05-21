//! End-to-end tests for the `#[lua_fn]` attribute macro.
//!
//! Verifies that a function marked `#[lua_fn]` (a) compiles into a
//! correct `LuaFnSpec` const, (b) generates a working `_register`
//! wrapper that produces a callable Lua function, and (c) the wrapper
//! correctly handles both stateless and stateful signatures.
//!
//! Lives as a sibling test module rather than under `#[cfg(test)]` in
//! each generated-API file so the macro's behaviour can be pinned with
//! exhaustive fixture coverage in one place.

#![cfg(test)]
#![allow(dead_code)]
// `#[lua_fn]` requires `LuaResult<T>` return signatures so the wrapper can
// build the closure. The fixture fns here never produce errors (they're
// pure unit-test seeds), so `unnecessary_wraps` fires on each one. Can't
// drop the `Result` without losing macro compatibility.
#![allow(clippy::unnecessary_wraps)]
// mlua's `FromLuaMulti` for borrowed types (`&str`) is awkward due to
// lifetime constraints on the Lua VM-owned data; the convention for
// `#[lua_fn]` params is owned types (`String`, `Table`, etc.) even when
// the body only reads them.
#![allow(clippy::needless_pass_by_value)]

use super::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};
use mlua::{Lua, Result as LuaResult};

// ── Stateless fns ─────────────────────────────────────────────────

/// Reverse a string. Used in `lua_fn` tests.
#[lua_fn(path = "test.util.reverse")]
fn reverse_str(_: &Lua, s: String) -> LuaResult<String> {
    Ok(s.chars().rev().collect())
}

#[lua_fn(path = "test.util.add")]
fn add_two(_: &Lua, a: i64, b: i64) -> LuaResult<i64> {
    Ok(a + b)
}

#[lua_fn(path = "test.util.empty")]
fn returns_unit(_: &Lua) -> LuaResult<()> {
    Ok(())
}

// ── Stateful fn ───────────────────────────────────────────────────

struct CountState {
    base: i64,
}

#[lua_fn(path = "test.util.add_with_base")]
fn add_with_base(state: &CountState, _: &Lua, n: i64) -> LuaResult<i64> {
    Ok(state.base + n)
}

// ── Spec coverage ─────────────────────────────────────────────────

#[test]
fn spec_const_carries_path() {
    assert_eq!(REVERSE_STR_SPEC.path, "test.util.reverse");
}

#[test]
fn spec_const_carries_doc() {
    assert_eq!(
        REVERSE_STR_SPEC.doc,
        &["Reverse a string. Used in `lua_fn` tests."]
    );
}

#[test]
fn spec_const_maps_string_param() {
    assert_eq!(REVERSE_STR_SPEC.params.len(), 1);
    assert_eq!(REVERSE_STR_SPEC.params[0].name, "s");
    assert_eq!(REVERSE_STR_SPEC.params[0].ty, "string");
}

#[test]
fn spec_const_maps_two_integer_params() {
    assert_eq!(ADD_TWO_SPEC.params.len(), 2);
    assert_eq!(ADD_TWO_SPEC.params[0].name, "a");
    assert_eq!(ADD_TWO_SPEC.params[0].ty, "integer");
    assert_eq!(ADD_TWO_SPEC.params[1].name, "b");
    assert_eq!(ADD_TWO_SPEC.params[1].ty, "integer");
}

#[test]
fn spec_const_unit_return_is_none() {
    assert!(RETURNS_UNIT_SPEC.returns.is_none());
}

#[test]
fn spec_const_string_return() {
    let r = REVERSE_STR_SPEC.returns.as_ref().expect("string return");
    assert_eq!(r.ty, "string");
}

#[test]
fn spec_const_integer_return() {
    let r = ADD_TWO_SPEC.returns.as_ref().expect("integer return");
    assert_eq!(r.ty, "integer");
}

#[test]
fn stateful_spec_strips_state_and_lua_from_params() {
    // State arg (`&CountState`) and `&Lua` are both skipped — only `n` shows up.
    assert_eq!(ADD_WITH_BASE_SPEC.params.len(), 1);
    assert_eq!(ADD_WITH_BASE_SPEC.params[0].name, "n");
    assert_eq!(ADD_WITH_BASE_SPEC.params[0].ty, "integer");
}

// ── Register wrapper end-to-end ───────────────────────────────────

#[test]
fn stateless_register_produces_callable_function() {
    let lua = Lua::new();
    let f = reverse_str_register(&lua, std::sync::Arc::new(())).unwrap();
    let result: String = f.call("hello").unwrap();
    assert_eq!(result, "olleh");
}

#[test]
fn stateless_two_arg_register_works() {
    let lua = Lua::new();
    let f = add_two_register(&lua, std::sync::Arc::new(())).unwrap();
    let result: i64 = f.call((40, 2)).unwrap();
    assert_eq!(result, 42);
}

#[test]
fn stateful_register_threads_state_into_call() {
    let lua = Lua::new();
    let state = std::sync::Arc::new(CountState { base: 100 });
    let f = add_with_base_register(&lua, state).unwrap();
    let result: i64 = f.call(7).unwrap();
    assert_eq!(result, 107);
}

#[test]
fn unit_return_register_works() {
    let lua = Lua::new();
    let f = returns_unit_register(&lua, std::sync::Arc::new(())).unwrap();
    let _: () = f.call(()).unwrap();
}

// ── lua_table! end-to-end ─────────────────────────────────────────

lua_table! {
    name: test_util_table,
    path: "test.util",
    state: (),
    fns: [reverse_str, add_two, returns_unit],
}

#[test]
fn lua_table_registers_all_fns_at_dotted_path() {
    let lua = Lua::new();
    register_test_util_table(&lua, ()).unwrap();

    let rev: String = lua
        .load(r#"return test.util.reverse("hello")"#)
        .eval()
        .unwrap();
    assert_eq!(rev, "olleh");

    let sum: i64 = lua.load("return test.util.add(10, 32)").eval().unwrap();
    assert_eq!(sum, 42);

    // returns_unit returns no value — confirm callable, ignore result
    lua.load("test.util.empty()").exec().unwrap();
}

#[test]
fn lua_table_render_emits_all_fns_in_declaration_order() {
    let mut out = String::new();
    render_test_util_table_lua(&mut out);

    // Each fn's block appears in the order listed in `fns: [...]`.
    let reverse_idx = out
        .find("function test.util.reverse")
        .expect("reverse missing");
    let add_idx = out.find("function test.util.add").expect("add missing");
    let empty_idx = out.find("function test.util.empty").expect("empty missing");
    assert!(reverse_idx < add_idx);
    assert!(add_idx < empty_idx);

    // Doc comments are forwarded
    assert!(out.contains("--- Reverse a string. Used in `lua_fn` tests."));

    // Param + return annotations present
    assert!(out.contains("--- @param s string"));
    assert!(out.contains("--- @return string"));
    assert!(out.contains("--- @param a integer"));
    assert!(out.contains("--- @param b integer"));
}

// ── Stateful table ────────────────────────────────────────────────

lua_table! {
    name: test_stateful_table,
    path: "test.with_state",
    state: CountState,
    fns: [add_with_base],
}

#[test]
fn lua_table_threads_state_to_stateful_fn() {
    let lua = Lua::new();
    register_test_stateful_table(&lua, CountState { base: 1000 }).unwrap();
    let result: i64 = lua
        .load("return test.with_state.add_with_base(23)")
        .eval()
        .unwrap();
    assert_eq!(result, 1023);
}

// ── Nested table path ─────────────────────────────────────────────

#[lua_fn(path = "test.nested.deep.greet")]
fn deep_greet(_: &Lua, name: String) -> LuaResult<String> {
    Ok(format!("hello, {name}"))
}

lua_table! {
    name: test_deep_table,
    path: "test.nested.deep",
    state: (),
    fns: [deep_greet],
}

#[test]
fn lua_table_creates_nested_path_chain() {
    let lua = Lua::new();
    register_test_deep_table(&lua, ()).unwrap();
    let result: String = lua
        .load(r#"return test.nested.deep.greet("alice")"#)
        .eval()
        .unwrap();
    assert_eq!(result, "hello, alice");
}
