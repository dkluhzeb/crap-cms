//! Guard: the shipped example project migrates cleanly.
//!
//! `example/` is the project new users run first, and its seed migration
//! writes through every public write path — rich text, polymorphic
//! relationships, auth users, blocks. Nothing else runs it, so a write-rule
//! change that the seed violates would ship unnoticed. This test copies the
//! project (without its local `data/` and `uploads/`) into a temp directory
//! and runs `migrate up` — the same entry point as `crap-cms migrate up`:
//! schema sync, then every Lua data migration.
//!
//! No mail leaves the test: the copy's SMTP settings are blanked (the
//! example reads them from `CRAP_SMTP_*`, which a developer machine may
//! set), and `migrate` runs no jobs — the inquiry notifications the seed
//! queues stay queued.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crap_cms::{
    commands::{self, MigrateAction, load_config},
    db::{BoxedConnection, DbConnection as _, pool},
};
use mlua::{Function, Lua, Table};
use serde_json::Value;

/// The example's local state, never part of the project definition.
const SKIPPED: [&str; 2] = ["data", "uploads"];

/// The SMTP settings blanked in the copy.
const SMTP_KEYS: [&str; 3] = ["smtp_host", "smtp_user", "smtp_pass"];

fn example_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("example")
}

/// Copy `src` into `dst` recursively, skipping the top-level `skip` names.
fn copy_project(src: &Path, dst: &Path, skip: &[&str]) {
    fs::create_dir_all(dst).expect("create destination");

    for entry in fs::read_dir(src).expect("read source dir").flatten() {
        let name = entry.file_name();
        if skip.iter().any(|s| name.to_string_lossy() == *s) {
            continue;
        }

        let from = entry.path();
        let to = dst.join(&name);

        if from.is_dir() {
            copy_project(&from, &to, &[]);
        } else {
            fs::copy(&from, &to).expect("copy file");
        }
    }
}

/// `crap.toml` with every SMTP credential replaced by an empty value, so no
/// environment variable can point the run at a real mail server.
fn without_smtp(toml: &str) -> String {
    toml.lines()
        .map(|line| {
            let key = line.split('=').next().unwrap_or("").trim();

            if SMTP_KEYS.contains(&key) {
                format!("{key} = \"\"")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn count(conn: &BoxedConnection, table: &str) -> i64 {
    conn.query_one(&format!("SELECT COUNT(*) AS n FROM {table}"), &[])
        .expect("count query")
        .expect("count row")
        .get_i64("n")
        .expect("integer count")
}

fn post_content(conn: &BoxedConnection, slug: &str) -> Value {
    let row = conn
        .query_one(
            &format!("SELECT content FROM posts WHERE slug = '{slug}'"),
            &[],
        )
        .expect("content query")
        .expect("post row");
    let text = row.get_string("content").expect("content column");

    serde_json::from_str(&text).expect("content is a JSON document")
}

#[test]
fn smtp_settings_are_blanked() {
    let toml = "[email]\nsmtp_host = \"${CRAP_SMTP_HOST:-}\"\nsmtp_port = 587\n\
                smtp_user = \"${CRAP_SMTP_USER:-}\"\nsmtp_pass = \"x\"\n";

    let blanked = without_smtp(toml);

    assert!(blanked.contains("smtp_host = \"\""));
    assert!(blanked.contains("smtp_user = \"\""));
    assert!(blanked.contains("smtp_pass = \"\""));
    assert!(blanked.contains("smtp_port = 587"));
    assert!(!blanked.contains("CRAP_SMTP"));
}

/// Regression: the seed wrote HTML into the JSON rich-text field
/// `posts.content` and a `{ collection, value }` table into the polymorphic
/// `related_content`, so `migrate up` on a fresh example failed.
#[test]
fn the_example_project_migrates_and_seeds() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("example");
    copy_project(&example_dir(), &project, &SKIPPED);

    let toml_path = project.join("crap.toml");
    let toml = fs::read_to_string(&toml_path).expect("read crap.toml");
    fs::write(&toml_path, without_smtp(&toml)).expect("write crap.toml");

    commands::db::migrate(&project, &MigrateAction::Up).expect("migrate up succeeds");

    let cfg = load_config(&project).expect("config loads");
    let db = pool::create_pool(&project, &cfg).expect("pool");
    let conn = db.get().expect("connection");

    assert_eq!(count(&conn, "users"), 6);
    assert_eq!(count(&conn, "categories"), 6);
    assert_eq!(count(&conn, "tags"), 12);
    assert_eq!(count(&conn, "posts"), 25);
    assert!(count(&conn, "projects") > 0);
    assert!(count(&conn, "pages") > 0);

    let content = post_content(&conn, "motion-design-web");
    assert_eq!(content["type"], "doc");
    assert_eq!(content["content"][0]["type"], "heading");
    assert_eq!(
        content["content"][2]["content"][1]["marks"][0]["type"],
        "strong"
    );
}

/// Load the example's `reading_time` field hook in a bare VM: `field_hook`
/// only passes the function through, so a stub stands in for the typing
/// factory the real VM provides.
fn reading_time_hook(lua: &Lua) -> Function {
    let hooks_dir = example_dir().join("?.lua");
    let path = hooks_dir.to_str().expect("utf-8 path");

    lua.load(format!(
        r#"package.path = "{path};" .. package.path
        crap = {{ collections = {{ posts = {{
          field_hook = function(_, fn) return fn end,
        }} }} }}"#
    ))
    .exec()
    .expect("stub the typing factory");

    lua.load(r#"return require("hooks.reading_time")"#)
        .eval()
        .expect("load the hook")
}

/// Regression: the hook read its own (empty) `reading_time` value and treated
/// it as HTML, so every post read "1 min read" — and a JSON-document `content`
/// raised on `value:gsub`. It now counts the words of `ctx.data.content` in
/// either form.
#[test]
fn reading_time_counts_the_content_words() {
    let lua = Lua::new();
    let hook = reading_time_hook(&lua);

    let words = "word ".repeat(450);
    let doc: Table = lua
        .load(format!(
            r#"return {{ data = {{ content = {{ type = "doc", content = {{
              {{ type = "paragraph", content = {{ {{ type = "text", text = "{words}" }} }} }},
            }} }} }} }}"#
        ))
        .eval()
        .unwrap();
    let html: Table = lua
        .load(format!(
            r#"return {{ data = {{ content = "<p>{words}</p>" }} }}"#
        ))
        .eval()
        .unwrap();
    let empty: Table = lua.load("return { data = {} }").eval().unwrap();

    let read = |ctx: Table| hook.call::<String>(("", ctx)).expect("hook runs");

    assert_eq!(read(doc), "3 min read");
    assert_eq!(read(html), "3 min read");
    assert_eq!(read(empty), "1 min read");
}
