//! Client-side display conditions (a condition hook that returns a table) —
//! the table must re-evaluate instantly for every control shape, including
//! a radio group, where the change event fires on the option that was
//! clicked rather than on the first input carrying the name.

use std::{fs, time::Duration};

use chromiumoxide::Page;
use serde_json::{Map, Value, json};
use tokio::time::sleep;

use crap_cms::{
    config::CrapConfig,
    core::{collection::*, field::*},
    db::DbConnection,
};
use crap_cms_e2e::{BrowserTestCtx, browser, helpers::*, setup_browser_test_at};

fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("post_type", FieldType::Radio)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Article".into()), "article"),
                SelectOption::new(LocalizedString::Plain("Link".into()), "link"),
            ])
            .build(),
        FieldDefinition::builder("link_url", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .condition("hooks.conditions.link_only")
                    .build(),
            )
            .build(),
    ];
    def
}

/// Boot with a `hooks/conditions/link_only.lua` that returns a condition
/// TABLE (client-evaluated), not a boolean.
fn prepared_config_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks").join("conditions");
    fs::create_dir_all(&hooks_dir).expect("mkdir hooks/conditions");
    fs::write(
        hooks_dir.join("link_only.lua"),
        r#"
return function(_ctx)
    return { field = "post_type", equals = "link" }
end
"#,
    )
    .expect("write hook file");
    tmp
}

const LINK_URL_HIDDEN: &str = "document.querySelector('[data-field-name=\"link_url\"]')?.classList.contains('form__field--hidden')";

#[tokio::test(flavor = "multi_thread")]
async fn radio_group_drives_a_client_side_condition() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    // `app` owns the temp config dir the condition hook is loaded from at
    // request time — bind it (a `..` pattern would drop it, deleting the dir
    // and silently un-resolving the hook), like `browser` keeps CDP alive.
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup_browser_test_at(
        vec![make_posts_def(), make_users_def()],
        vec![],
        config,
        prepared_config_dir(),
        "bcond1@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/posts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();

    // Nothing selected yet: the conditioned field starts hidden.
    assert!(
        browser::wait_for_js(&page, &format!("{LINK_URL_HIDDEN} === true")).await,
        "link_url is hidden until post_type = link"
    );
    browser::wait_for_js(&page, "customElements.get('crap-conditions')").await;

    // Clicking the SECOND radio (not the first, which is the only one a
    // first-match listener would watch) must reveal the field instantly.
    page.find_element("input[name=\"post_type\"][value=\"link\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(&page, &format!("{LINK_URL_HIDDEN} === false")).await,
        "link_url appears once the 'link' radio is chosen"
    );

    // And switching back hides it again.
    page.find_element("input[name=\"post_type\"][value=\"article\"]")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(&page, &format!("{LINK_URL_HIDDEN} === true")).await,
        "link_url hides again for 'article'"
    );

    server_handle.abort();
}

// ── typed condition data: checkbox, number, group sub-field, defaults ───

/// An `events` collection shaped like the shipped example: a checkbox
/// (`online`, checked by default) gating a URL, a number gating a note, and a
/// group sub-field gating another.
fn make_events_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("events");
    def.timestamps = true;

    let conditioned = |name: &str, hook: &str| {
        FieldDefinition::builder(name, FieldType::Text)
            .admin(FieldAdmin::builder().condition(hook).build())
            .build()
    };

    def.fields = vec![
        FieldDefinition::builder("online", FieldType::Checkbox)
            .default_value(json!(true))
            .build(),
        conditioned("event_url", "hooks.conditions.online_only"),
        FieldDefinition::builder("seats", FieldType::Number).build(),
        conditioned("big_room", "hooks.conditions.five_seats"),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build(),
        conditioned("seo_note", "hooks.conditions.titled"),
    ];
    def
}

