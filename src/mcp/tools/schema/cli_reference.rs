//! `cli_reference` MCP tool — return CLI documentation for the AI client.
//!
//! Command names, descriptions, flags and positional arguments are read off
//! the very [`clap::Command`] tree the binary parses with
//! ([`crate::commands::Cli`]), so the reference cannot drift from the CLI.
//! The only hand-written content is the `EXAMPLES` table: invocation
//! examples are prose clap has nowhere to keep.

use anyhow::Result;
use clap::{Arg, Command, CommandFactory};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::fmt::Write as _;

use crate::commands::Cli;

/// The binary name every usage line starts with.
const BINARY: &str = "crap-cms";

/// Invocation examples keyed by command path — `"migrate"` for a top-level
/// command, `"make collection"` for a subcommand. Every key is pinned to a
/// real command by the tests below.
static EXAMPLES: &[(&str, &[&str])] = &[
    ("serve", &["crap-cms serve", "crap-cms serve --detach"]),
    (
        "work",
        &[
            "crap-cms work",
            "crap-cms work --queues email",
            "crap-cms work -d --queues heavy --concurrency 2",
        ],
    ),
    (
        "check",
        &["crap-cms check", "crap-cms -C ./my-project check"],
    ),
    ("status", &["crap-cms status", "crap-cms status --check"]),
    ("init", &["crap-cms init"]),
    (
        "make collection",
        &[
            "crap-cms make collection posts -F 'title:text:required,body:richtext,status:select'",
            "crap-cms make collection users --auth --no-input",
        ],
    ),
    (
        "user create",
        &[
            "crap-cms user create -e admin@example.com",
            "crap-cms user create -e admin@example.com -p secret -f role=admin -f name='Admin'",
        ],
    ),
    (
        "migrate",
        &[
            "crap-cms migrate up",
            "crap-cms migrate create add_categories",
            "crap-cms migrate down -s 2",
            "crap-cms migrate fresh -y",
        ],
    ),
    (
        "backup",
        &["crap-cms backup", "crap-cms backup -o /backups -i"],
    ),
    (
        "restore",
        &[
            "crap-cms restore ./backups/backup-2026-03-07T10-00-00 -y",
            "crap-cms restore /tmp/backup -i -y",
        ],
    ),
    (
        "export",
        &[
            "crap-cms export",
            "crap-cms export -c posts -o posts.json",
            "crap-cms export -c users --include-credentials -o users.json",
        ],
    ),
    (
        "import",
        &[
            "crap-cms import backup.json",
            "crap-cms import posts.json -c posts",
        ],
    ),
    (
        "typegen",
        &[
            "crap-cms typegen lua",
            "crap-cms typegen client -l ts,go",
            "crap-cms typegen proto -m crate::proto",
        ],
    ),
    ("typegen lua", &["crap-cms typegen lua"]),
    (
        "typegen client",
        &[
            "crap-cms typegen client -l ts",
            "crap-cms typegen client -l ts,go,py,rs",
        ],
    ),
    ("typegen proto", &["crap-cms typegen proto -m crate::proto"]),
    (
        "proto",
        &["crap-cms proto", "crap-cms proto -o ./proto/content.proto"],
    ),
    ("mcp", &["crap-cms mcp"]),
    (
        "logs",
        &["crap-cms logs", "crap-cms logs -f", "crap-cms logs clear"],
    ),
    (
        "bench",
        &[
            "crap-cms bench hooks --all",
            "crap-cms bench queries --explain",
            "crap-cms bench queries -c posts --where '{\"status\": \"published\"}' --explain",
            "crap-cms bench create posts -y -n 20",
        ],
    ),
    (
        "update",
        &[
            "crap-cms update",
            "crap-cms update check",
            "crap-cms update install v0.1.0-alpha.7",
            "crap-cms update use v0.1.0-alpha.7",
            "crap-cms update completions bash",
            "crap-cms update completions --uninstall",
        ],
    ),
];

/// Top-level shape returned when `cli_reference` is called without a command.
#[derive(Serialize)]
struct CliOverview {
    binary: &'static str,
    description: String,
    usage: String,
    commands: Vec<CliCommandSummary>,
}

