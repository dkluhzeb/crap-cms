//! Architectural guard for **upgrade-guide parity**.
//!
//! Every entry under `## [Unreleased]` → `### Breaking` in `CHANGELOG.md`
//! describes something that can break an existing project on upgrade. The
//! upgrade guide (`docs/src/upgrade/alpha-10.md`) is where an operator finds
//! out *what to do about it* — so a Breaking bullet with no guide coverage is
//! a change that ships with a changelog line and no manual.
//!
//! This test fails when a Breaking bullet's lead-in doesn't turn up in the
//! guide and isn't in the reviewed [`ALLOWLIST`]. To make it pass you either:
//!   1. add the guide entry (the default) — a numbered action item when an
//!      operator or schema author must *do* something, a "Behavior changes"
//!      bullet otherwise, an "Additive features" entry for a new key; or
//!   2. if the bullet genuinely needs no guide item of its own — it is covered
//!      by an existing section under different wording, or it changes nothing
//!      an operator, client, or hook author can observe — add it to
//!      [`ALLOWLIST`] with the reason, which forces the decision through
//!      review.
//!
//! **The matching rule.** Each bullet opens with a bold lead-in
//! (`- **Checkbox values are validated.** …`). Both the lead-in and the whole
//! guide are normalized the same way — lowercased, backticks dropped, every
//! run of non-alphanumeric characters collapsed to one space — and the bullet
//! counts as covered when **any four consecutive words of its lead-in appear
//! somewhere in the normalized guide**. Four words is the whole heuristic:
//! long enough that a generic run like "is a validation error" cannot carry a
//! bullet on its own, short enough that the guide may reword the rest of the
//! sentence freely. A lead-in of four words or fewer must appear whole.
//!
//! **Scope & limits of this guard.** It is textual, not semantic. It reads
//! only the *lead-in*, so it cannot tell a thorough guide entry from a passing
//! mention of the same four words, and it says nothing about whether the guide
//! text is right. It reads only the `### Breaking` subsection, so a
//! behavior-changing `### Security` or `### Fixed` bullet is out of scope by
//! design — those are reviewed by hand. And the allowlist is load-bearing:
//! an entry there records a judgement this test cannot re-check. Treat it as a
//! high-signal tripwire for the obvious omission, not proof of coverage.
//!
//! `allowlist_entries_still_match_a_bullet` keeps the allowlist from rotting —
//! an entry whose bullet was reworded or removed fails rather than silently
//! excusing nothing.

use std::{fs, path::Path};

/// How many consecutive lead-in words must survive into the guide.
const WINDOW: usize = 4;

