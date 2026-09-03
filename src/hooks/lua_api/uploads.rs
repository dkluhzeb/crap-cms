//! `crap.uploads` namespace — signed serve-URL minting.

use mlua::{Error::RuntimeError, Lua, Result as LuaResult};

use crate::{
    core::upload::{key_from_served_url, signed_upload_url},
    typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Default TTL when the caller passes no `expires_in`.
const DEFAULT_EXPIRES_IN_SECS: i64 = 300;

/// Hard ceiling on `expires_in` — 30 days. Prevents accidentally minting
/// an effectively-unrevocable capability (revoking early means rotating
/// `[auth] secret`, which also invalidates every session). Relaxing the
/// cap later is additive; tightening it would break.
const MAX_EXPIRES_IN_SECS: i64 = 30 * 24 * 60 * 60;

/// Validate that `url` is a signable serve path: exactly
/// `/uploads/{collection}/{filename}` with non-empty segments and no
/// characters that cannot survive in a URL. Rejecting at mint time turns
/// "signed a URL that can never verify" into a loud error.
fn validate_signable_path(url: &str) -> Result<(), String> {
    let Some(key) = key_from_served_url(url) else {
        return Err(format!(
            "'{url}' is not an upload proxy path — pass a stored `/uploads/…` \
             value (doc.url or a size variant column)"
        ));
    };

    let mut segments = key.split('/');
    let (Some(slug), Some(file), None) = (segments.next(), segments.next(), segments.next()) else {
        return Err(format!(
            "'{url}' is not a `/uploads/{{collection}}/{{filename}}` path"
        ));
    };

    if slug.is_empty() || file.is_empty() {
        return Err(format!(
            "'{url}' is not a `/uploads/{{collection}}/{{filename}}` path"
        ));
    }

    if url
        .chars()
        .any(|c| c == '?' || c == '#' || c == ' ' || c.is_control())
    {
        return Err(format!(
            "'{url}' contains characters that cannot survive in a URL \
             (the file was stored before extension sanitization — re-upload it)"
        ));
    }

    Ok(())
}

/// Auth-secret-derived state for `crap.uploads.sign_url`.
pub(super) struct UploadSignState {
    secret: String,
}

/// Mint a time-boxed signed URL for a stored upload proxy path, so a browser
/// on another origin (or behind a CDN) can fetch a **private** file without a
/// session cookie or Bearer token. The returned URL is a capability: whoever
/// holds it can fetch the file until it expires — authorize first (typically
/// you sign values from a document that already passed the read pipeline,
/// e.g. inside an `after_read` hook) and keep TTLs short (capped at 30 days).
///
/// **Never sign a client-supplied path.** This function is a signing oracle:
/// whatever it signs becomes fetchable by anyone holding the URL. Passing
/// `ctx.query`/`ctx.body` values from a custom route hands out capabilities
/// for arbitrary private uploads.
#[lua_fn(
    path = "crap.uploads.sign_url",
    returns_doc = "The path with `exp` and `sig` query parameters appended."
)]
fn uploads_sign_url(
    state: &UploadSignState,
    _: &Lua,
    #[lua(
        doc = "A stored upload URL — the `/uploads/…` proxy path from `doc.url` or a `{size}_url` column."
    )]
    url: String,
    #[lua(doc = "Seconds until expiry (default 300). Must be positive.")] expires_in: Option<i64>,
) -> LuaResult<String> {
    validate_signable_path(&url)
        .map_err(|e| RuntimeError(format!("crap.uploads.sign_url: {e}")))?;

    let expires_in = expires_in.unwrap_or(DEFAULT_EXPIRES_IN_SECS);
    if expires_in <= 0 {
        return Err(RuntimeError(
            "crap.uploads.sign_url: expires_in must be positive".into(),
        ));
    }
    if expires_in > MAX_EXPIRES_IN_SECS {
        return Err(RuntimeError(format!(
            "crap.uploads.sign_url: expires_in exceeds the maximum of \
             {MAX_EXPIRES_IN_SECS} seconds (30 days)"
        )));
    }

    if state.secret.is_empty() {
        return Err(RuntimeError(
            "crap.uploads.sign_url requires a configured [auth] secret in crap.toml".into(),
        ));
    }

    let now = chrono::Utc::now().timestamp();

    signed_upload_url(&state.secret, &url, expires_in, now).ok_or_else(|| {
        RuntimeError("crap.uploads.sign_url: signing failed (expiry overflow)".into())
    })
}

lua_table! {
    name: crap_uploads,
    path: "crap.uploads",
    state: UploadSignState,
    header: "Upload helpers: signed serve-URL minting for cross-origin private media.",
    fns: [uploads_sign_url],
}

/// Register `crap.uploads` — signed serve-URL minting.
/// Parent `crap` table must already be in globals.
pub(super) fn register_uploads(lua: &Lua, auth_secret: &str) -> anyhow::Result<()> {
    register_crap_uploads(
        lua,
        UploadSignState {
            secret: auth_secret.to_string(),
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_with_uploads(secret: &str) -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_uploads(&lua, secret).unwrap();
        lua
    }

    #[test]
    fn sign_url_appends_exp_and_sig() {
        let lua = lua_with_uploads("s3cret");
        let url: String = lua
            .load(r#"return crap.uploads.sign_url("/uploads/posts/a_photo.jpg", 60)"#)
            .eval()
            .unwrap();

        assert!(url.starts_with("/uploads/posts/a_photo.jpg?exp="));
        assert!(url.contains("&sig="));
    }

    #[test]
    fn sign_url_defaults_ttl() {
        let lua = lua_with_uploads("s3cret");
        let url: String = lua
            .load(r#"return crap.uploads.sign_url("/uploads/posts/a.jpg")"#)
            .eval()
            .unwrap();
        assert!(url.contains("?exp="));
    }

    #[test]
    fn sign_url_rejects_non_proxy_path() {
        let lua = lua_with_uploads("s3cret");
        let err = lua
            .load(r#"return crap.uploads.sign_url("https://cdn.example.com/x.jpg", 60)"#)
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an upload proxy path"), "{err}");
    }

    #[test]
    fn sign_url_rejects_non_positive_ttl() {
        let lua = lua_with_uploads("s3cret");
        let err = lua
            .load(r#"return crap.uploads.sign_url("/uploads/p/a.jpg", 0)"#)
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be positive"), "{err}");
    }

    #[test]
    fn sign_url_rejects_dead_paths_at_mint() {
        let lua = lua_with_uploads("s3cret");

        for (input, expect) in [
            (r#""/uploads/""#, "filename"),
            (r#""/uploads/onlyslug""#, "filename"),
            (r#""/uploads/a/b/c.jpg""#, "filename"),
            (r#""/uploads/a/b.jpg?x=1""#, "cannot survive"),
            (r#""/uploads/a/b.pd\rf""#, "cannot survive"),
        ] {
            let err = lua
                .load(format!("return crap.uploads.sign_url({input}, 60)"))
                .eval::<String>()
                .unwrap_err()
                .to_string();
            assert!(err.contains(expect), "{input}: {err}");
        }
    }

    #[test]
    fn sign_url_caps_ttl() {
        let lua = lua_with_uploads("s3cret");
        let err = lua
            .load(r#"return crap.uploads.sign_url("/uploads/p/a.jpg", 99999999)"#)
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("30 days"), "{err}");
    }

    #[test]
    fn sign_url_requires_secret() {
        let lua = lua_with_uploads("");
        let err = lua
            .load(r#"return crap.uploads.sign_url("/uploads/p/a.jpg", 60)"#)
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("[auth] secret"), "{err}");
    }
}