/// The three table conditions, spelled as a config author writes them.
fn typed_config_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks").join("conditions");
    fs::create_dir_all(&hooks_dir).expect("mkdir hooks/conditions");

    for (name, table) in [
        ("online_only", r#"{ field = "online", equals = true }"#),
        ("five_seats", r#"{ field = "seats", equals = 5 }"#),
        ("titled", r#"{ field = "seo.title", equals = "x" }"#),
    ] {
        fs::write(
            hooks_dir.join(format!("{name}.lua")),
            format!("return function(_ctx)\n    return {table}\nend\n"),
        )
        .expect("write hook file");
    }
    tmp
}

/// JS: whether the wrapper of `name` is hidden.
fn hidden(name: &str) -> String {
    format!(
        "document.querySelector('[data-field-name=\"{name}\"]')?.classList.contains('form__field--hidden')"
    )
}

/// Regression: the browser evaluated table conditions against raw strings —
/// a checkbox as `"on"`/`""`, a number as `"5"`, a group sub-field under its
/// input name — while the server rendered against typed, nested values. The
/// shipped example's `{ field = "online", equals = true }` hid its field on
/// the first uncheck and never showed it again, and the create form ignored
/// the checkbox's default.
#[tokio::test(flavor = "multi_thread")]
async fn table_conditions_see_typed_values_live_and_on_create() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app: _app,
        ..
    } = setup_browser_test_at(
        vec![make_events_def(), make_users_def()],
        vec![],
        config,
        typed_config_dir(),
        "bcond2@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/events/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_js(&page, "customElements.get('crap-conditions')").await;

    assert!(
        browser::wait_for_js(&page, &format!("{} === false", hidden("event_url"))).await,
        "the create form conditions on the checkbox's default (checked)"
    );

    let online = "input[type=\"checkbox\"][name=\"online\"]";
    page.find_element(online)
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(&page, &format!("{} === true", hidden("event_url"))).await,
        "unchecking hides the URL"
    );

    page.find_element(online)
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    assert!(
        browser::wait_for_js(&page, &format!("{} === false", hidden("event_url"))).await,
        "checking again shows it — a checked box is `true`, not `\"on\"`"
    );

    page.evaluate(
        "() => { \
           for (const [name, value] of [['seats', '5'], ['seo__title', 'x']]) { \
             const el = document.querySelector(`[name=\"${name}\"]`); \
             el.value = value; \
             el.dispatchEvent(new Event('input', { bubbles: true })); \
           } \
         }",
    )
    .await
    .unwrap();

    assert!(
        browser::wait_for_js(&page, &format!("{} === false", hidden("big_room"))).await,
        "a number input compares as a number"
    );
    assert!(
        browser::wait_for_js(&page, &format!("{} === false", hidden("seo_note"))).await,
        "a group sub-field resolves by its dotted path"
    );

    server_handle.abort();
}

// ── one decode on both sides: the browser and the server agree ──────────