/// Reviewed exceptions: `(normalized lead-in prefix, why no guide item of its
/// own is needed)`. The prefix is matched against the *normalized* lead-in —
/// lowercase, no backticks, single spaces — so write it in that form, long
/// enough to name exactly one bullet.
///
/// Every entry is either "the guide covers this under different wording"
/// (naming the section, so a reviewer can check) or "nothing an operator,
/// client, or hook author can observe changed".
const ALLOWLIST: &[(&str, &str)] = &[
    // ── Nothing user-visible changed ────────────────────────────────────
    (
        "upload storage custom no",
        "Internal Rust constructor contract (creating storage without a Lua \
         runtime). No shipped code path took the fallback, so no operator, \
         client, or hook behavior changes.",
    ),
    // ── Covered by a numbered action item, worded as the action ─────────
    (
        "redis cache keys have",
        "Item 27 (Multi-node operators: explicit secret, separate Redis \
         namespaces).",
    ),
    (
        "jwts are rejected the",
        "Item 29 (API clients: no grace period after a token expires).",
    ),
    (
        "an unknown mcp tool",
        "Item 25 (MCP clients: unknown tools are protocol errors).",
    ),
    (
        "the job payload field",
        "The standalone section at the top of the guide, 'Job payload field \
         renamed to data', which carries the grpcurl diff.",
    ),
    (
        "lifecycle mutations publish their",
        "Item 16 (Event subscribers: undelete, unpublish, restore).",
    ),
    (
        "auth method surfaces entries",
        "Item 12 (Auth methods are strict), which documents strict surfaces \
         entries and surfaces = 'all'.",
    ),
    (
        "tab definitions require a",
        "Item 15 (Filter, select and tab strictness): 'Every tab in a tabs \
         field requires a label.'",
    ),
    (
        "stricter load time validation",
        "Split across the guide by subject: the auth email field is item 40, \
         duplicate custom-page slugs are item 6, block types are item 6a.",
    ),
    (
        "a present but unknown",
        "Item 6 ('Unknown field type' — e.g. type = 'tex').",
    ),
    (
        "crap email send rejects",
        "Item 6 ('crap.email.send { retries = N } is rejected').",
    ),
    (
        "crap jobs define now",
        "Item 6a (Rename job slugs, richtext node names, and block types to \
         valid slugs).",
    ),
    (
        "block type names are",
        "Item 6a (Rename job slugs, richtext node names, and block types to \
         valid slugs).",
    ),
    (
        "cors config is validated",
        "Item 6 ('[cors] in crap.toml is validated').",
    ),
    (
        "join fields reject missing",
        "Item 6c ('Join fields require non-empty string collection and on').",
    ),
    (
        "live collection global setting",
        "Item 6 ('Unknown keys error everywhere'), which lists the \
         per-collection live = { ... } sub-table.",
    ),
    (
        "mcp operations rejects unknown",
        "Item 6 ('Unknown keys error everywhere'), which lists mcp.operations.",
    ),
    (
        "removed the dead live",
        "Item 5 (Remove [live] default_mode from crap.toml), worded as the \
         edit to make.",
    ),
    (
        "lua bulk op queries",
        "Item 2 (Move bulk-op options off the query table).",
    ),
    (
        "lua bulk ops have",
        "Item 2 (Move bulk-op options off the query table) — the dedicated \
         option types are what the move lands in.",
    ),
    (
        "crap collections delete no",
        "Item 9 (Remove locale from crap.collections.delete options).",
    ),
    (
        "lua operation option tables",
        "Items 1 and 7 (unknown keys rejected in Lua CRUD option tables, and \
         in runtime option tables).",
    ),
    (
        "crap jobs queue rejects",
        "Item 7 ('crap.jobs.queue — the options argument accepts only …').",
    ),
    (
        "mcp where clauses reject",
        "Item 42 (Filter clients: one operator grammar on every surface), \
         which lists the valid operators and the removed MCP aliases.",
    ),
    (
        "conflicting generated table names",
        "Item 4 (Rename reserved field names), which covers the generated \
         join-table name collision checked at startup.",
    ),
    // ── Covered by a non-numbered section ───────────────────────────────
    (
        "job run reads are",
        "TL;DR bullet plus the 'Behavior changes' entry 'Job-run reads now \
         honor the job's access function'.",
    ),
    (
        "scheduler behavior changes stabilization",
        "'Behavior changes' entry 'Scheduler reliability + defaults'.",
    ),
    (
        "grpc document values now",
        "'gRPC clients' section: 'Document data/fields are now typed \
         DataMap/FieldValue'.",
    ),
    (
        "grpc dropped always true",
        "'gRPC clients' section: 'Removed always-true success fields'.",
    ),
    (
        "grpc dropped jobdefinitioninfo handler",
        "'gRPC clients' section: 'Removed JobDefinitionInfo.handler'.",
    ),
    (
        "grpc job run shape",
        "'gRPC clients' section: 'JobRunInfo is now the shared job-run \
         message'.",
    ),
    (
        "single relationships are optional",
        "'Generated client types' section: 'A single (non-has_many) \
         relationship is now optional on read'.",
    ),
    (
        "select fields narrow to",
        "'Generated client types' section: 'select fields become a named type, \
         and Rust/Go keep unknown values'.",
    ),
    (
        "polymorphic relationships are typed",
        "'Generated client types' section: 'Polymorphic relationships (a \
         relationship targeting multiple collections) are typed'.",
    ),
];

/// Read a repo file relative to the crate root.
fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);

    match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => panic!("cannot read {}: {e}", path.display()),
    }
}

/// Lowercase, drop backticks, collapse every non-alphanumeric run to one
/// space. Applied identically to the lead-ins and to the whole guide, so the
/// two are always compared in the same alphabet.
fn normalize(text: &str) -> String {
    let lowered = text.replace('`', "").to_lowercase();
    let words: Vec<&str> = lowered
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    words.join(" ")
}

/// The `## [Unreleased]` section of the changelog, up to the next release
/// heading.
fn unreleased_section(changelog: &str) -> &str {
    let start = changelog
        .find("## [Unreleased]")
        .expect("CHANGELOG.md has no `## [Unreleased]` section");
    let body = start + 3;

    changelog[body..]
        .find("\n## [")
        .map_or(&changelog[start..], |end| &changelog[start..body + end])
}

