//! Per-collection checks: collection settings, auth-collection users, and
//! auth-method shape.

use std::collections::HashMap;

use crate::{
    core::{
        CollectionDefinition, Registry,
        collection::{Activation, AuthMethod, Surface},
    },
    db::{DbConnection, query},
};

use super::Finding;

pub(super) fn check_collections(
    reg: &Registry,
    conn: &dyn DbConnection,
    findings: &mut Vec<Finding>,
) {
    for (slug, def) in &reg.collections {
        // Auth collection without soft delete means accounts are permanently deleted
        if def.is_auth_collection() && !def.soft_delete {
            findings.push(
                Finding::new(format!(
                    "Auth collection '{slug}' has soft_delete disabled — deleted accounts are permanent"
                ))
                .with_hint("Consider enabling soft_delete for auth collections to support account recovery."),
            );
        }

        // Upload collection without versions means no rollback on file changes
        if def.is_upload_collection() && !def.has_versions() {
            findings.push(Finding::new(format!(
                "Upload collection '{slug}' has no versioning — overwritten files cannot be recovered"
            )));
        }

        // Soft delete without retention = trash grows unbounded
        if def.soft_delete && def.soft_delete_retention.is_none() {
            findings.push(
                Finding::new(format!(
                    "Collection '{slug}' has soft_delete enabled but no retention policy — trash grows unbounded"
                ))
                .with_hint("Set `soft_delete_retention = \"30d\"` to auto-purge old trash."),
            );
        }

        check_auth_users(slug, def, conn, findings);
    }
}

/// An auth collection with no users can't be logged into. A count that fails
/// is its own finding — never read as "no users", which would send the
/// operator off to create an account that may already exist.
fn check_auth_users(
    slug: &str,
    def: &CollectionDefinition,
    conn: &dyn DbConnection,
    findings: &mut Vec<Finding>,
) {
    if !def.is_auth_collection() {
        return;
    }

    match query::count(conn, slug, def, &[], None) {
        Ok(0) => findings.push(
            Finding::new(format!(
                "Auth collection '{slug}' has no users — nobody can log in through it"
            ))
            .with_hint(format!(
                "Run `crap-cms user create -c {slug}` to create an admin user."
            )),
        ),
        Ok(_) => {}
        Err(e) => findings.push(Finding::new(format!(
            "Could not count the users of auth collection '{slug}': {e:#}"
        ))),
    }
}