#[derive(Serialize)]
struct CliCommandSummary {
    name: String,
    description: String,
}

/// Shape returned when `cli_reference` is called with a specific command
/// name. All optional fields are skipped when `None` to preserve the wire
/// format exactly (some commands have no subcommands, etc.).
#[derive(Serialize)]
struct CliCommandDetail {
    command: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<Vec<CliFlag>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<Vec<CliArg>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subcommands: Option<Vec<CliSubcommand>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    examples: Option<&'static [&'static str]>,
}

#[derive(Serialize)]
struct CliFlag {
    flag: String,
    description: String,
}

#[derive(Serialize)]
struct CliArg {
    arg: String,
    description: String,
}

#[derive(Serialize)]
struct CliSubcommand {
    name: String,
    usage: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<Vec<CliFlag>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    examples: Option<&'static [&'static str]>,
}

#[derive(Serialize)]
struct CliReferenceError {
    error: String,
}

/// The placeholder clap prints for an argument's value (`FIELDS` in
/// `--fields <FIELDS>`), falling back to the upper-cased argument id.
fn value_name(arg: &Arg) -> String {
    let Some([name, ..]) = arg.get_value_names() else {
        return arg.get_id().as_str().to_uppercase();
    };

    name.to_string()
}

/// An argument's short help line, empty when the argument is undocumented.
fn help_text(arg: &Arg) -> String {
    arg.get_help().map(ToString::to_string).unwrap_or_default()
}

/// A command's description: the long doc paragraph when it has one, the
/// one-line summary otherwise.
fn description_of(cmd: &Command) -> String {
    cmd.get_long_about()
        .or_else(|| cmd.get_about())
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// The documented non-positional arguments of `cmd`. `--help` / `--version`
/// are clap's own and carry no information for a client.
fn flag_args(cmd: &Command) -> impl Iterator<Item = &Arg> {
    cmd.get_arguments()
        .filter(|arg| !arg.is_positional())
        .filter(|arg| !matches!(arg.get_long(), Some("help" | "version")))
}

/// Render one argument the way `--help` does: `-s, --long <VALUE>`.
fn render_flag(arg: &Arg) -> String {
    let mut rendered = match (arg.get_short(), arg.get_long()) {
        (Some(short), Some(long)) => format!("-{short}, --{long}"),
        (Some(short), None) => format!("-{short}"),
        (None, Some(long)) => format!("--{long}"),
        (None, None) => arg.get_id().as_str().to_owned(),
    };

    if arg.get_action().takes_values() {
        let _ = write!(rendered, " <{}>", value_name(arg));
    }

    rendered
}

/// The invocation line for `cmd`, reached under the command path `path`
/// (empty for the binary itself): positionals first, then the subcommand
/// slot, then the option placeholder.
fn usage_of(cmd: &Command, path: &str) -> String {
    let mut usage = if path.is_empty() {
        BINARY.to_owned()
    } else {
        format!("{BINARY} {path}")
    };

    for arg in cmd.get_positionals() {
        let name = value_name(arg);
        if arg.is_required_set() {
            let _ = write!(usage, " <{name}>");
        } else {
            let _ = write!(usage, " [{name}]");
        }
    }

    if cmd.has_subcommands() {
        usage.push_str(if cmd.is_subcommand_required_set() {
            " <COMMAND>"
        } else {
            " [COMMAND]"
        });
    }

    if flag_args(cmd).next().is_some() {
        usage.push_str(" [OPTIONS]");
    }

    usage
}

fn flags_of(cmd: &Command) -> Option<Vec<CliFlag>> {
    let flags: Vec<CliFlag> = flag_args(cmd)
        .map(|arg| CliFlag {
            flag: render_flag(arg),
            description: help_text(arg),
        })
        .collect();

    (!flags.is_empty()).then_some(flags)
}

fn args_of(cmd: &Command) -> Option<Vec<CliArg>> {
    let args: Vec<CliArg> = cmd
        .get_positionals()
        .map(|arg| CliArg {
            arg: value_name(arg),
            description: help_text(arg),
        })
        .collect();

    (!args.is_empty()).then_some(args)
}

/// Hand-written examples for one command path, if any.
fn examples_for(path: &str) -> Option<&'static [&'static str]> {
    EXAMPLES
        .iter()
        .find(|(key, _)| *key == path)
        .map(|(_, examples)| *examples)
}