/// The `### Breaking` subsection of `section`, up to the next `### ` heading.
fn breaking_subsection(section: &str) -> &str {
    let start = section
        .find("### Breaking")
        .expect("the `## [Unreleased]` section has no `### Breaking` subsection");
    let body = start + 4;

    section[body..]
        .find("\n### ")
        .map_or(&section[start..], |end| &section[start..body + end])
}

/// Every bullet of `breaking`, its wrapped lines folded back in.
///
/// A line whose first non-space characters are `- ` opens a bullet at any
/// indentation, so a sub-bullet is a bullet in its own right and is checked
/// like every other one. Any other indented line is a wrapped continuation of
/// the bullet above it — a bullet's lead-in wraps with it, so it has to be
/// folded back before the `**…**` span is read.
fn bullets(breaking: &str) -> Vec<String> {
    let mut parsed: Vec<String> = Vec::new();

    for line in breaking.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("- ") {
            parsed.push(rest.to_string());
            continue;
        }

        if line.starts_with("  ")
            && let Some(current) = parsed.last_mut()
        {
            current.push(' ');
            current.push_str(line.trim());
        }
    }

    parsed
}

/// The normalized bold lead-in of every `- **…**` bullet in `breaking`.
fn lead_ins(breaking: &str) -> Vec<String> {
    let mut leads = Vec::new();

    for bullet in bullets(breaking) {
        if let Some(lead) = bold_lead_in(&bullet) {
            leads.push(normalize(lead));
        }
    }

    leads
}

/// The text between the opening `**` and the next `**`, when the bullet opens
/// with one.
fn bold_lead_in(bullet: &str) -> Option<&str> {
    let rest = bullet.strip_prefix("**")?;
    let end = rest.find("**")?;

    Some(&rest[..end])
}

/// Whether any [`WINDOW`]-word run of `lead` occurs in `guide`. A lead-in of
/// [`WINDOW`] words or fewer must occur whole.
fn covered(lead: &str, guide: &str) -> bool {
    let words: Vec<&str> = lead.split(' ').collect();

    if words.len() <= WINDOW {
        return guide.contains(lead);
    }

    words
        .windows(WINDOW)
        .any(|run| guide.contains(run.join(" ").as_str()))
}

/// Whether a reviewed [`ALLOWLIST`] entry excuses this lead-in.
fn allowlisted(lead: &str) -> bool {
    ALLOWLIST.iter().any(|(prefix, _)| lead.starts_with(prefix))
}

/// Every lead-in of the current `### Breaking` subsection.
fn breaking_lead_ins() -> Vec<String> {
    let changelog = read("CHANGELOG.md");

    lead_ins(breaking_subsection(unreleased_section(&changelog)))
}

#[test]
fn every_breaking_change_has_an_upgrade_guide_entry() {
    let guide = normalize(&read("docs/src/upgrade/alpha-10.md"));
    let leads = breaking_lead_ins();

    assert!(
        leads.len() > 20,
        "only {} Breaking lead-in(s) parsed out of `## [Unreleased]` — the \
         changelog layout probably changed and this guard has gone vacuous",
        leads.len()
    );

    let uncovered: Vec<String> = leads
        .iter()
        .filter(|lead| !covered(lead, &guide) && !allowlisted(lead))
        .map(|lead| format!("  {lead}"))
        .collect();

    assert!(
        uncovered.is_empty(),
        "Breaking change(s) in CHANGELOG.md `## [Unreleased]` with no entry in \
         docs/src/upgrade/alpha-10.md.\n\
         Add the guide entry — a numbered action item when someone must do \
         something, a \"Behavior changes\" bullet otherwise, an \"Additive \
         features\" entry for a new key — wording it so four consecutive words \
         of the changelog lead-in survive into the guide. If the bullet \
         genuinely needs no guide item, add it to ALLOWLIST in \
         tests/upgrade_guide_parity.rs with the reason.\n\n{}",
        uncovered.join("\n")
    );
}

#[test]
fn allowlist_entries_still_match_a_bullet() {
    let leads = breaking_lead_ins();

    let stale: Vec<String> = ALLOWLIST
        .iter()
        .filter(|(prefix, _)| !leads.iter().any(|lead| lead.starts_with(prefix)))
        .map(|(prefix, _)| format!("  {prefix}"))
        .collect();

    assert!(
        stale.is_empty(),
        "ALLOWLIST entr(ies) in tests/upgrade_guide_parity.rs no longer match \
         any `### Breaking` bullet — the bullet was reworded or removed, so \
         the exemption excuses nothing and would hide the next omission. \
         Update the prefix or drop the entry.\n\n{}",
        stale.join("\n")
    );
}

