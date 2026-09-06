//! Executable-documentation guards.
//!
//! Docs that assert mechanisms nothing executes rot silently — this
//! cycle alone found a scenario instructing users to edit a Rust source
//! file, a recipe that failed outside a git repo, and grep-the-sources
//! instructions aimed at binary users. Two guards:
//!
//! 1. Every `crap-cms …` invocation in the book must name a **real
//!    subcommand path**, validated against the live CLI tree parsed
//!    from the binary's own `--help` output. A doc telling users to run
//!    a command that was renamed or removed fails here, naming the file.
//! 2. The load-bearing documented flows are **actually executed**
//!    against a scaffolded config dir: the scenario-08 upgrade loop
//!    (extract → status → edit → diff), scenario-02's extract targets,
//!    and drift-tooling's clean-layout answer.
//!
//! Textual-scan limits apply: flags and free-form arguments are not
//! validated (placeholders like `<new-version>` make that unsound) —
//! only the subcommand chain is.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn crap_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_crap-cms"))
}

fn run(args: &[&str]) -> std::process::Output {
    std::process::Command::new(crap_bin())
        .args(args)
        .output()
        .expect("failed to run binary")
}

fn run_ok(args: &[&str]) -> String {
    let output = run(args);
    assert!(
        output.status.success(),
        "command {:?} failed.\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Parse the `Commands:` section of a clap `--help` output into the
/// list of subcommand names.
fn parse_help_commands(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_commands = false;

    for line in help.lines() {
        if line.starts_with("Commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            // Section ends at the next unindented header (`Options:` etc.).
            if !line.starts_with(' ') && !line.is_empty() {
                break;
            }
            // "  serve    Start the server" → first token of an
            // exactly-two-space-indented line.
            if let Some(rest) = line.strip_prefix("  ")
                && !rest.starts_with(' ')
                && let Some(name) = rest.split_whitespace().next()
                && name.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// The live CLI tree: top-level command → set of its subcommands
/// (empty set = leaf command).
fn live_cli_tree() -> BTreeMap<String, BTreeSet<String>> {
    let root_help = run_ok(&["--help"]);
    let mut tree = BTreeMap::new();

    for cmd in parse_help_commands(&root_help) {
        if cmd == "help" {
            continue;
        }
        let sub_help_out = run(&[&cmd, "--help"]);
        let sub_help = String::from_utf8_lossy(&sub_help_out.stdout).to_string();
        let subs: BTreeSet<String> = parse_help_commands(&sub_help)
            .into_iter()
            .filter(|s| s != "help")
            .collect();
        tree.insert(cmd, subs);
    }

    tree
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

/// Extract the subcommand chain (max depth 2) from a documented
/// invocation line, skipping flags and their values.
fn doc_command_chain(line: &str) -> Vec<String> {
    let after = match line.find("crap-cms ") {
        Some(i) => &line[i + "crap-cms ".len()..],
        None => return Vec::new(),
    };

    let mut chain = Vec::new();
    let mut tokens = after.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok == "-C" {
            // The one flag docs place *before* the subcommand; consumes
            // its directory value.
            let _ = tokens.next();
            continue;
        }
        if tok.starts_with('-') {
            // Any other flag ends the subcommand chain — everything
            // after it is options/arguments.
            break;
        }
        let is_name = !tok.is_empty()
            && tok.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && tok.chars().all(|c| c.is_ascii_lowercase() || c == '-');
        if !is_name {
            break; // placeholder, path, number, or free-form argument
        }
        chain.push(tok.to_string());
        if chain.len() == 2 {
            break;
        }
    }
    chain
}

/// Guard 1: every `crap-cms` invocation in the book names a real
/// subcommand path.
#[test]
fn every_documented_cli_invocation_names_a_real_subcommand() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tree = live_cli_tree();
    assert!(
        tree.len() >= 15,
        "CLI tree parse looks broken ({} top-level commands) — the help \
         format may have changed (itself a D4)",
        tree.len()
    );

    let mut docs = Vec::new();
    markdown_files(&root.join("docs/src"), &mut docs);
    markdown_files(&root.join("docs/dev"), &mut docs);
    assert!(!docs.is_empty(), "docs must exist");

    let mut violations = Vec::new();
    for doc in &docs {
        let contents = fs::read_to_string(doc).unwrap_or_default();
        let rel = doc
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();

        let mut in_fence = false;
        for (lineno, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            // Only invocation lines *inside fenced code blocks* — a
            // prose sentence wrapping onto a "crap-cms sources" line
            // start is not an invocation.
            if !in_fence {
                continue;
            }
            let is_invocation =
                trimmed.starts_with("$ crap-cms ") || trimmed.starts_with("crap-cms ");
            if !is_invocation {
                continue;
            }

            let chain = doc_command_chain(trimmed);
            let Some(top) = chain.first() else { continue };

            let Some(subs) = tree.get(top) else {
                violations.push(format!(
                    "{rel}:{}: unknown command `crap-cms {top}`",
                    lineno + 1
                ));
                continue;
            };
            if let Some(sub) = chain.get(1)
                && !subs.is_empty()
                && !subs.contains(sub)
            {
                violations.push(format!(
                    "{rel}:{}: `crap-cms {top}` has no subcommand `{sub}`",
                    lineno + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "documented CLI invocation(s) name commands the binary does not \
         have — fix the doc or the CLI:\n  {}",
        violations.join("\n  ")
    );
}

/// Positive control: the chain extractor and tree
/// lookup must flag a synthetic bogus command.
#[test]
fn doc_command_scan_fires_on_synthetic_violation() {
    let tree = live_cli_tree();
    let chain = doc_command_chain("$ crap-cms frobnicate now");
    assert_eq!(chain.first().map(String::as_str), Some("frobnicate"));
    assert!(
        !tree.contains_key("frobnicate"),
        "a bogus command must not resolve against the live tree"
    );
}

/// Guard 2: the documented drift-tooling flow (scenario 08 + scenario
/// 02 + drift-tooling.md) executes as written against a scaffolded
/// config dir.
#[test]
fn documented_template_workflow_executes_as_written() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_str().unwrap();

    run_ok(&["init", "--no-input", dir]);

    // drift-tooling.md: a fresh config dir is on the current layout.
    let layout = run_ok(&["-C", dir, "templates", "layout"]);
    assert!(
        layout.contains("already on the current layout"),
        "documented clean-layout answer missing: {layout}"
    );

    // Scenario 08 step 4 / drift-tooling: extract the documented file.
    run_ok(&["-C", dir, "templates", "extract", "layout/base.hbs"]);

    // Scenario 08: an untouched extract reports pristine.
    let status = run_ok(&["-C", dir, "templates", "status"]);
    assert!(
        status.contains("pristine"),
        "documented `pristine` state missing from status: {status}"
    );

    // Scenario 08 step 3: diff runs against the documented path form.
    run_ok(&["-C", dir, "templates", "diff", "templates/layout/base.hbs"]);

    // Scenario 02 step 1: the documented extract targets must exist.
    run_ok(&[
        "-C",
        dir,
        "templates",
        "extract",
        "collections/items_table.hbs",
        "collections/items_row.hbs",
    ]);

    // Edited override still tracks (scenario 08 pattern A precondition).
    let base = tmp.path().join("templates/layout/base.hbs");
    let content = fs::read_to_string(&base).unwrap();
    fs::write(&base, content + "\n{{!-- local customization --}}\n").unwrap();
    let status = run_ok(&["-C", dir, "templates", "status"]);
    assert!(
        status.contains("current"),
        "an edited same-version override must report `current`: {status}"
    );
}

/// Multi-node inventory pin: the deployment doc
/// must keep covering every subsystem that holds node-local state or a
/// cluster-wide safety mechanism. The class instance was "per-node rate
/// limits silently multiply an attacker's budget by node count" — the
/// mechanisms exist; the risk is the doc silently dropping one during a
/// rewrite while operators rely on it as the multi-node checklist.
#[test]
fn multi_server_doc_covers_every_node_local_subsystem() {
    const REQUIRED_TOPICS: &[&str] = &[
        "rate_limit_backend",     // per-node limiter budget multiplication
        "transport = \"redis\"",  // live-event fan-out
        "backend = \"redis\"",    // populate-cache invalidation
        "_crap_cron_fired",       // cron dedup
        "FOR UPDATE SKIP LOCKED", // job claiming
        "NFS",                    // local storage single-writer assumption
        "Mcp-Session-Id",         // per-node MCP audit-label map
        "sticky",                 // stream stickiness guidance
    ];

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = fs::read_to_string(root.join("docs/src/deployment/multi-server.md"))
        .expect("multi-server.md must exist");

    let missing: Vec<&&str> = REQUIRED_TOPICS
        .iter()
        .filter(|t| !doc.contains(**t))
        .collect();
    assert!(
        missing.is_empty(),
        "multi-server.md no longer covers node-local subsystem(s): {missing:?}"
    );
}
