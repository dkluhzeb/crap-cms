# xtask

Workspace task runner. Invoked via the `cargo xtask` alias — a
build-tool binary that lives in its own crate so its dependencies and
compile time stay out of the main crate's hot path.

## Why a separate crate

The [`cargo-xtask`](https://github.com/matklad/cargo-xtask) pattern.
Two alternatives we rejected:

- **`build.rs` in the main crate** — re-runs on every compile, slows
  incremental builds, can't be invoked on demand.
- **Shell scripts under `scripts/`** — no editor support, no type
  safety, every contributor has to learn a slightly different shell
  dialect.

A regular Rust binary that depends on the main crate gives us full
type safety against the same types being documented / generated, with
no impact on the main crate's build path. `xtask` is excluded from
`default-members` in the root `Cargo.toml`, so plain `cargo build` /
`cargo test` from the workspace root never compiles it.

## Subcommands

| Subcommand | Writes to | CI gate |
|------------|-----------|---------|
| `gen-lua-types [--check]` | `types/crap.lua` | yes |
| `gen-template-doc [--check]` | `docs/src/admin-ui/reference/template-context.md` | yes |

Run without `--check` to write; with `--check` to diff (non-zero exit
+ unified diff on stderr if out of sync). Each gate is wired into
`.github/workflows/ci.yml`.

### `gen-lua-types`

Renders the static `types/crap.lua` LuaLS type-definition file from
Rust source — every `--- @class` / `--- @alias` /
`function crap.X.Y(...)` block flows through a derive
(`LuaAnnotation`, `LuaAlias`, `LuaTypeAlias`, `LuaFieldTypeViews`) or
a `lua_table!`-generated `render_X_lua` function. The macros live in
[`crap-cms-macros`](../macros/README.md); the assembly pipeline lives
in `src/typegen/lua/static_file.rs::render_static_file`.

```bash
cargo xtask gen-lua-types          # write types/crap.lua
cargo xtask gen-lua-types --check  # diff vs on-disk; CI gate
```

### `gen-template-doc`

Renders `docs/src/admin-ui/reference/template-context.md` from the
typed admin-page-context structs under `src/admin/context/page/`.
Each per-page struct (login, dashboard, collection edit, …) is
walked via `schemars` and rendered through an inline Handlebars
template, then either written to disk or diffed against the
committed copy.

The render fn lives at `crap_cms::docgen::generate_template_context_md`
(re-export of `src/admin/context/page/schema_doc.rs`). A
`#[cfg(test)]` drift assertion also calls it, so local
`cargo test --lib -- schema_doc` catches staleness without needing
the xtask binary.

```bash
cargo xtask gen-template-doc          # write template-context.md
cargo xtask gen-template-doc --check  # diff vs on-disk; CI gate
```

## Architecture

`main.rs` is intentionally thin: CLI parsing + dispatch only. All
business logic lives in per-subcommand modules:

```
xtask/src/
├── main.rs              # CLI parsing, subcommand dispatch
├── drift.rs             # shared: workspace_root(), check_drift(), print_diff()
├── gen_lua_types.rs     # gen-lua-types business logic + tests
└── gen_template_doc.rs  # gen-template-doc business logic + tests
```

Both subcommands follow the same shape: render to a `String`, then
either write to disk (default mode) or call `check_drift(path, generated,
regen_cmd)` (`--check` mode). The error message includes the
subcommand-specific regen invocation so users see the right
fix-up command on drift.

## Tests

- `drift::tests::*` — the diff helper itself: matches, differs,
  missing file.
- `gen_lua_types::tests::render_static_file_matches_repo_copy` —
  mirrors the main-crate `assembled_output_matches_on_disk` test at
  the xtask layer.
- `gen_template_doc::tests::template_doc_matches_repo_copy` —
  ditto for the template-context Markdown.

The in-crate drift tests (above) give immediate signal on local
`cargo test -p xtask`; CI additionally invokes both `--check`
subcommands directly so failures point at the right regen command.

## Adding a new subcommand

1. **Add a module** `xtask/src/gen_<name>.rs` with a single public
   entry point:
   ```rust
   pub(crate) fn run(check: bool) -> anyhow::Result<()> { ... }
   ```
   Use `crate::drift::{check_drift, workspace_root}` for the path
   resolution + diff handling. Add unit tests in the same file.
2. **Wire it into `main.rs`** — declare the module, add a `Cmd`
   variant with the `--check` flag, dispatch it from `main()`. No
   business logic in `main.rs` — just routing.
3. **Add the CI gate** — append a step to `.github/workflows/ci.yml`
   that runs `cargo xtask gen-<name> --check` after the existing
   gates. Note the file the gate protects in the step's comment so
   reviewers know what to expect on failure.
4. **If the render fn lives in the main crate**, expose it via
   `pub mod docgen` (or `pub mod typegen`) in `src/lib.rs` so xtask
   can call it — the underlying module can stay `pub(crate)` and the
   re-export is the public boundary.

## Gotchas

- **`CARGO_MANIFEST_DIR` resolution.** `xtask` resolves the workspace
  root via `env!("CARGO_MANIFEST_DIR")` + `pop()`. Works because
  `cargo xtask` invokes the binary from the xtask crate directory.
  Don't `cd` before invoking, or paths won't resolve.
- **No `--no-verify`-style shortcuts.** If CI's `--check` fails,
  regenerate with the matching non-`--check` invocation and commit
  the diff. Never edit the generated files by hand — the next
  regeneration will overwrite them.
- **The crate is `publish = false`.** It exists to be built and run
  in this workspace only; it has no semver story.
