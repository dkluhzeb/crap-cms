//! DescribeCollection handler — describe a collection's or global's schema.

use tonic::{Request, Response, Status};

use crate::api::{
    content,
    handlers::{ContentService, convert::field_def_to_proto},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Describe a collection's schema (fields, timestamps, auth, upload).
    pub(in crate::api::handlers) async fn describe_collection_impl(
        &self,
        request: Request<content::DescribeCollectionRequest>,
    ) -> Result<Response<content::DescribeCollectionResponse>, Status> {
        let req = request.into_inner();

        if req.is_global {
            let def = self.get_global_def(&req.slug)?;

            return Ok(Response::new(content::DescribeCollectionResponse {
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
                timestamps: false,
                auth: false,
                fields: def.fields.iter().map(field_def_to_proto).collect(),
                upload: false,
                drafts: false,
            }));
        }

        let def = self.get_collection_def(&req.slug)?;

        Ok(Response::new(content::DescribeCollectionResponse {
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
            fields: def.fields.iter().map(field_def_to_proto).collect(),
            upload: def.is_upload_collection(),
            drafts: def.has_drafts(),
        }))
    }
}
