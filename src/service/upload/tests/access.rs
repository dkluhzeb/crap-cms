//! A caller the collection refuses never gets as far as storing a file, and a
//! rule consulted before the file is stored judges the file's own columns.

use std::{fs, path::Path};

use super::support::*;
use crate::core::{HookRef, upload::FALLBACK_MAX_ATTEMPTS};
use crate::service::{AppInfra, upload::*};

/// Install an access function `access.<name>` with the given Lua body.
fn install_rule(config_dir: &Path, name: &str, body: &str) {
    let dir = config_dir.join("access");
    fs::create_dir_all(&dir).expect("access dir");
    fs::write(dir.join(format!("{name}.lua")), body).expect("write access fn");
}

/// `access.deny`: refuses everyone.
fn install_deny_rule(config_dir: &Path) {
    install_rule(
        config_dir,
        "deny",
        "return function()\n    return false\nend\n",
    );
}

/// `access.png_only`: allows a write whose data is a PNG file — a rule that
/// reads a column the server derives from the file.
fn install_png_only_rule(config_dir: &Path) {
    install_rule(
        config_dir,
        "png_only",
        "return function(ctx)\n    return ctx.data ~= nil and ctx.data.mime_type == \"image/png\"\nend\n",
    );
}

/// How many files sit under `dir`, at any depth.
fn files_under(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };

    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();

            if path.is_dir() { files_under(&path) } else { 1 }
        })
        .sum()
}

/// Every file stored under the test's uploads directory.
fn stored_files(config_dir: &Path) -> usize {
    files_under(&config_dir.join("uploads"))
}

fn create_input<'a>(
    infra: &'a AppInfra,
    def: &CollectionDefinition,
    file: &'a UploadedFile,
) -> CreateUploadInput<'a> {
    CreateUploadInput {
        storage: &infra.storage,
        file,
        form: empty_form(def),
        locale_ctx: None,
        password: None,
        draft: false,
        upload_max_file_size: MAX_FILE_SIZE,
        image_max_attempts: FALLBACK_MAX_ATTEMPTS,
    }
}

fn update_input<'a>(
    infra: &'a AppInfra,
    def: &CollectionDefinition,
    id: &'a str,
    file: UploadedFile,
) -> UpdateUploadInput<'a> {
    UpdateUploadInput {
        id,
        storage: &infra.storage,
        file: Some(file),
        form: empty_form(def),
        locale_ctx: None,
        password: None,
        draft: false,
        upload_max_file_size: MAX_FILE_SIZE,
        image_max_attempts: FALLBACK_MAX_ATTEMPTS,
        form_echoes_locked_fields: false,
    }
}

/// Regression: an upload create stored the file — the whole image pipeline
/// included — before the collection's `create` rule was consulted, so a caller
/// with no create access could make the server do all of it per request. The
/// rule now refuses before anything is stored.
#[test]
fn a_create_the_rule_denies_never_stores_the_file() {
    let mut def = media_with_drafts();
    def.access.create = Some(HookRef::new("access.deny"));

    let (tmp, infra, def) = infra_for(def);
    install_deny_rule(tmp.path());

    let ctx = ServiceContext::collection("media", &def)
        .infra(&infra)
        .build();

    let file = image_file("a.png", 16);
    let err = create_upload(&ctx, create_input(&infra, &def, &file))
        .err()
        .expect("the rule denies the create");

    assert!(
        matches!(err, ServiceError::AccessDenied(_)),
        "access is judged before the file is stored: {err:?}"
    );
    assert_eq!(stored_files(tmp.path()), 0);
}

/// The same for a file-bearing update the collection's `update` rule denies.
#[test]
fn an_update_the_rule_denies_never_stores_the_file() {
    let (tmp, infra, def) = infra();
    let published = create(&infra, &def, &image_file("a.png", 16));
    let before = stored_files(tmp.path());

    let mut denied = def;
    denied.access.update = Some(HookRef::new("access.deny"));
    install_deny_rule(tmp.path());

    let ctx = ServiceContext::collection("media", &denied)
        .infra(&infra)
        .build();

    let input = update_input(&infra, &denied, &published.id, image_file("b.png", 16));
    let err = update_upload(&ctx, input)
        .err()
        .expect("the rule denies the update");

    assert!(
        matches!(err, ServiceError::AccessDenied(_)),
        "access is judged before the file is stored: {err:?}"
    );
    assert_eq!(stored_files(tmp.path()), before, "no new file is stored");
}

/// Regression: the pre-check judged the rule on the request without the
/// file's columns, so a `create` rule reading `ctx.data.mime_type` saw `nil`
/// and refused every upload before the file was even looked at. The pre-check
/// now sees the columns the file determines before it is stored, and agrees
/// with the write's own check.
#[test]
fn a_create_rule_on_the_file_type_judges_the_file() {
    let mut def = media_with_drafts();
    def.access.create = Some(HookRef::new("access.png_only"));

    let (tmp, infra, def) = infra_for(def);
    install_png_only_rule(tmp.path());

    let ctx = ServiceContext::collection("media", &def)
        .infra(&infra)
        .build();

    let png = image_file("a.png", 16);
    let created = create_upload(&ctx, create_input(&infra, &def, &png))
        .expect("the rule allows a PNG")
        .doc;
    assert_eq!(created.get_str("mime_type"), Some("image/png"));

    let stored = stored_files(tmp.path());

    let text = file("notes.txt", b"plain text");
    let err = create_upload(&ctx, create_input(&infra, &def, &text))
        .err()
        .expect("the rule refuses a text file");

    assert!(matches!(err, ServiceError::AccessDenied(_)), "{err:?}");
    assert_eq!(
        stored_files(tmp.path()),
        stored,
        "the refused file is not stored"
    );
}

/// The same for an `update` rule: replacing the file with a PNG passes, with a
/// text file is refused before it is stored.
#[test]
fn an_update_rule_on_the_file_type_judges_the_replacement() {
    let (tmp, infra, def) = infra();
    let published = create(&infra, &def, &image_file("a.png", 16));

    let mut guarded = def;
    guarded.access.update = Some(HookRef::new("access.png_only"));
    install_png_only_rule(tmp.path());

    let ctx = ServiceContext::collection("media", &guarded)
        .infra(&infra)
        .build();

    let input = update_input(&infra, &guarded, &published.id, image_file("b.png", 16));
    update_upload(&ctx, input).expect("the rule allows a PNG replacement");

    let stored = stored_files(tmp.path());

    let input = update_input(&infra, &guarded, &published.id, file("notes.txt", b"text"));
    let err = update_upload(&ctx, input)
        .err()
        .expect("the rule refuses a text replacement");

    assert!(matches!(err, ServiceError::AccessDenied(_)), "{err:?}");
    assert_eq!(
        stored_files(tmp.path()),
        stored,
        "the refused file is not stored"
    );
}
