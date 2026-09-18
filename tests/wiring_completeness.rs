//! Wiring-completeness guards.
//!
//! A component that is *written but never wired in* fails silently: a
//! typegen render function that never lands in `BLOCK_RENDERS` simply
//! leaves its section out of `types/crap.lua` (the `crap.jobs` run API
//! shipped this way — fully implemented, invisible to editors), and a
//! web component that is defined but never placed in any template or
//! `h()` call renders nothing anywhere (the inline-create panel shipped
//! this way — completely non-functional). Both scans are textual with
//! the same limits `surface_parity.rs` documents: a high-signal
//! tripwire, not an AST proof.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::production_code;

fn files_with_ext(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_with_ext(&path, ext, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
}

fn concat_sources(dir: &Path, ext: &str) -> String {
    let mut files = Vec::new();
    files_with_ext(dir, ext, &mut files);
    files
        .iter()
        .filter_map(|f| fs::read_to_string(f).ok())
        .collect()
}

/// Every `.rs` file under `dir`, reduced to live production code — test
/// modules and comments removed — so a renderer that only a unit test calls,
/// or that a doc comment merely names, still reads as unwired.
fn concat_production_rust(dir: &Path) -> String {
    let mut files = Vec::new();
    files_with_ext(dir, "rs", &mut files);

    files
        .iter()
        .filter_map(|f| fs::read_to_string(f).ok())
        .map(|src| production_code(&src))
        .collect()
}

/// True when the signature opening at `lines[i]` is followed by a `{` body
/// rather than terminated by `;`. A bodiless signature is a trait method
/// declaration whose implementations the derive macros emit — not a
/// definition that can be written-but-unwired.
fn has_body(lines: &[&str], i: usize) -> bool {
    for line in lines.iter().skip(i).take(8) {
        let trimmed = line.trim_end();

        if trimmed.ends_with('{') {
            return true;
        }
        if trimmed.ends_with(';') {
            return false;
        }
    }

    true
}

/// The `render_*` name defined at `lines[i]`, if that line opens one.
fn render_fn_name(lines: &[&str], i: usize) -> Option<String> {
    let trimmed = lines[i].trim_start();

    let rest = trimmed
        .strip_prefix("pub fn render_")
        .or_else(|| trimmed.strip_prefix("pub(crate) fn render_"))
        .or_else(|| trimmed.strip_prefix("pub(super) fn render_"))
        .or_else(|| trimmed.strip_prefix("fn render_"))?;

    let end = rest.find(['(', '<'])?;
    let name = format!("render_{}", &rest[..end]);

    has_body(lines, i).then_some(name)
}

/// Names of `render_*` definitions in `source` that `corpus` mentions only
/// once — at the definition itself.
fn orphan_render_fns(source: &str, corpus: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();

    let mut orphans: Vec<String> = (0..lines.len())
        .filter_map(|i| render_fn_name(&lines, i))
        .filter(|name| corpus.matches(name.as_str()).count() < 2)
        .collect();

    orphans.sort();
    orphans.dedup();
    orphans
}

/// Every `fn render_*` in the Lua typegen module must be *referenced*
/// somewhere beyond its definition — from `BLOCK_RENDERS`, from another
/// render function that composes it, or from the derive macros that emit
/// its call site. A render function whose name appears exactly once is
/// written but unreachable, and its output silently never ships.
///
/// Test modules and comments are excluded from the reference count: a
/// renderer exercised only by its own unit test, or merely named in a doc
/// comment, is still unwired in production.
#[test]
fn every_lua_typegen_render_fn_is_wired() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let source = concat_production_rust(&root.join("src/typegen/lua"));
    assert!(!source.trim().is_empty(), "src/typegen/lua must exist");

    let macros = concat_production_rust(&root.join("macros/src"));
    assert!(!macros.trim().is_empty(), "macros/src must exist");

    let orphans = orphan_render_fns(&source, &format!("{source}{macros}"));

    assert!(
        orphans.is_empty(),
        "typegen render function(s) defined but never wired into \
         BLOCK_RENDERS, composed by another renderer, or called from a \
         derive macro — their output silently never reaches \
         types/crap.lua:\n  {}",
        orphans.join("\n  ")
    );
}

