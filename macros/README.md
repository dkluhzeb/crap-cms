# crap-cms-macros

Proc-macro crate for crap-cms typegen. Drives `types/crap.lua`
generation from Rust source — every `--- @class` / `--- @alias` /
`function crap.X.Y(...)` block in the shipped Lua type file comes from
one of the macros here.

## Why a separate crate

Rust requires proc-macros to live in their own crate
(`[lib] proc-macro = true`). The trait targets the macros emit
references (`LuaAnnotation`, `LuaAlias`, etc.) live in the main crate
at `crate::typegen::lua::annotation` so consumers can `use
crate::typegen::lua::LuaAnnotation` and the derive output's
`<T as LuaAnnotation>::CLASS_NAME` resolves correctly.

The macros emit absolute paths under `crate::typegen::lua::*` rather
than relative paths. The main crate's `extern crate self as crap_cms`
shim lets `crate::...` resolve from inside generated impls.

## What's here

| Macro / derive          | Apply to                              | Emits                                               |
|-------------------------|---------------------------------------|-----------------------------------------------------|
| `#[derive(LuaAnnotation)]`     | struct (named fields)          | `--- @class crap.X { @field … }`                    |
| `#[derive(LuaAlias)]`          | enum (unit / newtype / mixed)  | `--- @alias crap.X "a" \| "b" \| string \| …`       |
| `#[derive(LuaTypeAlias)]`      | unit struct                    | `--- @alias crap.X <literal target>`                |
| `#[derive(LuaTaggedClass)]`    | tagged or untagged enum        | one `--- @class crap.X<Variant>` per variant + union `--- @alias crap.X` |
| `#[derive(LuaFieldTypeViews)]` | the catch-all `FieldDefinition` struct | `crap.BaseField` + 20 per-type subclasses    |
| `#[lua_fn(path = "...")]`      | top-level `fn`                 | `function crap.X.Y(arg, …) end` + register glue     |
| `lua_table!{ … }`              | function-like                  | the `crap.X = {}` table + the register fn that wires the contained fns |

The `shared.rs` module holds cross-derive helpers: `LuaField` (the
struct/enum field descriptor used by `LuaAnnotation`,
`LuaTaggedClass`, and `LuaFieldTypeViews`), doc-comment extraction,
field-line emission, the type-mapping table, and `rename_all` strategy
application. Modify here when adding a feature that's relevant to more
than one derive.

## Type mapping (auto-resolved Rust → Lua)

| Rust                                            | Lua                                                            |
|-------------------------------------------------|----------------------------------------------------------------|
| `String`, `&str`, `&'static str`                | `string`                                                       |
| `bool`                                          | `boolean`                                                      |
| `u8`/`u16`/…/`usize`, `i8`/…/`isize`            | `integer`                                                      |
| `f32`, `f64`                                    | `number`                                                       |
| `Option<T>`                                     | `T?`                                                           |
| `Vec<T: scalar>`                                | `T[]` (the scalar string is the same as the table above)       |
| `Vec<T: LuaAnnotation>`                         | `T[]` (resolved via `<T as LuaAnnotation>::CLASS_NAME`)        |
| Bare path `T`                                   | `T::CLASS_NAME` (requires `T: LuaAnnotation`)                  |
| `HashMap<K, V>` (in `#[lua_fn]` param types)    | `table<K, V>`                                                  |

Unrecognized types are a compile error — add `#[lua(ty = "…")]` on the
field to override.

## The `#[lua(…)]` attribute reference

Container-level (the `#[lua(…)]` you put on the struct/enum):

- `class = "crap.X"` — for `LuaAnnotation`. The class name.
- `alias = "crap.X"` — for `LuaAlias` / `LuaTypeAlias`. The alias name.
- `target = "<literal>"` — for `LuaTypeAlias`. The right-hand side of the alias.
- `tag = "type"` — for `LuaTaggedClass`. The discriminator field name. Omit for untagged enums.
- `rename_all = "snake_case" | "camelCase" | "lowercase"` — variant or field-name renaming strategy.
- `extends = "<parent>"` — for `LuaAnnotation`. Emits `--- @class crap.X : <parent>`.
- `extra_field = "[K] V"` — for `LuaAnnotation`. Trailing `--- @field [K] V` line (LuaLS index signature).