/// The direct children of `cmd`, one level deep. `help` is clap's own
/// generated subcommand and is left out.
fn subcommands_of(cmd: &Command, path: &str) -> Option<Vec<CliSubcommand>> {
    let subcommands: Vec<CliSubcommand> = cmd
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
        .map(|sub| {
            let sub_path = format!("{path} {}", sub.get_name());

            CliSubcommand {
                name: sub.get_name().to_owned(),
                usage: usage_of(sub, &sub_path),
                description: description_of(sub),
                flags: flags_of(sub),
                examples: examples_for(&sub_path),
            }
        })
        .collect();

    (!subcommands.is_empty()).then_some(subcommands)
}

fn command_detail(cmd: &Command, path: &str) -> CliCommandDetail {
    CliCommandDetail {
        command: usage_of(cmd, path),
        description: description_of(cmd),
        flags: flags_of(cmd),
        args: args_of(cmd),
        subcommands: subcommands_of(cmd, path),
        examples: examples_for(path),
    }
}

fn overview_of(root: &Command) -> CliOverview {
    CliOverview {
        binary: BINARY,
        description: description_of(root),
        usage: usage_of(root, ""),
        commands: root
            .get_subcommands()
            .filter(|sub| sub.get_name() != "help")
            .map(|sub| CliCommandSummary {
                name: sub.get_name().to_owned(),
                description: sub.get_about().map(ToString::to_string).unwrap_or_default(),
            })
            .collect(),
    }
}

/// Walk a whitespace-separated command path (`"make collection"`) down the
/// clap tree. `None` for an empty path or an unknown segment.
fn resolve<'a>(root: &'a Command, path: &str) -> Option<&'a Command> {
    let mut current = root;
    let mut walked = false;

    for segment in path.split_whitespace() {
        current = current.find_subcommand(segment)?;
        walked = true;
    }

    walked.then_some(current)
}

/// Return CLI reference documentation, optionally filtered by command name.
pub(in crate::mcp::tools) fn exec_cli_reference(command: Option<&str>) -> Result<String> {
    let root = Cli::command();

    let Some(requested) = command else {
        return Ok(to_string_pretty(&overview_of(&root))?);
    };

    let path = requested.split_whitespace().collect::<Vec<_>>().join(" ");

    let Some(cmd) = resolve(&root, &path) else {
        let err = CliReferenceError {
            error: format!(
                "Unknown command: '{requested}'. Call cli_reference without a command argument to see all available commands."
            ),
        };
        return Ok(to_string_pretty(&err)?);
    };

    Ok(to_string_pretty(&command_detail(cmd, &path))?)
}

#[cfg(test)]
mod tests {
    use clap::ArgAction;
    use serde_json::{Value, from_str};

    use super::*;

    fn flag_names(flags: Option<&[CliFlag]>) -> Vec<&str> {
        flags
            .map(|flags| flags.iter().map(|f| f.flag.as_str()).collect())
            .unwrap_or_default()
    }

    /// The overview is the clap top-level command list, not a copy of it.
    #[test]
    fn overview_lists_exactly_the_clap_top_level_commands() {
        let root = Cli::command();
        let expected: Vec<&str> = root
            .get_subcommands()
            .filter(|sub| sub.get_name() != "help")
            .map(Command::get_name)
            .collect();

        let listed: Vec<String> = overview_of(&root)
            .commands
            .into_iter()
            .map(|c| c.name)
            .collect();

        assert_eq!(listed, expected);
    }

    /// A command's subcommands and their flags come from clap, including
    /// the ones a hand-maintained table used to miss (`--confirm`,
    /// `--dry-run`).
    #[test]
    fn a_commands_subcommands_and_flags_come_from_clap() {
        let root = Cli::command();
        let trash = root.find_subcommand("trash").expect("trash is a command");
        let detail = command_detail(trash, "trash");
        let subcommands = detail.subcommands.expect("trash has subcommands");

        let listed: Vec<&str> = subcommands.iter().map(|s| s.name.as_str()).collect();
        let expected: Vec<&str> = trash.get_subcommands().map(Command::get_name).collect();
        assert_eq!(listed, expected);

        let purge = subcommands
            .iter()
            .find(|s| s.name == "purge")
            .expect("trash purge is listed");
        let flags = flag_names(purge.flags.as_deref());

        assert_eq!(
            flags,
            vec![
                "-c, --collection <COLLECTION>",
                "--older-than <OLDER_THAN>",
                "--dry-run",
                "-y, --confirm",
            ]
        );
        assert_eq!(purge.usage, "crap-cms trash purge [OPTIONS]");
    }