/// Positive control: the real scan flags a renderer whose only mention is
/// its own definition, and clears one that is called.
#[test]
fn render_scan_fires_on_synthetic_orphan() {
    let orphan = "fn render_only_defined_here(out: &mut String) {\n}\n";
    assert_eq!(
        orphan_render_fns(orphan, orphan),
        vec!["render_only_defined_here".to_string()],
        "a lone definition must count as unreferenced"
    );

    let wired = format!(
        "{orphan}fn compose(out: &mut String) {{\n    render_only_defined_here(out);\n}}\n"
    );
    assert!(
        orphan_render_fns(orphan, &wired).is_empty(),
        "a composed renderer must not be flagged"
    );
}

/// Positive control for the stripping: a mention inside a test module or a
/// doc comment must not count as wiring. Both gate spellings in this tree
/// are covered — the per-module `#[cfg(all(test, …))]` and the whole-file
/// `#![cfg(test)]` that `src/typegen/lua/fn_macro_tests.rs` carries, whose
/// calls would otherwise keep a dead renderer looking wired.
#[test]
fn render_scan_ignores_test_modules_and_comments() {
    let gated_module = concat!(
        "fn render_x(out: &mut String) {\n}\n",
        "/// render_x is described here\n",
        "#[cfg(all(test, feature = \"sqlite\"))]\n",
        "mod tests {\n    fn t() {\n        render_x(out);\n    }\n}\n",
    );

    let production = production_code(gated_module);
    assert!(!production.contains("mod tests"), "test module must be cut");
    assert_eq!(
        orphan_render_fns(&production, &production),
        vec!["render_x".to_string()]
    );

    let whole_file_test = concat!(
        "#![cfg(test)]\n",
        "use std::fmt;\n",
        "fn t() {\n    render_x(out);\n}\n",
    );
    let corpus = format!("{production}{}", production_code(whole_file_test));

    assert_eq!(
        orphan_render_fns(&production, &corpus),
        vec!["render_x".to_string()],
        "a call from a whole-file test module must not count as wiring"
    );
}

/// Every custom element defined under `static/components/` must be
/// *placed* somewhere: a `<crap-…` tag in a template, or the tag name
/// string in another JS file (an `h('crap-…')` construction, an
/// `import`-and-place site). A tag whose only mention is its own
/// defining file is registered but never instantiated.
#[test]
fn every_defined_web_component_is_placed_somewhere() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut js_files = Vec::new();
    files_with_ext(&root.join("static/components"), "js", &mut js_files);
    assert!(!js_files.is_empty(), "static/components must exist");

    let templates = concat_sources(&root.join("templates"), "hbs");
    assert!(!templates.is_empty(), "templates must exist");

    // (tag, defining file) pairs.
    let mut defined = Vec::new();
    for file in &js_files {
        let contents = fs::read_to_string(file).unwrap_or_default();
        for line in contents.lines() {
            let Some(idx) = line.find("customElements.define(") else {
                continue;
            };
            let rest = &line[idx + "customElements.define(".len()..];
            let tag: String = rest
                .trim_start()
                .trim_start_matches(['"', '\''])
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect();
            if tag.starts_with("crap-") {
                defined.push((tag, file.clone()));
            }
        }
    }
    assert!(
        defined.len() >= 30,
        "expected the full component inventory, found {} defines — \
         the extraction pattern may have rotted (itself a D4)",
        defined.len()
    );

    let mut orphans = Vec::new();
    for (tag, def_file) in &defined {
        let in_templates = templates.contains(&format!("<{tag}"));

        let in_other_js = js_files.iter().any(|f| {
            f != def_file
                && fs::read_to_string(f)
                    .unwrap_or_default()
                    .contains(tag.as_str())
        });

        if !in_templates && !in_other_js {
            orphans.push(tag.clone());
        }
    }

    assert!(
        orphans.is_empty(),
        "web component(s) defined but never placed in any template or \
         other JS file — registered, never instantiated:\n  {}",
        orphans.join("\n  ")
    );
}

fn ci_yaml() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("ci.yml must exist")
}

