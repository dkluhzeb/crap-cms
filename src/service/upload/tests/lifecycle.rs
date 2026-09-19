//! A stored file lives exactly as long as a live row, draft or version
//! snapshot references it.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::support::*;
use crate::service::upload::*;
use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, FieldType, VersionsConfig, upload::FALLBACK_MAX_ATTEMPTS},
    db::{LocaleContext, LocaleMode, query},
    service::{delete_document, unpublish_document},
};

/// Regression: a draft save carrying a new file deleted the file the
/// PUBLISHED row still references. Every live page then 404s and no version
/// brings the bytes back. The published row and its file must both survive
/// a draft save untouched.
#[test]
fn a_draft_save_with_a_new_file_keeps_the_published_file() {
    let (_tmp, infra, def) = infra();

    let published = create(&infra, &def, &file("first.txt", b"first"));
    let published_key = stored_key(&published);

    update(
        &infra,
        &def,
        &published.id,
        Some(file("second.txt", b"second")),
        true,
    );

    assert!(
        infra.storage.exists(&published_key).expect("exists"),
        "the published row still references {published_key}"
    );

    let conn = infra.pool.get().expect("connection");
    let reread = query::find_by_id(&conn, "media", &def, &published.id, None)
        .expect("find")
        .expect("the published row");

    assert_eq!(
        stored_key(&reread),
        published_key,
        "a draft save must not move the published row's file"
    );
}

/// Replacing the file on a published write drops the previous one when
/// nothing else references it — with versions switched off, the row was the
/// only reference, so the bytes go once the write commits.
#[test]
fn a_published_replacement_deletes_an_unreferenced_previous_file() {
    let mut unversioned = media_with_drafts();
    unversioned.versions = None;
    let (_tmp, infra, def) = infra_for(unversioned);

    let published = create(&infra, &def, &file("first.txt", b"first"));
    let first_key = stored_key(&published);

    let updated = update(
        &infra,
        &def,
        &published.id,
        Some(file("second.txt", b"second")),
        false,
    );
    let second_key = stored_key(&updated);

    assert_ne!(first_key, second_key);
    assert!(
        !infra.storage.exists(&first_key).expect("exists"),
        "the replaced file must be gone"
    );
    assert!(
        infra.storage.exists(&second_key).expect("exists"),
        "the stored file must survive the write"
    );
}

/// A replaced file stays in storage while a version snapshot still names
/// it: restoring that version has to find the bytes it was saved with.
#[test]
fn a_replaced_file_survives_while_a_version_references_it() {
    let (_tmp, infra, def) = infra();

    let published = create(&infra, &def, &file("first.txt", b"first"));
    let first_key = stored_key(&published);

    let updated = update(
        &infra,
        &def,
        &published.id,
        Some(file("second.txt", b"second")),
        false,
    );

    assert_ne!(first_key, stored_key(&updated));
    assert!(
        infra.storage.exists(&first_key).expect("exists"),
        "the version created on upload still references {first_key}"
    );
}

/// Pruning the last snapshot that referenced a replaced file releases it:
/// with a one-version cap, the replacement's own snapshot pushes the
/// original out and nothing names the original file any more.
#[test]
fn pruning_the_last_version_that_referenced_a_file_deletes_it() {
    let mut capped = media_with_drafts();
    capped.versions = Some(VersionsConfig::new(true, 1));
    let (_tmp, infra, def) = infra_for(capped);

    let published = create(&infra, &def, &file("first.txt", b"first"));
    let first_key = stored_key(&published);

    let updated = update(
        &infra,
        &def,
        &published.id,
        Some(file("second.txt", b"second")),
        false,
    );

    assert!(
        !infra.storage.exists(&first_key).expect("exists"),
        "the pruned version was the last reference to {first_key}"
    );
    assert!(
        infra.storage.exists(&stored_key(&updated)).expect("exists"),
        "the published file stays"
    );
}

