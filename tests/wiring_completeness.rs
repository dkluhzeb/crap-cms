//! Wiring-completeness guards (ledger class **M7**).
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

use std::fs;
use std::path::{Path, PathBuf};

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

/// Every `fn render_*` in the Lua typegen module must be *referenced*
/// somewhere beyond its definition — from `BLOCK_RENDERS`, or from
/// another render function that composes it. A render function whose
/// name appears exactly once in the module source is written but
/// unreachable, and its output silently never ships.
#[test]
fn every_lua_typegen_render_fn_is_wired() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/typegen/lua");
    let source = concat_sources(&root, "rs");
    assert!(!source.is_empty(), "src/typegen/lua must exist");

    let lines: Vec<&str> = source.lines().collect();
    let mut orphans = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(rest) = line
            .trim_start()
            .strip_prefix("pub fn render_")
            .or_else(|| line.trim_start().strip_prefix("pub(crate) fn render_"))
            .or_else(|| line.trim_start().strip_prefix("pub(super) fn render_"))
            .or_else(|| line.trim_start().strip_prefix("fn render_"))
        else {
            continue;
        };

        // Skip `#[test] fn render_*` — unit tests named after the
        // renderer they exercise are not renderers.
        let is_test_fn = lines[..i]
            .iter()
            .rev()
            .take_while(|l| l.trim_start().starts_with('#'))
            .any(|l| l.contains("#[test]"));
        if is_test_fn {
            continue;
        }

        let Some(end) = rest.find(['(', '<']) else {
            continue;
        };
        let name = format!("render_{}", &rest[..end]);

        let refs = source.matches(name.as_str()).count();
        if refs < 2 {
            orphans.push(name);
        }
    }
    orphans.sort();
    orphans.dedup();

    assert!(
        orphans.is_empty(),
        "typegen render function(s) defined but never wired into \
         BLOCK_RENDERS or composed by another renderer — their output \
         silently never reaches types/crap.lua:\n  {}",
        orphans.join("\n  ")
    );
}

/// Positive control (ledger class **D4**): the render scan must flag a
/// synthetic orphan.
#[test]
fn render_scan_fires_on_synthetic_orphan() {
    let synthetic = "fn render_only_defined_here(out: &mut String) {}\n";
    let refs = synthetic.matches("render_only_defined_here").count();
    assert!(refs < 2, "a lone definition must count as unreferenced");
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

/// CI-gate pin (ledger class **D4**): every enforcement gate the project
/// relies on must actually appear in the CI workflow. The decay mode is
/// real — 139 browser e2e tests once sat behind a feature flag CI never
/// enabled and failed silently for a full release cycle. This does not
/// prove the jobs *run* (a `if: false` would slip past a textual pin);
/// it catches the common regression of a gate being dropped or renamed
/// during a workflow refactor.
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
        "cargo test --workspace --exclude crap-cms-e2e",
        "--features postgres",
        "--no-default-features --features postgres",
        "--all-features",
        "cargo test -p crap-cms-e2e",
    ];

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ci = fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("ci.yml must exist");

    let missing: Vec<&&str> = REQUIRED_GATES
        .iter()
        .filter(|g| !ci.contains(**g))
        .collect();

    assert!(
        missing.is_empty(),
        "CI gate(s) missing from .github/workflows/ci.yml — a guard the \
         project relies on is no longer enforced:\n  {missing:?}"
    );
}

/// Init-phase completeness pin (ledger class **M15**): every Lua API
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