/// `(indent, text after `run:`)` when `line` is a `run:` mapping key.
fn run_directive(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();

    let rest = trimmed
        .strip_prefix("- run:")
        .or_else(|| trimmed.strip_prefix("run:"))?;

    Some((line.len() - trimmed.len(), rest.trim()))
}

/// A block scalar continues while the line is blank or indented past the
/// `run:` key that opened it.
fn block_continues(line: &str, indent: usize) -> bool {
    line.trim().is_empty() || line.len() - line.trim_start().len() > indent
}

/// Drop a trailing `#` comment. No command in this workflow quotes a `#`.
fn strip_yaml_comment(line: &str) -> String {
    line.split('#').next().unwrap_or("").trim().to_string()
}

/// Every command the workflow actually *executes*, comments stripped.
/// Block scalars (`run: |`) contribute each of their lines.
///
/// Only `run:` lines count. A step `name:` and the explanatory comments
/// above it repeat the command text verbatim, so a whole-file `contains`
/// stays green after the `run:` line itself is deleted.
fn ci_run_commands(yaml: &str) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut commands = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let Some((indent, rest)) = run_directive(lines[i]) else {
            i += 1;
            continue;
        };
        i += 1;

        if !rest.is_empty() && !rest.starts_with('|') && !rest.starts_with('>') {
            commands.push(strip_yaml_comment(rest));
            continue;
        }

        while i < lines.len() && block_continues(lines[i], indent) {
            commands.push(strip_yaml_comment(lines[i]));
            i += 1;
        }
    }

    commands.retain(|c| !c.is_empty());
    commands
}

/// CI-gate pin: every enforcement gate the project relies on must appear in
/// a command the workflow actually runs. The decay mode is real — 139
/// browser e2e tests once sat behind a feature flag CI never enabled and
/// failed silently for a full release cycle. This does not prove the jobs
/// execute (an `if: false` would slip past a textual pin); it catches the
/// common regression of a gate being dropped or renamed during a workflow
/// refactor.
#[test]
fn ci_workflow_still_runs_every_gate() {
    const REQUIRED_GATES: &[&str] = &[
        "cargo fmt --all -- --check",
        "biome ci",
        "-D warnings",
        "fmt --check", // crap-cms template formatter
        "cargo xtask gen-lua-types --check",
        "cargo xtask gen-template-doc --check",
        "cargo xtask gen-proto --check",
        "cargo xtask gen-wire-doc --check",
        "cargo xtask gen-doc-tables --check",
        "cargo audit",
        "cargo test --workspace --exclude crap-cms-e2e",
        "cargo test -p crap-cms-e2e",
    ];

    let commands = ci_run_commands(&ci_yaml());
    assert!(
        commands.len() >= 20,
        "expected the full run-command inventory, found {} — the `run:` \
         extraction may have rotted",
        commands.len()
    );

    let missing: Vec<&&str> = REQUIRED_GATES
        .iter()
        .filter(|g| !commands.iter().any(|c| c.contains(**g)))
        .collect();

    assert!(
        missing.is_empty(),
        "CI gate(s) missing from the `run:` commands in \
         .github/workflows/ci.yml — a guard the project relies on is no \
         longer enforced:\n  {missing:?}"
    );
}

/// Positive control: a gate named only in a step `name:` or a `#` comment
/// must not satisfy the pin.
#[test]
fn ci_gate_scan_ignores_step_names_and_comments() {
    let synthetic = "\
jobs:
  check:
    steps:
      - name: Lua type definitions in sync (cargo xtask gen-lua-types --check)
        # run: cargo xtask gen-lua-types --check
        run: echo replaced
";

    assert_eq!(
        ci_run_commands(synthetic),
        vec!["echo replaced".to_string()]
    );
}

/// Positive control: a block scalar contributes each of its lines, and the
/// scan stops at the next less-indented key.
#[test]
fn ci_run_scan_reads_block_scalars() {
    let synthetic = "\
      - name: Two steps
        run: |
          cargo install cargo-audit
          cargo audit

      - uses: actions/checkout@v4
";

    assert_eq!(
        ci_run_commands(synthetic),
        vec![
            "cargo install cargo-audit".to_string(),
            "cargo audit".to_string()
        ]
    );
}

/// One row of the feature-matrix job.
struct MatrixRow {
    label: String,
    flags: String,
    runs_tests: bool,
}