/// A drafted file that never went live is deleted once the snapshot naming
/// it is pruned — here by a newer draft under a one-version cap. The
/// published row's file and the published snapshot are untouched.
#[test]
fn a_superseded_draft_file_is_deleted_once_no_snapshot_names_it() {
    let mut capped = media_with_drafts();
    capped.versions = Some(VersionsConfig::new(true, 1));
    let (_tmp, infra, def) = infra_for(capped);

    let published = create(&infra, &def, &file("first.txt", b"first"));
    let published_key = stored_key(&published);

    let first_draft = update(
        &infra,
        &def,
        &published.id,
        Some(file("second.txt", b"second")),
        true,
    );
    let drafted_key = stored_key(&first_draft);

    let second_draft = update(
        &infra,
        &def,
        &published.id,
        Some(file("third.txt", b"third")),
        true,
    );

    assert!(
        !infra.storage.exists(&drafted_key).expect("exists"),
        "the superseded draft's file has no reference left: {drafted_key}"
    );
    assert!(
        infra.storage.exists(&published_key).expect("exists"),
        "the published row still references {published_key}"
    );
    assert!(
        infra
            .storage
            .exists(&stored_key(&second_draft))
            .expect("exists"),
        "the pending draft's file stays"
    );
}

/// Regression: `locale = "all"` shapes a READ — a write has no such shape,
/// and every operation refuses it. An upload write reaches the write
/// without going through an operation, so it accepted `all`, silently wrote
/// the DEFAULT locale's columns under a locale the caller never named, and
/// skipped the shared-field lock along the way.
#[test]
fn an_all_locales_upload_write_is_refused() {
    let (_tmp, infra, def) = infra();
    let published = create(&infra, &def, &file("first.txt", b"first"));

    let locale_ctx = LocaleContext {
        mode: LocaleMode::All,
        config: LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        },
    };
    let ctx = ServiceContext::collection("media", &def)
        .infra(&infra)
        .build();

    let err = update_upload(
        &ctx,
        UpdateUploadInput {
            id: &published.id,
            storage: &infra.storage,
            file: None,
            form: empty_form(&def),
            locale_ctx: Some(&locale_ctx),
            password: None,
            ui_locale: None,
            draft: false,
            upload_max_file_size: MAX_FILE_SIZE,
            image_max_attempts: FALLBACK_MAX_ATTEMPTS,
            form_echoes_locked_fields: false,
        },
    )
    .err()
    .expect("a write targets one locale");

    let ServiceError::Validation(validation) = err else {
        panic!("expected a validation error, got {err:?}");
    };
    let message = validation
        .to_field_map()
        .get("locale")
        .cloned()
        .expect("the locale field is named");
    assert!(message.contains("'all'"), "unexpected: {message}");
}

/// Regression: unpublishing writes a version like every other lifecycle
/// step, so it prunes like one — and pruning a snapshot can drop a stored
/// file's last reference. Unpublish released nothing, so those bytes were
/// orphaned with no later write able to see them.
#[test]
fn unpublishing_releases_a_file_its_pruning_orphaned() {
    let mut capped = media_with_drafts();
    capped.versions = Some(VersionsConfig::new(true, 2));
    let (_tmp, infra, def) = infra_for(capped);

    let published = create(&infra, &def, &file("first.txt", b"first"));
    let first_key = stored_key(&published);

    update(
        &infra,
        &def,
        &published.id,
        Some(file("second.txt", b"second")),
        false,
    );

    assert!(
        infra.storage.exists(&first_key).expect("exists"),
        "the version created on upload still references {first_key}"
    );

    let ctx = ServiceContext::collection("media", &def)
        .infra(&infra)
        .build();
    unpublish_document(&ctx, &published.id).expect("unpublish");

    assert!(
        !infra.storage.exists(&first_key).expect("exists"),
        "the snapshot the unpublish pruned was the last reference to {first_key}"
    );
}

