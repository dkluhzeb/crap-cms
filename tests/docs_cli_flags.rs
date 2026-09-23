//! Executable-documentation guard: every long flag of every CLI subcommand is
//! documented in `docs/src/cli/flags.md`, in that subcommand's section.
//!
//! `docs_cli_smoke` validates the subcommand chains the book names; this one
//! closes the other direction — a flag added to the clap tree without a line
//! in the reference fails here, naming the subcommand and the flag. The tree
//! is the live one (`Cli::command()`), so nothing has to be kept in sync by
//! hand.
//!
//! **Section scoping.** A `###` / `####` heading documents the commands in
//! its backticked spans (a heading spanning ``user lock`` and ``user unlock``
//! documents two; one spanning ``status --check`` documents `status`). A flag of
//! `jobs trigger` must appear in the `jobs trigger` section or in an
//! ancestor's (`jobs`) — a parent section may document flags its
//! subcommands share. Root-level flags (`--config`) are documented once
//! under "Global Flags" and are not scanned.

use std::{collections::BTreeMap, fs, path::PathBuf};

use clap::{Command, CommandFactory};
use crap_cms::commands::Cli;

/// The CLI reference page.
fn flags_md() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/src/cli/flags.md");

    fs::read_to_string(&path).expect("read docs/src/cli/flags.md")
}

/// The commands a heading documents: each backticked span, up to its first
/// flag.
fn heading_commands(line: &str) -> Vec<String> {
    line.split('`')
        .skip(1)
        .step_by(2)
        .map(|span| {
            span.split_whitespace()
                .take_while(|token| !token.starts_with('-'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|command| !command.is_empty())
        .collect()
}

/// Whether `line` is a section heading (`##` to `####`).
fn is_heading(line: &str) -> bool {
    ["## ", "### ", "#### "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// The page's section text per documented command. A section runs from its
/// heading to the next heading; a `#` line inside a code fence is a shell
/// comment, not a heading.
fn sections(md: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Vec<String> = Vec::new();
    let mut in_fence = false;

    for line in md.lines() {
        if line.starts_with("```") {
            in_fence = !in_fence;
        }

        if !in_fence && is_heading(line) {
            current = heading_commands(line);

            for command in &current {
                out.entry(command.clone()).or_default();
            }

            continue;
        }

        for command in &current {
            let text = out.get_mut(command).expect("section registered");
            text.push_str(line);
            text.push('\n');
        }
    }

    out
}

/// Every `(subcommand path, long flag)` of `cmd` and its subcommands.
fn collect_flags(cmd: &Command, parent: &str, out: &mut Vec<(String, String)>) {
    let path = if parent.is_empty() {
        cmd.get_name().to_string()
    } else {
        format!("{parent} {}", cmd.get_name())
    };

    for arg in cmd.get_arguments() {
        let Some(long) = arg.get_long() else {
            continue;
        };

        if arg.is_hide_set() || long == "help" || long == "version" {
            continue;
        }

        out.push((path.clone(), long.to_string()));
    }

    for sub in cmd.get_subcommands() {
        collect_flags(sub, &path, out);
    }
}

/// Every subcommand flag of the live CLI tree.
fn cli_flags() -> Vec<(String, String)> {
    let mut out = Vec::new();

    for sub in Cli::command().get_subcommands() {
        collect_flags(sub, "", &mut out);
    }

    out
}

/// The documentation a subcommand's flags may be found in: its own section
/// plus every ancestor's.
fn scoped_text(sections: &BTreeMap<String, String>, path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split(' ').collect();

    let texts: Vec<&String> = (1..=parts.len())
        .filter_map(|n| sections.get(&parts[..n].join(" ")))
        .collect();

    if texts.is_empty() {
        return None;
    }

    Some(texts.into_iter().map(String::as_str).collect())
}

/// Whether `text` names `--long` as a whole flag (not a prefix of a longer one).
fn mentions_flag(text: &str, long: &str) -> bool {
    let needle = format!("--{long}");

    text.match_indices(&needle).any(|(at, _)| {
        text[at + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-'))
    })
}

#[test]
fn every_subcommand_flag_is_documented_in_its_section() {
    let sections = sections(&flags_md());
    let mut missing = Vec::new();

    for (path, long) in cli_flags() {
        let Some(text) = scoped_text(&sections, &path) else {
            missing.push(format!("`{path}` has no section (flag --{long})"));
            continue;
        };

        if !mentions_flag(&text, &long) {
            missing.push(format!("`{path} --{long}`"));
        }
    }

    assert!(
        missing.is_empty(),
        "docs/src/cli/flags.md does not document:\n  {}",
        missing.join("\n  ")
    );
}

/// The walk must reach nested subcommands and their flags — an empty walk
/// would pass the guard above vacuously.
#[test]
fn the_flag_walk_reaches_nested_subcommands() {
    let flags = cli_flags();

    for (path, long) in [
        ("jobs trigger", "priority"),
        ("update", "force"),
        ("update install", "reinstall"),
        ("user create", "password-stdin"),
    ] {
        assert!(
            flags.iter().any(|(p, l)| p == path && l == long),
            "walk missed `{path} --{long}`"
        );
    }
}

#[test]
fn a_flag_is_matched_whole_not_as_a_prefix() {
    assert!(mentions_flag("| `--id` | — |", "id"));
    assert!(mentions_flag("pass --force.", "force"));
    assert!(!mentions_flag("| `--id-token` |", "id"));
    assert!(!mentions_flag("no flag here", "id"));
}

#[test]
fn headings_document_each_backticked_command() {
    assert_eq!(
        heading_commands("#### `user lock` / `user unlock`"),
        vec!["user lock", "user unlock"]
    );
    assert_eq!(heading_commands("#### `status --check`"), vec!["status"]);
    assert!(heading_commands("## Global Flags").is_empty());
}