/// Surface auth-method shape smells the startup validator would let
/// through silently. Hard errors (enabled + empty methods, duplicate
/// `password_login` / bearer) are already rejected at startup —
/// `status` only flags configurations that *work* but probably
/// aren't what the operator intended.
pub(super) fn check_auth_methods(reg: &Registry, findings: &mut Vec<Finding>) {
    // Tracks every `Always`-activated strategy, keyed by the surface
    // it covers. Used to flag the cross-collection race where two
    // such strategies compete on the same surface.
    let mut always_strategies_by_surface: HashMap<Surface, Vec<String>> = HashMap::new();
    // Tracks every `Header`-activated strategy by `(lowercase header,
    // surface)`. Two strategies sharing both fields race for the
    // request — HashMap iteration order picks the winner.
    let mut header_strategies_by_key: HashMap<(String, Surface), Vec<String>> = HashMap::new();

    for (slug, def) in &reg.collections {
        let Some(auth) = def.auth.as_ref() else {
            continue;
        };
        if !auth.enabled {
            continue;
        }

        let has_password = auth.password_login_enabled();
        let has_bearer_anywhere =
            auth.accepts_bearer(Surface::Admin) || auth.accepts_bearer(Surface::Grpc);

        // Password-login without bearer: the Login RPC will issue a JWT
        // that no subsequent request can use (every surface's bearer
        // check fails). Almost always a config mistake.
        if has_password && !has_bearer_anywhere {
            findings.push(
                Finding::new(format!(
                    "Collection '{slug}' has password_login but no bearer method — \
                     Login will issue a JWT that no future request can authenticate",
                ))
                .with_hint(
                    "Add `{ type = \"bearer\", surfaces = { \"grpc\", \"admin\" } }` \
                     to the methods list, or use crap.auth.with_defaults({...}).",
                ),
            );
        }

        for method in &auth.methods {
            if let AuthMethod::Strategy {
                name,
                activates_on,
                surfaces,
                ..
            } = method
            {
                let owner = format!("{slug}.{name}");
                match activates_on {
                    Activation::Always { .. } => {
                        for surface in surfaces {
                            always_strategies_by_surface
                                .entry(*surface)
                                .or_default()
                                .push(owner.clone());
                        }
                    }
                    Activation::Header { header } => {
                        let key = header.to_ascii_lowercase();
                        for surface in surfaces {
                            header_strategies_by_key
                                .entry((key.clone(), *surface))
                                .or_default()
                                .push(owner.clone());
                        }
                    }
                }
            }
        }
    }

    // Multiple Always-active strategies on the same surface: ordering
    // of `registry.collections` determines which one wins. Operators
    // rarely want this; prefer a header discriminator to bind each
    // strategy to its own request signal.
    for (surface, owners) in &always_strategies_by_surface {
        if owners.len() > 1 {
            findings.push(
                Finding::new(format!(
                    "Multiple always-active auth strategies on surface {surface:?}: {} — \
                     request authentication depends on registration order",
                    owners.join(", ")
                ))
                .with_hint(
                    "Bind each strategy to its own request signal via \
                     `activates_on = { header = \"x-...\" }`.",
                ),
            );
        }
    }

    // Multiple header-activated strategies bound to the *same* header
    // on the *same* surface: identical situation as the always case
    // — HashMap iteration order picks the winner non-deterministically.
    // Detected separately because a header discriminator that's used
    // by exactly one strategy is fine; the collision is the issue.
    for ((header, surface), owners) in &header_strategies_by_key {
        if owners.len() > 1 {
            findings.push(
                Finding::new(format!(
                    "Multiple auth strategies bound to header '{header}' on surface \
                     {surface:?}: {} — request authentication depends on registration order",
                    owners.join(", ")
                ))
                .with_hint(
                    "Use distinct header names per strategy, or scope each strategy \
                     to a different `surfaces` list.",
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        HookRef,
        collection::{Auth, SurfaceSet},
    };
    #[cfg(feature = "sqlite")]
    use crate::db::InMemoryConn;

    fn registry_with(defs: Vec<CollectionDefinition>) -> Registry {
        let mut reg = Registry::new();
        for def in defs {
            reg.register_collection(def);
        }
        reg
    }

    fn auth_def_with(slug: &str, methods: Vec<AuthMethod>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.auth = Some(Auth {
            enabled: true,
            methods,
            ..Default::default()
        });
        def
    }

    /// Test helper: an `Always`-active strategy on the admin surface.
    fn strategy_always(name: &str, authenticate: &str) -> AuthMethod {
        AuthMethod::Strategy {
            name: name.to_string(),
            authenticate: HookRef::new(authenticate),
            activates_on: Activation::always(),
            surfaces: SurfaceSet::admin_only(),
        }
    }

    /// Test helper: a header-discriminated strategy on the given surfaces.
    fn strategy_on_header(
        name: &str,
        authenticate: &str,
        header: &str,
        surfaces: SurfaceSet,
    ) -> AuthMethod {
        AuthMethod::Strategy {
            name: name.to_string(),
            authenticate: HookRef::new(authenticate),
            activates_on: Activation::Header {
                header: header.to_string(),
            },
            surfaces,
        }
    }

    #[test]
    fn check_auth_methods_clean_default_methods_emits_nothing() {
        let reg = registry_with(vec![auth_def_with("users", Auth::default_methods())]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings.is_empty(),
            "default methods should be clean: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_password_without_bearer_warns() {
        let reg = registry_with(vec![auth_def_with(
            "users",
            vec![AuthMethod::password_login()],
        )]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert_eq!(findings.len(), 1, "expected exactly one finding");
        assert!(
            findings[0].message.contains("password_login but no bearer"),
            "wrong finding: {}",
            findings[0].message
        );
    }

    #[test]
    fn check_auth_methods_disabled_auth_skipped() {
        // enabled = false → not surveyed
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth {
            enabled: false,
            methods: vec![AuthMethod::password_login()],
            ..Default::default()
        });
        let reg = registry_with(vec![def]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn check_auth_methods_multiple_always_strategies_on_same_surface_warns() {
        let mtls_strategy = AuthMethod::Strategy {
            name: "mtls".to_string(),
            authenticate: HookRef::new("hooks.auth.mtls"),
            activates_on: Activation::always(),
            surfaces: SurfaceSet::admin_only(),
        };
        let proxy_strategy = AuthMethod::Strategy {
            name: "proxy".to_string(),
            authenticate: HookRef::new("hooks.auth.proxy"),
            activates_on: Activation::always(),
            surfaces: SurfaceSet::admin_only(),
        };
        let reg = registry_with(vec![
            auth_def_with("users", vec![AuthMethod::bearer(), mtls_strategy]),
            auth_def_with("admins", vec![AuthMethod::bearer(), proxy_strategy]),
        ]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("Multiple always-active")),
            "expected always-collision warning, got: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_header_activated_strategy_does_not_warn() {
        let api_key = strategy_on_header(
            "api-key",
            "hooks.auth.api_key",
            "x-api-key",
            SurfaceSet::grpc_only(),
        );
        let reg = registry_with(vec![auth_def_with(
            "users",
            vec![AuthMethod::password_login(), AuthMethod::bearer(), api_key],
        )]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(findings.is_empty(), "header-discriminated strategy is fine");
    }

    #[test]
    fn check_auth_methods_header_overlap_across_collections_warns() {
        // Two different collections both register a strategy bound
        // to the same header on the same surface. Whichever fires
        // first depends on HashMap iteration order — almost always
        // a config mistake.
        let mk_strategy = |name: &str| {
            strategy_on_header(
                name,
                "hooks.auth.api_key",
                "x-api-key",
                SurfaceSet::grpc_only(),
            )
        };
        let reg = registry_with(vec![
            auth_def_with(
                "users",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    mk_strategy("api-key"),
                ],
            ),
            auth_def_with(
                "service_accounts",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    mk_strategy("svc-key"),
                ],
            ),
        ]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("strategies bound to header 'x-api-key'")),
            "expected header-overlap warning, got: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_header_case_difference_still_collides() {
        // `X-API-KEY` and `x-api-key` are the same HTTP header.
        // Activation matching lowercases for comparison; the status
        // check must do the same to catch operator-typo collisions.
        let reg = registry_with(vec![
            auth_def_with(
                "users",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    strategy_on_header(
                        "lower",
                        "hooks.auth.lower",
                        "x-api-key",
                        SurfaceSet::grpc_only(),
                    ),
                ],
            ),
            auth_def_with(
                "service_accounts",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    strategy_on_header(
                        "upper",
                        "hooks.auth.upper",
                        "X-API-KEY",
                        SurfaceSet::grpc_only(),
                    ),
                ],
            ),
        ]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("strategies bound to header 'x-api-key'")),
            "case-different header names should still collide: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_distinct_headers_do_not_collide() {
        // Two strategies, each bound to its own header → no warning.
        let reg = registry_with(vec![auth_def_with(
            "users",
            vec![
                AuthMethod::password_login(),
                AuthMethod::bearer(),
                strategy_on_header(
                    "api-key",
                    "hooks.auth.api_key",
                    "x-api-key",
                    SurfaceSet::grpc_only(),
                ),
                strategy_on_header(
                    "sso",
                    "hooks.auth.sso",
                    "x-sso-assertion",
                    SurfaceSet::admin_only(),
                ),
            ],
        )]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings.is_empty(),
            "distinct headers should not collide: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_same_header_different_surfaces_does_not_collide() {
        // Same header name but the strategies are scoped to different
        // surfaces — they never race because they fire on different
        // request paths.
        let reg = registry_with(vec![
            auth_def_with(
                "users",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    strategy_on_header(
                        "admin-key",
                        "hooks.auth.admin",
                        "x-api-key",
                        SurfaceSet::admin_only(),
                    ),
                ],
            ),
            auth_def_with(
                "service_accounts",
                vec![
                    AuthMethod::password_login(),
                    AuthMethod::bearer(),
                    strategy_on_header(
                        "grpc-key",
                        "hooks.auth.grpc",
                        "x-api-key",
                        SurfaceSet::grpc_only(),
                    ),
                ],
            ),
        ]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(
            findings.is_empty(),
            "same header on different surfaces should not collide: {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_auth_methods_single_always_strategy_does_not_warn() {
        // Only the multi-collection collision is a warning; one
        // always-active strategy on its own is the operator's call
        // (the startup validator already logs an info-level warning).
        let reg = registry_with(vec![auth_def_with(
            "users",
            vec![
                AuthMethod::bearer(),
                strategy_always("mtls", "hooks.auth.mtls"),
            ],
        )]);
        let mut findings = Vec::new();
        check_auth_methods(&reg, &mut findings);
        assert!(findings.is_empty());
    }

    /// Regression: the no-users warning also required the collection to have
    /// hooks, so a fresh project — an auth collection with none — never got it.
    #[cfg(feature = "sqlite")]
    #[test]
    fn an_empty_auth_collection_without_hooks_is_reported() {
        let conn = InMemoryConn::open();
        conn.execute("CREATE TABLE users (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        let def = auth_def_with("users", Auth::default_methods());

        let mut findings = Vec::new();
        check_auth_users("users", &def, &conn, &mut findings);

        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].message.contains("has no users"),
            "{}",
            findings[0].message
        );
    }

    /// A count that fails must not read as "no users".
    #[cfg(feature = "sqlite")]
    #[test]
    fn a_failed_user_count_is_reported_as_an_error_not_as_no_users() {
        let conn = InMemoryConn::open();
        let def = auth_def_with("users", Auth::default_methods());

        let mut findings = Vec::new();
        check_auth_users("users", &def, &conn, &mut findings);

        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].message.contains("Could not count"),
            "{}",
            findings[0].message
        );
        assert!(!findings[0].message.contains("no users"));
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn a_populated_or_non_auth_collection_is_not_reported() {
        let conn = InMemoryConn::open();
        conn.execute("CREATE TABLE users (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute("INSERT INTO users (id) VALUES ('u1')", &[])
            .unwrap();

        let users = auth_def_with("users", Auth::default_methods());
        let posts = CollectionDefinition::new("posts");

        let mut findings = Vec::new();
        check_auth_users("users", &users, &conn, &mut findings);
        check_auth_users("posts", &posts, &conn, &mut findings);

        assert!(findings.is_empty());
    }
}
