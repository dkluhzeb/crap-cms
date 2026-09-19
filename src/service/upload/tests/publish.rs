//! Publishing a draft makes its file live and settles its conversions —
//! from a single update, a bulk update and a translation alike.

use super::support::*;
use crate::service::upload::*;
use crate::{
    config::LocaleConfig,
    db::{LocaleContext, LocaleMode},
};

/// A draft save records its file in the snapshot and leaves everything
/// live alone: the published row keeps its url, and the drafted file's
/// deferred conversion is NOT queued — a job would write its derivative url
/// onto the published row, which does not reference that file yet.
#[test]
fn a_draft_save_records_its_file_without_publishing_or_queueing_it() {
    let (_tmp, infra, def) = infra_for(media_with_queued_webp());

    let published = create(&infra, &def, &image_file("first.png", 40));
    let published_url = live_url(&infra, &def, &published.id);

    let drafted = update(
        &infra,
        &def,
        &published.id,
        Some(image_file("second.png", 40)),
        true,
    );

    assert_eq!(
        live_url(&infra, &def, &published.id),
        published_url,
        "a draft save must not move the published row's file"
    );
    assert_eq!(
        latest_snapshot(&infra, &published.id)["url"].as_str(),
        drafted.get_str("url"),
        "the draft snapshot carries the new file"
    );

    let payloads = queued_conversion_payloads(&infra);
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    assert!(
        payloads[0].contains(&thumbnail_source(&published)),
        "only the published file's conversion is queued: {payloads:?}"
    );
}

/// Publishing the draft makes its file live: the published row takes the
/// drafted url, the previous file's still-queued conversion is cancelled
/// and the drafted file's own is queued in its place. The previous file's
/// bytes stay — the version created on upload still references them.
#[test]
fn publishing_a_draft_makes_its_file_live_and_queues_its_conversions() {
    let (_tmp, infra, def) = infra_for(media_with_queued_webp());

    let published = create(&infra, &def, &image_file("first.png", 40));
    let published_key = stored_key(&published);

    let drafted = update(
        &infra,
        &def,
        &published.id,
        Some(image_file("second.png", 40)),
        true,
    );
    let drafted_url = drafted.get_str("url").expect("a url").to_string();

    update(&infra, &def, &published.id, None, false);

    assert_eq!(
        live_url(&infra, &def, &published.id),
        drafted_url,
        "the publish must carry the drafted file over to the row"
    );

    let payloads = queued_conversion_payloads(&infra);
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    assert!(
        payloads[0].contains(&thumbnail_source(&drafted)),
        "the drafted file's conversion is queued: {payloads:?}"
    );
    assert!(
        !payloads[0].contains(&thumbnail_source(&published)),
        "the previous file's conversion is cancelled: {payloads:?}"
    );
    assert!(
        infra.storage.exists(&published_key).expect("exists"),
        "a version snapshot still references {published_key}"
    );
}

/// A publish that carries a file of its own publishes THAT file: the
/// request's upload wins over the pending draft's.
#[test]
fn a_file_in_the_publishing_request_wins_over_the_drafted_one() {
    let (_tmp, infra, def) = infra_for(media_with_queued_webp());

    let published = create(&infra, &def, &image_file("first.png", 40));

    let drafted = update(
        &infra,
        &def,
        &published.id,
        Some(image_file("second.png", 40)),
        true,
    );

    let republished = update(
        &infra,
        &def,
        &published.id,
        Some(image_file("third.png", 40)),
        false,
    );

    assert_eq!(
        live_url(&infra, &def, &published.id),
        republished.get_str("url").expect("a url"),
        "the file the request carried must win"
    );

    let payloads = queued_conversion_payloads(&infra);
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    assert!(
        payloads[0].contains(&thumbnail_source(&republished)),
        "only the request's own file owes a conversion: {payloads:?}"
    );
    assert!(
        !payloads[0].contains(&thumbnail_source(&drafted)),
        "the drafted file was not published: {payloads:?}"
    );
}