/// A drafted file that never went live is still this document's file:
/// deleting the document has to take it too. The delete removed only the
/// published row's files, so a file that lived solely in a draft snapshot
/// stayed in storage forever with nothing left to reference it.
#[test]
fn deleting_a_document_removes_a_file_only_its_draft_ever_named() {
    let (_tmp, infra, def) = infra();

    let published = create(&infra, &def, &file("first.txt", b"first"));
    let published_key = stored_key(&published);

    let drafted = update(
        &infra,
        &def,
        &published.id,
        Some(file("second.txt", b"second")),
        true,
    );
    let drafted_key = stored_key(&drafted);
    assert_ne!(published_key, drafted_key);

    let ctx = ServiceContext::collection("media", &def)
        .infra(&infra)
        .build();
    delete_document(&ctx, &published.id, Some(&*infra.storage), None).expect("delete");

    assert!(
        !infra.storage.exists(&published_key).expect("exists"),
        "the published file goes with the document"
    );
    assert!(
        !infra.storage.exists(&drafted_key).expect("exists"),
        "the drafted file goes with the document too"
    );
}

/// Regression: the stored bytes were released only once the whole write
/// — post-commit work included — had returned. A panic in that work
/// unwound through the cleanup guard, which deleted the file the
/// committed row already pointed at. The bytes must stay from the moment
/// the commit is durable, whatever happens afterwards.
#[test]
fn a_post_commit_panic_keeps_the_committed_file() {
    let def = media_with_drafts();
    let (_tmp, infra) = infra_with_post_commit_panic(def.clone());
    let ctx = ServiceContext::collection("media", &def)
        .infra(&infra)
        .build();
    let uploaded = file("first.txt", b"first");

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        create_upload(
            &ctx,
            CreateUploadInput {
                storage: &infra.storage,
                file: &uploaded,
                form: empty_form(&def),
                locale_ctx: None,
                password: None,
                ui_locale: None,
                draft: false,
                upload_max_file_size: MAX_FILE_SIZE,
                image_max_attempts: FALLBACK_MAX_ATTEMPTS,
            },
        )
    }));
    assert!(outcome.is_err(), "the post-commit cache clear panics");

    let conn = infra.pool.get().expect("connection");
    let docs = query::find(&conn, "media", &def, &query::FindQuery::default(), None).expect("find");
    assert_eq!(docs.len(), 1, "the row committed before the panic");

    let key = stored_key(&docs[0]);
    assert!(
        infra.storage.exists(&key).expect("exists"),
        "the committed row still references {key}"
    );
}

/// The counterpart: a write that stores its file and then rolls back
/// inside the transaction (a required field is missing) still takes the
/// bytes with it — the watch saw no commit.
#[test]
fn a_write_that_never_commits_releases_its_file() {
    let mut strict = media_with_drafts();
    strict.fields.push(
        FieldDefinition::builder("caption", FieldType::Text)
            .required(true)
            .build(),
    );
    let (tmp, infra, def) = infra_for(strict);
    let ctx = ServiceContext::collection("media", &def)
        .infra(&infra)
        .build();

    let err = create_upload(
        &ctx,
        CreateUploadInput {
            storage: &infra.storage,
            file: &file("first.txt", b"first"),
            form: empty_form(&def),
            locale_ctx: None,
            password: None,
            ui_locale: None,
            draft: false,
            upload_max_file_size: MAX_FILE_SIZE,
            image_max_attempts: FALLBACK_MAX_ATTEMPTS,
        },
    )
    .err()
    .expect("the missing required field fails the write");
    assert!(matches!(err, ServiceError::Validation(_)), "{err:?}");

    assert!(
        !has_file_under(&tmp.path().join("uploads")),
        "no file may survive a write that never committed"
    );
}

/// An update that carries no file changes no files.
#[test]
fn an_update_without_a_file_keeps_the_stored_one() {
    let (_tmp, infra, def) = infra();

    let published = create(&infra, &def, &file("first.txt", b"first"));
    let key = stored_key(&published);

    update(&infra, &def, &published.id, None, false);

    assert!(infra.storage.exists(&key).expect("exists"));
}