Field-level (the `#[lua(…)]` you put on a struct field or a tuple-variant payload):

- `ty = "<literal>"` — override the auto-mapped type with a literal string. Disables auto-mapping for this field.
- `ty_expr = "<rust expression>"` — for runtime-resolved type names (rare; used by `crap.FieldType` views).
- `rename = "…"` — emit `@field <rename>` instead of the Rust field name. Wins over container `rename_all`.
- `optional` — force the `?` suffix even when the Rust type isn't `Option<T>` (e.g. a `bool` that the Lua user can omit).
- `skip` — exclude this field from the emit (Rust-internal, not in the Lua surface).
- `applies_to = "text, textarea"` — for `LuaFieldTypeViews` only. Names the field-type variants this field appears under.
- `flatten` — for `LuaFieldTypeViews` only. Flatten the field's inner struct fields into the parent view class.

Variant-level (the `#[lua(…)]` you put on an enum variant):

- `rename = "…"` — override the variant's Lua-side name (the literal string in an alias union, or the per-variant class suffix in `LuaTaggedClass`).
- `view_class = "crap.XField"` — for `LuaAlias` only. Marks this variant as having a per-type view class; together with `LuaFieldTypeViews` on the parent struct, drives `crap.BaseField` + per-type subclass emission.

## Authoring patterns

### Lua-facing struct with a real Rust counterpart

```rust
use serde::{Deserialize, Serialize};
use crate::typegen::lua::LuaAnnotation;

#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.MyConfig")]
pub struct MyConfig {
    /// One-line field doc — emitted as the prose of `--- @field name …`.
    pub name: String,
    /// Optional knob; `Option<bool>` → `boolean?`.
    pub enabled: Option<bool>,
    /// Custom Lua-side shape that doesn't auto-map.
    #[lua(ty = "table<string, any>")]
    pub extras: serde_json::Map<String, serde_json::Value>,
}
```

The handler that builds the runtime value uses
`lua.to_value(&my_config)` (requires `LuaSerdeExt`) — the Lua table
shape is derived from the same struct as the Lua doc, so no drift.

### Lua input parsed into a typed Rust struct

```rust
use mlua::{FromLua, Lua, LuaSerdeExt, Value, Result as LuaResult};
use serde::Deserialize;
use crate::typegen::lua::LuaAnnotation;

#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.MyOptions")]
pub struct MyOptions {
    pub locale: Option<String>,
    #[serde(rename = "overrideAccess")]
    #[lua(rename = "overrideAccess", optional)]
    pub override_access: bool,
}

impl FromLua for MyOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}
```

Reach for `deny_unknown_fields` so typos on the Lua side become loud
errors instead of silently-ignored fields.

### Tagged enum → per-variant classes (`LuaTaggedClass`)

```rust
#[derive(Serialize, Deserialize, LuaTaggedClass)]
#[serde(tag = "type", rename_all = "snake_case")]
#[lua(class = "crap.AuthMethod")]
pub enum AuthMethod {
    PasswordLogin { verify_email: bool },
    Bearer { surfaces: SurfaceSet },
}
```

Emits:

```text
--- @class crap.AuthMethodPasswordLogin
--- @field type "password_login"
--- @field verify_email boolean

--- @class crap.AuthMethodBearer
--- @field type "bearer"
--- @field surfaces crap.Surface[]

--- @alias crap.AuthMethod
--- | crap.AuthMethodPasswordLogin
--- | crap.AuthMethodBearer
```

The macro reads `tag` and `rename_all` from `#[serde(...)]` automatically;
override on `#[lua(...)]` only if the Lua-side shape needs to differ
from the serde shape (rare). For `#[serde(untagged)]` enums, the
per-variant classes carry no discriminator field — that's the
correct Lua shape. Tuple variants are not supported; refactor to
struct variants (`Variant { payload: T }`).

### Union-typed alias (`LuaAlias` mixed-mode)

```rust
#[derive(LuaAlias)]
#[lua(alias = "crap.FieldWidth", rename_all = "lowercase")]
pub enum FieldWidth {
    Full,
    Half,
    Third,
    Custom(String),
}
```

Emits `--- @alias crap.FieldWidth "full" | "half" | "third" | string`.
The `LuaAlias` derive picks one of three output modes by inspecting
the variants:

- All unit variants → multi-line literal-union (per-variant docs preserved).
- All newtype variants → single-line type-union.
- Mixed (unit + newtype) → single-line union of quoted literals + mapped types.

### Pure type alias (`LuaTypeAlias`)

For things that don't have a Rust enum (function-signature aliases,
arbitrary literal unions):

```rust
#[derive(LuaTypeAlias)]
#[lua(
    alias = "crap.ValidateFunction",
    target = "fun(value: any, context: crap.ValidateContext): string?"
)]
pub struct ValidateFunction;
```

The unit struct is never constructed — it exists only as a derive
target. Keep these next to the Rust field whose value they describe
(not in a generic "doc types" module).

### Lua-callable function (`#[lua_fn]` + `lua_table!`)

```rust
use mlua::{Lua, Result as LuaResult};
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Get a config value by key.
#[lua_fn(path = "crap.config.get")]
fn config_get(
    state: &MyState,
    _lua: &Lua,
    #[lua(doc = "Config key.")] key: String,
) -> LuaResult<Option<String>> {
    Ok(state.config.get(&key).cloned())
}

lua_table! {
    name: crap_config,
    path: "crap.config",
    state: MyState,
    header: "Read-only config-value lookup API.",
    fns: [config_get],
}
```

`#[lua_fn]` synthesizes a `register_*` closure and a `*_SPEC` const;
`lua_table!` emits a `register_crap_X(lua, state)` function plus a
`render_crap_X_lua(out)` function that the typegen pipeline calls to
emit the Lua function stubs.

## How the static file is composed

`src/typegen/lua/static_file.rs::render_static_file` walks an array
of per-section `render_*` functions in order. Each one calls into the
derive-emitted `T::render_lua_annotation(out)` /
`T::render_lua_alias(out)` / `render_crap_X_lua(out)` helpers. The
output is byte-stable across runs given the same Rust source.

CI gates on `cargo xtask gen-lua-types --check` — if the on-disk
`types/crap.lua` diverges from what Rust source would emit, the build
fails. After changing any `#[derive(Lua*)]` / `#[lua_fn]` / `lua_table!`
input, run `cargo xtask gen-lua-types` and commit the regenerated
`types/crap.lua`.

## Adding a new derive

1. Add a module in `macros/src/lua_<name>.rs`.
2. Implement `pub(crate) fn derive(input: TokenStream) -> TokenStream`.
3. Register the proc-macro entry point in `macros/src/lib.rs`.
4. Re-export the macro name from `src/typegen/lua/annotation.rs`'s
   `pub use crap_cms_macros::{…}` line.
5. Add a row to the table at the top of this README + the type-mapping
   table in `lib.rs` if you introduce a new auto-mapped Rust type.

## Gotchas

- **Mlua FromLua and the macro work together.** `#[lua_fn]` accepts
  any param type that impls `mlua::FromLua`. The macro will refuse to
  compile if it can't auto-map the type; explicit `#[lua(ty = "…")]`
  on the param resolves that.
- **`#[derive(LuaAnnotation)]` and `#[derive(LuaFieldTypeViews)]` can
  stack on the same struct.** Each derive container ignores the
  other's per-field attributes (`applies_to`, `flatten`,
  `extra_field`, etc.) via `#[darling(default)] #[allow(dead_code)]`
  in the container struct.
- **`#[lua_fn]` auto-emits `#[allow(clippy::needless_pass_by_value,
  clippy::unnecessary_wraps)]` on the user function** — these
  warnings are forced by the wrapper closure's `FromLuaMulti`
  signature (owned params required) and `LuaResult` return type. Do
  not add app-level `#![allow]` for these lints; the macro footprint
  covers them.
- **Lua doc-comments use 4-space indented examples carefully.** Rust
  doc-comments forwarded through `#[lua_fn]` become rustdoc tests by
  default. Use ```` ```lua ```` fenced blocks (which rustdoc skips)
  for example Lua code, not 4-space indentation.
- **Empty class blocks are clippy ICE territory.** When a section of
  the generated Lua file is just blank lines, never `include_str!` a
  whitespace-only file — clippy 0.1.95 ICEs on this. Inline
  `out.push_str("\n")` instead.