#[test]
fn allowlist_prefixes_name_exactly_one_bullet() {
    let leads = breaking_lead_ins();

    let ambiguous: Vec<String> = ALLOWLIST
        .iter()
        .filter_map(|(prefix, _)| {
            let hits = leads.iter().filter(|lead| lead.starts_with(prefix)).count();
            (hits > 1).then(|| format!("  {prefix}  ({hits} bullets)"))
        })
        .collect();

    assert!(
        ambiguous.is_empty(),
        "ALLOWLIST prefix(es) match more than one `### Breaking` bullet, so one \
         reviewed exemption silently covers a bullet nobody looked at. \
         Lengthen the prefix until it names exactly one.\n\n{}",
        ambiguous.join("\n")
    );
}

#[test]
fn normalize_folds_case_backticks_and_punctuation() {
    assert_eq!(
        normalize("**`crap.jobs.queue` rejects unknown option keys.**"),
        "crap jobs queue rejects unknown option keys"
    );
}

#[test]
fn lead_in_is_taken_from_the_bold_span_only() {
    assert_eq!(
        bold_lead_in("**Tab definitions require a `label`.** A tab inside …"),
        Some("Tab definitions require a `label`.")
    );
    assert_eq!(bold_lead_in("A bullet with no bold lead-in."), None);
}

#[test]
fn a_wrapped_lead_in_is_folded_before_it_is_read() {
    let breaking = "### Breaking\n\n\
                    - **Numbers ignore surrounding whitespace, and any\n  \
                    non-zero number checks a checkbox.** The rest.\n";

    assert_eq!(
        lead_ins(breaking),
        vec!["numbers ignore surrounding whitespace and any non zero number checks a checkbox"]
    );
}

/// A parent bullet whose lead-in wraps, a sub-bullet under it, and a wrapped
/// line under the sub-bullet — the three shapes the bullet scan tells apart.
const NESTED_BULLETS: &str = "### Breaking\n\n\
                              - **A parent bullet whose lead-in\n  \
                              wraps.** Intro text:\n  \
                              - **A sub-bullet.** Its own entry.\n    \
                              A wrapped line.\n";

/// A sub-bullet carries a breaking change of its own, so it is checked on its
/// own instead of disappearing into the bullet above it.
#[test]
fn an_indented_sub_bullet_is_a_bullet_of_its_own() {
    let leads = lead_ins(NESTED_BULLETS);

    assert_eq!(leads.len(), 2);
    assert_eq!(leads[0], "a parent bullet whose lead in wraps");
    assert_eq!(leads[1], "a sub bullet");
}

#[test]
fn a_wrapped_line_folds_into_the_bullet_above_it() {
    let parsed = bullets(NESTED_BULLETS);

    assert_eq!(parsed.len(), 2);
    assert!(parsed[0].ends_with("wraps.** Intro text:"));
    assert!(parsed[1].ends_with("Its own entry. A wrapped line."));
}

#[test]
fn four_shared_words_cover_a_bullet_and_three_do_not() {
    let guide = normalize("The admin form now rejects a checkbox value it does not know.");

    // Five words, of which the first four are in the guide verbatim.
    assert!(covered("now rejects a checkbox value", &guide));
    // Shares three words with the guide ("a checkbox value") — not enough.
    assert!(!covered("accepts a checkbox value", &guide));
}

#[test]
fn a_short_lead_in_must_appear_whole() {
    let guide = normalize("Drafts are gated per view.");

    assert!(covered("gated per view", &guide));
    assert!(!covered("gated per reader", &guide));
}

/// Positive control: the scan must still fire on a bullet the guide never
/// mentions. Without it the parity test could pass by parsing nothing.
#[test]
fn the_scan_fires_on_an_uncovered_bullet() {
    let breaking = "### Breaking\n\n- **Widgets are frobnicated on load.** Details.\n";
    let guide = normalize("An upgrade guide that says nothing about widgets.");
    let leads = lead_ins(breaking);

    assert_eq!(leads.len(), 1);
    assert!(!covered(&leads[0], &guide));
    assert!(!allowlisted(&leads[0]));
}

#[test]
fn section_slicing_stops_at_the_next_heading() {
    let changelog = "# CL\n\n## [Unreleased]\n\n### Breaking\n\n- **A.** x\n\n\
                     ### Security\n\n- **B.** y\n\n## [0.1.0-alpha.9]\n\n- **C.** z\n";
    let breaking = breaking_subsection(unreleased_section(changelog));

    assert!(breaking.contains("**A.**"));
    assert!(!breaking.contains("**B.**"));
    assert!(!breaking.contains("**C.**"));
}