fn apply_matrix_field(row: &mut MatrixRow, entry: &str) {
    let Some((key, value)) = entry.split_once(':') else {
        return;
    };
    let value = value.trim().trim_matches('"');

    match key.trim() {
        "label" => row.label = value.to_string(),
        "flags" => row.flags = value.to_string(),
        "test" => row.runs_tests = value == "true",
        _ => {}
    }
}

/// Parse the feature-matrix `include:` rows structurally, so a row's `test:`
/// flag is read as the boolean it is rather than inferred from a substring.
fn feature_matrix_rows(yaml: &str) -> Vec<MatrixRow> {
    let mut rows: Vec<MatrixRow> = Vec::new();
    let mut include_indent = None;

    for line in yaml.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if trimmed == "include:" {
            include_indent = Some(indent);
            continue;
        }

        let Some(base) = include_indent else {
            continue;
        };
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if indent <= base {
            break;
        }

        if trimmed.starts_with("- ") {
            rows.push(MatrixRow {
                label: String::new(),
                flags: String::new(),
                runs_tests: false,
            });
        }

        if let Some(row) = rows.last_mut() {
            apply_matrix_field(row, trimmed.trim_start_matches("- "));
        }
    }

    rows
}

fn parse_feature_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|f| f.trim().trim_matches('"').to_string())
        .filter(|f| !f.is_empty())
        .collect()
}

/// `(every feature the crate declares, the `default` list)`.
fn cargo_features() -> (Vec<String>, Vec<String>) {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("Cargo.toml must exist");

    let section = manifest
        .split("[features]")
        .nth(1)
        .and_then(|s| s.split("\n[").next())
        .expect("Cargo.toml must declare [features]");

    let mut all = Vec::new();
    let mut default = Vec::new();

    for line in section.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();

        if name == "default" {
            default = parse_feature_list(value);
        } else if !name.is_empty() && !name.starts_with('#') {
            all.push(name.to_string());
        }
    }

    (all, default)
}

/// The features a cargo flag string turns on, resolving `--all-features` and
/// `--no-default-features` the way cargo does.
fn features_enabled(flags: &str, all: &[String], default: &[String]) -> BTreeSet<String> {
    if flags.contains("--all-features") {
        return all.iter().cloned().collect();
    }

    let mut enabled: BTreeSet<String> = if flags.contains("--no-default-features") {
        BTreeSet::new()
    } else {
        default.iter().cloned().collect()
    };

    for chunk in flags.split("--features").skip(1) {
        let list = chunk.split_whitespace().next().unwrap_or("");
        enabled.extend(parse_feature_list(list));
    }

    enabled
}

/// Flag strings of every CI invocation that actually runs the main crate's
/// tests: literal `cargo test` commands, plus the matrix rows whose `test:`
/// is true. Matrix-interpolated commands are skipped — the row they expand
/// from is counted instead — as is the e2e crate, which carries its own
/// features.
fn test_running_flag_sets(yaml: &str) -> Vec<String> {
    let mut sets: Vec<String> = ci_run_commands(yaml)
        .into_iter()
        .filter(|c| {
            c.contains("cargo test") && !c.contains("${{") && !c.contains("-p crap-cms-e2e")
        })
        .collect();

    sets.extend(
        feature_matrix_rows(yaml)
            .into_iter()
            .filter(|r| r.runs_tests)
            .map(|r| r.flags),
    );

    sets
}

/// Every feature the crate declares must be enabled by a CI invocation that
/// *runs tests*. A feature reached only by a compile-only matrix row has its
/// `#[cfg(feature = "…")]` unit tests built and never executed — the S3
/// response-status regression tests and the Redis window/TTL math sat behind
/// exactly that hole, invisible for as long as the rows stayed compile-only.
#[test]
fn every_cargo_feature_has_a_test_running_ci_row() {
    let ci = ci_yaml();
    let (all, default) = cargo_features();

    assert!(
        all.len() >= 4,
        "expected at least 4 Cargo features, found {all:?} — either the \
         manifest parse rotted or a feature was removed deliberately (then \
         lower this floor)"
    );

    let mut covered = BTreeSet::new();
    for flags in test_running_flag_sets(&ci) {
        covered.extend(features_enabled(&flags, &all, &default));
    }

    let uncovered: Vec<&String> = all.iter().filter(|f| !covered.contains(*f)).collect();

    assert!(
        uncovered.is_empty(),
        "cargo feature(s) that no test-running CI row enables — their \
         feature-gated tests compile but never execute. Add a matrix row \
         with `test: true` that turns them on:\n  {uncovered:?}"
    );
}

