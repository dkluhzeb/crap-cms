//! Validate handler — check field data against collection rules without
//! persisting.
//!
//! Codec over [`op::run_blocking`]: the dry-run pipeline (field-access
//! stripping as the resolved user, field hooks, validators, unique checks)
//! lives in the shared [`Validate`] operation body.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::{
    api::{
        content::{self, DataMap},
        handlers::{
            ContentService, collection::helpers::extract_auth_password, proto::data_map_to_json_map,
        },
    },
    core::{DocumentFields, collection::Surface},
    db::LocaleContext,
    service::op::{self, Credentials, Principal, TargetRef, Validate, ValidateArgs},
};

/// The field data a validate request previews: the wire map, minus an auth
/// collection's `password`. A password is a credential, not field data — the
/// create and update requests split it off the same way — so the dry-run's
/// `before_validate` hooks never see the plaintext.
fn validate_data(data: Option<&DataMap>, is_auth: bool) -> Result<DocumentFields, Status> {
    let mut data: DocumentFields = data
        .map(data_map_to_json_map)
        .transpose()
        .map_err(Status::invalid_argument)?
        .unwrap_or_default()
        .into();

    extract_auth_password(&mut data, is_auth, true)?;

    Ok(data)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Validate document data without persisting — returns per-field errors.
    pub(in crate::api::handlers) async fn validate_impl(
        &self,
        request: Request<content::ValidateRequest>,
    ) -> Result<Response<content::ValidateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let data = validate_data(req.data.as_ref(), def.is_auth_collection())?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let args = ValidateArgs::builder(data)
            .locale_ctx(locale_ctx)
            .exclude_id(req.id.clone())
            .draft(req.draft.unwrap_or(false))
            .build();

        let principal = Principal::Credentials(Credentials {
            surface: Surface::Grpc,
            bearer: token,
            session_cookie: None,
            headers,
        });

        let outcome = op::run_blocking::<Validate>(
            Arc::clone(&self.infra),
            principal,
            TargetRef::collection(req.collection),
            args,
        )
        .await
        .map_err(|e| self.core_error_status(e))?;

        let (valid, errors) = match outcome {
            None => (true, std::collections::HashMap::new()),
            Some(ve) => (false, ve.to_field_map()),
        };

        Ok(Response::new(content::ValidateResponse { valid, errors }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::api::handlers::proto::json_to_field_value;

    fn data_map(value: &serde_json::Value) -> DataMap {
        let fields: HashMap<_, _> = value
            .as_object()
            .expect("object")
            .iter()
            .map(|(k, v)| (k.clone(), json_to_field_value(v)))
            .collect();

        DataMap { fields }
    }

    /// Regression: a validate request on an auth collection kept `password` in
    /// the field data, so the dry-run's `before_validate` hooks saw the
    /// plaintext — the create path and the MCP / Lua validate split it off.
    #[test]
    fn validate_strips_an_auth_collections_password() {
        let wire = data_map(&json!({ "email": "a@b.c", "password": "pw" }));
        let data = validate_data(Some(&wire), true).expect("valid wire data");

        assert!(!data.contains_key("password"), "{data:?}");
        assert_eq!(data.get("email"), Some(&json!("a@b.c")));
    }

    /// A plain collection's `password` is ordinary field data and stays.
    #[test]
    fn validate_keeps_a_plain_collections_password_field() {
        let wire = data_map(&json!({ "password": "x" }));
        let data = validate_data(Some(&wire), false).expect("valid wire data");

        assert_eq!(data.get("password"), Some(&json!("x")));
    }
}
