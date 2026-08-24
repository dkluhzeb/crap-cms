//! Shared breadcrumb base-chains for the admin collection/global pages, so the
//! fixed `Collections › <Collection>` and `Dashboard › <Global>` prefixes live
//! in one place instead of being re-spelled at every page handler.

use crate::admin::context::Breadcrumb;
use crate::admin::handlers::shared::paths;
use crate::core::collection::{CollectionDefinition, GlobalDefinition};

/// The collections-root → collection chain (`Collections › <Collection>`).
/// Handlers push their own tail crumb(s) onto the returned vec.
pub(crate) fn collection_base(def: &CollectionDefinition, slug: &str) -> Vec<Breadcrumb> {
    vec![
        Breadcrumb::link("collections", paths::COLLECTIONS_ROOT),
        Breadcrumb::link(def.display_name(), paths::collection(slug)),
    ]
}

/// The collection chain plus the item crumb
/// (`Collections › <Collection> › <title>`) — for pages under a single document.
pub(crate) fn collection_item_base(
    def: &CollectionDefinition,
    slug: &str,
    id: &str,
    title: impl Into<String>,
) -> Vec<Breadcrumb> {
    let mut chain = collection_base(def, slug);
    chain.push(Breadcrumb::link(title, paths::collection_item(slug, id)));
    chain
}

/// The dashboard → global chain (`Dashboard › <Global>`), with the global name
/// as a link — for pages *under* a global (e.g. its version history).
pub(crate) fn global_base(def: &GlobalDefinition, slug: &str) -> Vec<Breadcrumb> {
    vec![
        Breadcrumb::link("dashboard", paths::DASHBOARD),
        Breadcrumb::link(def.display_name(), paths::global(slug)),
    ]
}