/// Every conditioned field of the parity collection, the table its hook
/// returns, and whether the condition holds for the values the test enters.
/// Each value shape the write decodes is covered: a checkbox, numbers (an
/// integer compared with a float), NFC text, an email, a `has_many` list, a
/// group sub-field, a day, a UTC date and time, a date and time in a chosen
/// timezone and a month.
const PARITY_CONDITIONS: &[(&str, &str, bool)] = &[
    ("c_flag", r#"{ field = "flag", equals = true }"#, true),
    ("c_seats", r#"{ field = "seats", equals = 5.0 }"#, true),
    ("n_seats", r#"{ field = "seats", not_equals = 5 }"#, false),
    ("c_ratio", r#"{ field = "ratio", equals = 2.5 }"#, true),
    (
        "c_name",
        "{ field = \"name\", equals = \"Caf\u{e9}\" }",
        true,
    ),
    (
        "c_mail",
        r#"{ field = "mail", equals = "foo@example.com" }"#,
        true,
    ),
    (
        "c_tags",
        r#"{ field = "tags", equals = { "a", "b" } }"#,
        true,
    ),
    ("c_seo", r#"{ field = "seo.title", equals = "x" }"#, true),
    (
        "c_day",
        r#"{ field = "day", equals = "2026-01-15T12:00:00.000Z" }"#,
        true,
    ),
    (
        "n_day",
        r#"{ field = "day", equals = "2026-01-15" }"#,
        false,
    ),
    (
        "c_at",
        r#"{ field = "at", equals = "2026-01-15T09:30:00.000Z" }"#,
        true,
    ),
    (
        "c_meet",
        r#"{ field = "meet", equals = "2026-07-15T07:00:00.000Z" }"#,
        true,
    ),
    (
        "c_month",
        r#"{ field = "month", equals = "2026-01" }"#,
        true,
    ),
];

/// A `facts` collection holding one field of every value shape and one
/// conditioned text field per [`PARITY_CONDITIONS`] row.
fn make_facts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("facts");
    def.timestamps = true;

    let option = |value: &str| SelectOption::new(LocalizedString::Plain(value.into()), value);
    let date = |name: &str, appearance: PickerAppearance| {
        FieldDefinition::builder(name, FieldType::Date)
            .picker_appearance(appearance)
            .build()
    };

    def.fields = vec![
        FieldDefinition::builder("flag", FieldType::Checkbox).build(),
        FieldDefinition::builder("seats", FieldType::Number).build(),
        FieldDefinition::builder("ratio", FieldType::Number).build(),
        FieldDefinition::builder("name", FieldType::Text).build(),
        FieldDefinition::builder("mail", FieldType::Email).build(),
        FieldDefinition::builder("tags", FieldType::Select)
            .options(vec![option("a"), option("b")])
            .has_many(true)
            .build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build(),
        date("day", PickerAppearance::DayOnly),
        date("at", PickerAppearance::DayAndTime),
        FieldDefinition::builder("meet", FieldType::Date)
            .picker_appearance(PickerAppearance::DayAndTime)
            .timezone(true)
            .build(),
        date("month", PickerAppearance::MonthOnly),
    ];

    def.fields
        .extend(PARITY_CONDITIONS.iter().map(|(name, _, _)| {
            FieldDefinition::builder(*name, FieldType::Text)
                .admin(
                    FieldAdmin::builder()
                        .condition(format!("hooks.conditions.{name}"))
                        .build(),
                )
                .build()
        }));
    def
}

/// One condition hook per [`PARITY_CONDITIONS`] row, returning its table.
fn parity_config_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks_dir = tmp.path().join("hooks").join("conditions");
    fs::create_dir_all(&hooks_dir).expect("mkdir hooks/conditions");

    for (name, table, _) in PARITY_CONDITIONS {
        fs::write(
            hooks_dir.join(format!("{name}.lua")),
            format!("return function(_ctx)\n    return {table}\nend\n"),
        )
        .expect("write hook file");
    }
    tmp
}

/// Enter every value the parity conditions judge, the way an editor does:
/// text typed as written (a decomposed accent, a mixed-case email), local
/// dates and times, a timezone picked beside `meet`.
const ENTER_PARITY_VALUES: &str = r#"() => {
    const set = (selector, value) => {
        const el = document.querySelector(selector);
        el.value = value;
        el.dispatchEvent(new Event('input', { bubbles: true }));
    };
    document.querySelector('input[type="checkbox"][name="flag"]').checked = true;
    set('input[name="seats"]', '5');
    set('input[name="ratio"]', '2.50');
    set('input[name="name"]', 'Cafe\u0301');
    set('input[name="mail"]', 'Foo@Example.COM');
    for (const o of document.querySelector('select[name="tags"]').options) o.selected = true;
    set('input[name="seo__title"]', 'x');
    set('input[name="day"]', '2026-01-15');
    set('input[name="at"]', '2026-01-15T09:30');
    set('input[name="meet"]', '2026-07-15T09:00');
    set('select[name="meet_tz"]', 'Europe/Berlin');
    set('input[name="month"]', '2026-01');
    document.querySelector('#edit-form').dispatchEvent(new Event('change', { bubbles: true }));
}"#;

/// Whether each conditioned field is shown, by name.
async fn shown(page: &Page) -> Value {
    let names: Vec<&str> = PARITY_CONDITIONS.iter().map(|(name, _, _)| *name).collect();
    let script = format!(
        "() => Object.fromEntries({}.map((n) => [n, !document.querySelector(`[data-field-name=\"${{n}}\"]`)?.classList.contains('form__field--hidden')]))",
        json!(names)
    );

    page.evaluate(script).await.unwrap().into_value().unwrap()
}

/// What every conditioned field must show as.
fn expected_shown() -> Value {
    let map: Map<String, Value> = PARITY_CONDITIONS
        .iter()
        .map(|(name, _, holds)| ((*name).to_string(), Value::Bool(*holds)))
        .collect();
    Value::Object(map)
}

/// Poll until the page shows the expected visibility, returning the last one.
async fn settle_shown(page: &Page) -> Value {
    let expected = expected_shown();
    let mut last = Value::Null;

    for _ in 0..50 {
        last = shown(page).await;
        if last == expected {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    last
}

/// Regression: the browser evaluated a date condition against the input's
/// text (`"2026-01-15"`, a local time) while the server evaluated it against
/// the stored UTC instant, and compared an integer with a float as unequal
/// while the browser saw one number — so the same condition showed a field
/// while editing and hid it on reload. The browser now decodes every value
/// exactly as the server stores it: the same conditions decide the same way
/// live, on the server's render of the saved document, and live again on
/// that render.
#[tokio::test(flavor = "multi_thread")]
async fn table_conditions_decide_alike_in_the_browser_and_on_the_server() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let BrowserTestCtx {
        base_url,
        server_handle,
        page,
        browser: _browser,
        app,
        ..
    } = setup_browser_test_at(
        vec![make_facts_def(), make_users_def()],
        vec![],
        config,
        parity_config_dir(),
        "bcond3@test.com",
        "pass123",
    )
    .await;

    page.goto(format!("{base_url}/admin/collections/facts/create"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::wait_for_js(&page, "customElements.get('crap-conditions')").await;

    page.evaluate(ENTER_PARITY_VALUES).await.unwrap();
    assert_eq!(
        settle_shown(&page).await,
        expected_shown(),
        "the browser decodes every entered value as the server stores it"
    );

    page.evaluate("() => document.querySelector('#edit-form')?.requestSubmit()")
        .await
        .unwrap();

    let conn = app.pool.get().unwrap();
    let mut doc_id = String::new();
    for _ in 0..60 {
        if let Some(row) = conn.query_one("SELECT id FROM facts", &[]).unwrap() {
            doc_id = row.get_string("id").unwrap();
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(!doc_id.is_empty(), "the document is saved");

    let stored = conn
        .query_one("SELECT meet, day FROM facts", &[])
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.get_string("meet").unwrap(),
        "2026-07-15T07:00:00.000Z",
        "the chosen zone converted to UTC on save"
    );
    assert_eq!(
        stored.get_string("day").unwrap(),
        "2026-01-15T12:00:00.000Z"
    );

    page.goto(format!("{base_url}/admin/collections/facts/{doc_id}"))
        .await
        .unwrap()
        .wait_for_navigation()
        .await
        .unwrap();
    browser::find_element_after_nav(&page, "[data-field-name=\"c_flag\"]").await;
    assert_eq!(
        shown(&page).await,
        expected_shown(),
        "the server's render of the stored document decides alike"
    );

    browser::wait_for_js(&page, "customElements.get('crap-conditions')").await;
    page.evaluate(
        "() => document.querySelector('#edit-form').dispatchEvent(new Event('change', { bubbles: true }))",
    )
    .await
    .unwrap();
    assert_eq!(
        settle_shown(&page).await,
        expected_shown(),
        "the browser re-decodes the rendered inputs alike"
    );

    server_handle.abort();
}