    /// Positive control: a command that gains a flag gains it in the
    /// reference too, with no edit to this file.
    #[test]
    fn a_new_flag_is_reflected_without_touching_the_reference() {
        let fake = Command::new("widget").about("Fake command").subcommand(
            Command::new("poke")
                .about("Poke it")
                .arg(
                    Arg::new("hard")
                        .long("hard")
                        .action(ArgAction::SetTrue)
                        .help("Poke harder"),
                )
                .arg(Arg::new("times").short('n').long("times").help("How often")),
        );

        let detail = command_detail(&fake, "widget");
        let subcommands = detail.subcommands.expect("the fake has a subcommand");
        let poke = &subcommands[0];

        assert_eq!(
            flag_names(poke.flags.as_deref()),
            vec!["--hard", "-n, --times <TIMES>"]
        );
        assert_eq!(poke.flags.as_ref().unwrap()[0].description, "Poke harder");
        assert_eq!(detail.command, "crap-cms widget [COMMAND]");
    }

    /// Positional arguments are reported as `args`, not as flags.
    #[test]
    fn positionals_are_reported_as_args() {
        let result = exec_cli_reference(Some("restore")).unwrap();
        let parsed: Value = from_str(&result).unwrap();

        let args = parsed["args"].as_array().unwrap();
        assert_eq!(args[0]["arg"].as_str().unwrap(), "BACKUP");
        assert_eq!(
            parsed["command"].as_str().unwrap(),
            "crap-cms restore <BACKUP> [OPTIONS]"
        );
    }

    /// A subcommand path resolves to that subcommand's own detail.
    #[test]
    fn a_subcommand_path_resolves() {
        let result = exec_cli_reference(Some("make collection")).unwrap();
        let parsed: Value = from_str(&result).unwrap();

        assert_eq!(
            parsed["command"].as_str().unwrap(),
            "crap-cms make collection [SLUG] [OPTIONS]"
        );
        assert!(parsed.get("subcommands").is_none());
    }

    /// Every hand-written example key must name a real command — otherwise
    /// the examples silently stop being served when a command is renamed.
    #[test]
    fn every_example_key_is_a_real_command() {
        let root = Cli::command();

        for (key, _) in EXAMPLES {
            assert!(
                resolve(&root, key).is_some(),
                "EXAMPLES key `{key}` is not a command"
            );
        }
    }

    #[test]
    fn cli_reference_all_commands() {
        let result = exec_cli_reference(None).unwrap();
        let parsed: Value = from_str(&result).unwrap();
        let commands = parsed["commands"].as_array().unwrap();
        assert!(commands.len() >= 15);

        let names: Vec<&str> = commands
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        for expected in &["serve", "migrate", "user", "backup", "jobs", "mcp"] {
            assert!(names.contains(expected), "Missing command: {expected}");
        }
    }

    #[test]
    fn cli_reference_specific_command() {
        let result = exec_cli_reference(Some("migrate")).unwrap();
        let parsed: Value = from_str(&result).unwrap();
        assert!(parsed.get("subcommands").is_some());
        let subs = parsed["subcommands"].as_array().unwrap();
        let sub_names: Vec<&str> = subs.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert!(sub_names.contains(&"up"));
        assert!(sub_names.contains(&"down"));
        assert!(sub_names.contains(&"create"));
        assert!(sub_names.contains(&"list"));
        assert!(sub_names.contains(&"fresh"));
    }

    #[test]
    fn cli_reference_unknown_command() {
        let result = exec_cli_reference(Some("nonexistent")).unwrap();
        let parsed: Value = from_str(&result).unwrap();
        assert!(parsed.get("error").is_some());
    }
}