/// The rule lives in the service write, not in the multipart upload
/// surface: a publish issued through the plain update operation (gRPC, Lua,
/// MCP) adopts the drafted file exactly the same way.
#[test]
fn a_plain_update_publish_adopts_the_drafted_file() {
    let (_tmp, infra, def) = infra_for(media_with_queued_webp());

    let published = create(&infra, &def, &image_file("first.png", 40));
    let drafted = update(
        &infra,
        &def,
        &published.id,
        Some(image_file("second.png", 40)),
        true,
    );
    let drafted_url = drafted.get_str("url").expect("a url").to_string();

    let ctx = ServiceContext::collection("media", &def)
        .infra(&infra)
        .build();

    update_document(
        &ctx,
        &published.id,
        WriteInput::builder(DocumentFields::new()).build(),
    )
    .expect("publish");

    assert_eq!(live_url(&infra, &def, &published.id), drafted_url);

    let payloads = queued_conversion_payloads(&infra);
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    assert!(
        payloads[0].contains(&thumbnail_source(&drafted)),
        "{payloads:?}"
    );
}

/// A publish that follows an edit which changed no file leaves the queued
/// conversion alone: re-queueing would cancel the job for the very file the
/// row keeps referencing.
#[test]
fn publishing_a_draft_that_changed_no_file_keeps_its_pending_conversion() {
    let (_tmp, infra, def) = infra_for(media_with_queued_webp());

    let published = create(&infra, &def, &image_file("first.png", 40));

    update(&infra, &def, &published.id, None, true);
    update(&infra, &def, &published.id, None, false);

    let payloads = queued_conversion_payloads(&infra);
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    assert!(
        payloads[0].contains(&thumbnail_source(&published)),
        "the published file's conversion is untouched: {payloads:?}"
    );
}

/// Regression: a bulk publish adopted the drafted file but settled nothing,
/// so the drafted file's conversions were never queued, the previous file's
/// were never cancelled, and the previous file was never released — and no
/// later write could still see it, so those bytes stayed forever.
#[test]
fn a_bulk_publish_settles_the_drafted_file() {
    let (_tmp, infra, def) = infra_for(media_with_queued_webp_capped());

    let published = create(&infra, &def, &image_file("first.png", 40));
    let published_key = stored_key(&published);

    let drafted = update(
        &infra,
        &def,
        &published.id,
        Some(image_file("second.png", 40)),
        true,
    );
    let drafted_url = drafted.get_str("url").expect("a url").to_string();

    bulk_publish(&infra, &def);

    assert_eq!(
        live_url(&infra, &def, &published.id),
        drafted_url,
        "the bulk publish carries the drafted file over to the row"
    );

    let payloads = queued_conversion_payloads(&infra);
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    assert!(
        payloads[0].contains(&thumbnail_source(&drafted)),
        "the drafted file's conversion is queued: {payloads:?}"
    );
    assert!(
        !payloads[0].contains(&thumbnail_source(&published)),
        "the previous file's conversion is cancelled: {payloads:?}"
    );
    assert!(
        !infra.storage.exists(&published_key).expect("exists"),
        "nothing references {published_key} once the cap pruned its snapshots"
    );
}

/// A publish issued in a non-default locale makes the drafted file live
/// through the snapshot write-back. It used to skip the file adoption
/// entirely, so the row pointed at the drafted file while its conversions
/// were never queued and the previous file's never cancelled — a queued
/// job for the released file later wrote a derivative of deleted bytes.
#[test]
fn a_translation_publish_settles_the_drafted_file() {
    let (_tmp, infra, def) = infra_for(media_with_queued_webp_capped());
    let locales = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    let de = LocaleContext {
        mode: LocaleMode::Single("de".to_string()),
        config: locales,
    };

    let published = create(&infra, &def, &image_file("first.png", 40));
    let published_key = stored_key(&published);
    let drafted = update(
        &infra,
        &def,
        &published.id,
        Some(image_file("second.png", 40)),
        true,
    );
    let drafted_url = drafted.get_str("url").expect("a url").to_string();

    update_in_locale(&infra, &def, &published.id, None, false, Some(&de));

    assert_eq!(
        live_url(&infra, &def, &published.id),
        drafted_url,
        "the German publish carries the drafted file over to the row"
    );

    let payloads = queued_conversion_payloads(&infra);
    assert_eq!(payloads.len(), 1, "{payloads:?}");
    assert!(
        payloads[0].contains(&thumbnail_source(&drafted)),
        "the drafted file's conversion is queued: {payloads:?}"
    );
    assert!(
        !payloads[0].contains(&thumbnail_source(&published)),
        "the previous file's conversion is cancelled: {payloads:?}"
    );
    assert!(
        !infra.storage.exists(&published_key).expect("exists"),
        "nothing references {published_key} once the cap pruned its snapshots"
    );
}