/// The reviewed flag combinations must stay in the matrix. Deleting a row is
/// a deliberate decision, not a silent refactor casualty.
#[test]
fn feature_matrix_still_covers_every_reviewed_flag_set() {
    const REQUIRED_FLAG_SETS: &[&str] = &[
        "--features postgres",
        "--no-default-features --features postgres",
        "--all-features",
        "--features s3-storage,redis",
    ];

    let rows = feature_matrix_rows(&ci_yaml());
    assert!(
        rows.len() >= 4,
        "expected at least 4 feature-matrix rows, found {} — either the \
         `include:` parse rotted or a row was removed deliberately (then \
         lower this floor)",
        rows.len()
    );
    assert!(
        rows.iter().all(|r| !r.label.is_empty()),
        "every matrix row must carry a label"
    );

    let missing: Vec<&&str> = REQUIRED_FLAG_SETS
        .iter()
        .filter(|set| !rows.iter().any(|r| r.flags == **set))
        .collect();

    assert!(
        missing.is_empty(),
        "feature-matrix row(s) gone from .github/workflows/ci.yml:\n  {missing:?}"
    );
}

/// Positive control: a feature that only ever appears in a compile-only row
/// must read as uncovered.
#[test]
fn feature_coverage_scan_fires_on_a_compile_only_feature() {
    let synthetic = "\
        include:
          - label: all
            flags: --all-features
            test: false
          - label: default
            flags: \"\"
            test: true
";

    let all = vec!["sqlite".to_string(), "redis".to_string()];
    let default = vec!["sqlite".to_string()];

    let rows = feature_matrix_rows(synthetic);
    assert_eq!(rows.len(), 2);

    let mut covered = BTreeSet::new();
    for row in rows.iter().filter(|r| r.runs_tests) {
        covered.extend(features_enabled(&row.flags, &all, &default));
    }

    assert!(
        !covered.contains("redis"),
        "a feature enabled only by a compile-only row must count as uncovered"
    );
    assert!(covered.contains("sqlite"), "the default feature is covered");
}

/// Init-phase completeness pin: every Lua API
/// that registers into a process-wide registry (`crap.*.define`,
/// `crap.*.register*`) must carry an init-phase guard — a runtime call
/// would land in one pooled VM and be intermittent across requests, or
/// bypass migration/scheduler enrollment. Building this pin found
/// `crap.hooks.register`/`remove` unguarded. A file counts as guarded
/// when it references `require_init_phase` (the helper) or `InitPhase`
/// (the direct app-data check `pages.rs` uses).
#[test]
fn every_registering_lua_api_is_init_phase_guarded() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/hooks/lua_api");
    let mut files = Vec::new();
    files_with_ext(&root, "rs", &mut files);
    assert!(!files.is_empty(), "src/hooks/lua_api must exist");

    let mut violations = Vec::new();
    let mut registering_files = 0;

    for file in &files {
        let contents = fs::read_to_string(file).unwrap_or_default();

        let registers = contents.lines().any(|l| {
            l.contains("path = \"crap.") && (l.contains(".define\"") || l.contains(".register"))
        });
        if !registers {
            continue;
        }
        registering_files += 1;

        let guarded = contents.contains("require_init_phase") || contents.contains("InitPhase");
        if !guarded {
            violations.push(
                file.strip_prefix(root.parent().unwrap().parent().unwrap())
                    .unwrap_or(file)
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }

    assert!(
        registering_files >= 10,
        "expected the full register/define API inventory, found \
         {registering_files} files — the detection pattern may have rotted \
         (itself a D4)"
    );
    assert!(
        violations.is_empty(),
        "Lua registration API(s) without an init-phase guard — a runtime \
         call lands in one pooled VM and misbehaves intermittently:\n  {}",
        violations.join("\n  ")
    );
}
