//! Collection-scoped auth callback handler — dispatches
//! `/admin/auth/callback/{collection}/{name}` to Lua hooks.
//!
//! The un-scoped [`super::callback::auth_callback`] can only bind a session when
//! there is exactly one auth collection. This scoped variant takes the target
//! auth collection from the URL, so a deployment with multiple auth collections
//! (e.g. `admins` + `customers`) registers a distinct OAuth redirect URI per
//! collection. The session still binds ONLY to the named collection — the
//! hook-returned user must exist in it — so this route cannot bind across
//! collections any more than the un-scoped one can.

use std::{collections::HashMap, net::SocketAddr};

use axum::{
    body::Bytes,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, Method},
    response::{IntoResponse, Redirect, Response},
};

use crate::admin::{
    AdminState,
    handlers::{
        auth::callback::{CallbackRequest, complete_auth_callback, form_fields},
        shared::paths,
    },
};

/// GET/POST `/admin/auth/callback/{collection}/{name}` — dispatch to the Lua
/// auth callback hook `hooks.auth_callback.{name}`, binding the resulting session
/// to the auth collection named in the URL.
///
/// `{collection}` must be a known auth collection; otherwise the request fails
/// closed (redirect to login). The hook contract is identical to the un-scoped
/// [`super::callback::auth_callback`].
pub async fn auth_callback_scoped(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path((collection, name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // The collection comes from the URL, so it must be a real auth collection.
    // (`admit_callback_user` re-checks the user is stored in it, but rejecting an
    // unknown/non-auth collection up front avoids running a hook for nothing.)
    let is_auth = state
        .infra
        .registry
        .get_collection(&collection)
        .is_some_and(|d| d.is_auth_collection());

    if !is_auth {
        return Redirect::to(paths::LOGIN).into_response();
    }

    let form = form_fields(&headers, &body);
    let request =
        CallbackRequest::builder(addr, &collection, &name, &method, &params, &form, &headers)
            .build();

    complete_auth_callback(&state, &request).await
}
