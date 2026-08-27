//! `ListCollections` handler — list all registered collections and globals.

use tonic::{Request, Response};

use crate::api::{content, handlers::ContentService};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// List all registered collections and globals.
    pub(in crate::api::handlers) fn list_collections_impl(
        &self,
        _request: Request<content::ListCollectionsRequest>,
    ) -> Response<content::ListCollectionsResponse> {
        let mut collections: Vec<content::CollectionInfo> = self
            .infra
            .registry
            .collections
            .values()
            .map(|def| content::CollectionInfo {
                slug: def.slug.to_string(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                timestamps: def.timestamps,
                auth: def.is_auth_collection(),
                upload: def.is_upload_collection(),
            })
            .collect();

        collections.sort_by(|a, b| a.slug.cmp(&b.slug));

        let mut globals: Vec<content::GlobalInfo> = self
            .infra
            .registry
            .globals
            .values()
            .map(|def| content::GlobalInfo {
                slug: def.slug.to_string(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
            })
            .collect();

        globals.sort_by(|a, b| a.slug.cmp(&b.slug));

        Response::new(content::ListCollectionsResponse {
            collections,
            globals,
        })
    }
}
