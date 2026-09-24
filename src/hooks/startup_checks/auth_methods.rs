//! Boot validation of per-collection `auth.methods` shape.

use std::collections::HashMap;

use anyhow::{Result, bail};
use tracing::warn;

use crate::core::{
    Registry, Slug,
    collection::{Activation, AuthMethod, MfaMode, Surface, SurfaceSet},
};

/// Validate per-collection `auth.methods` configurations.
///
/// Hard errors (boot fails):
/// - `enabled = true` with no methods listed.
/// - Duplicate `password_login` or `bearer` on one collection.
/// - Any method with an empty `surfaces` set — would silently
///   never fire on any request, almost certainly a config mistake.
/// - Strategy with `activates_on = { header = "" }` — would
///   silently never match (no HTTP header has an empty name).
/// - Strategy with empty `authenticate` — no Lua hook to invoke.
/// - `mfa = "custom"` without `mfa_deliver` (or the reverse), and
///   `mfa_exempt_callbacks` without an MFA mode.
///
/// Soft warnings (logged, boot continues):
/// - `Always`-activated strategies (potential footgun — fires on
///   every request that reaches the surface).
/// - Multiple `Always` strategies sharing a surface (request-
///   authentication outcome depends on registration order).
///
/// # Errors
///
/// Returns an aggregated error listing every shape problem found.
pub fn validate_auth_methods(registry: &Registry) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();
    let mut always_by_surface: HashMap<Surface, Vec<(String, String)>> = HashMap::new();

    for (slug, def) in &registry.collections {
        let Some(auth) = def.auth.as_ref() else {
            continue;
        };
        if !auth.enabled {
            continue;
        }
        if auth.methods.is_empty() {
            errors.push(format!(
                "collection '{slug}': auth.enabled is true but auth.methods is empty. \
                 Use crap.auth.default_methods() for the standard set."
            ));
            continue;
        }
        check_one_collection_methods(slug, &auth.methods, &mut errors, &mut always_by_surface);
    }

    warn_on_always_cross_collection_collisions(&always_by_surface);

    if errors.is_empty() {
        return Ok(());
    }
    bail!(
        "Auth method configuration errors:\n  - {}",
        errors.join("\n  - ")
    );
}

/// Walk a single collection's methods, collecting per-method shape
/// errors and tracking `Always`-active strategies for the cross-
/// collection collision warning emitted by the caller.
fn check_one_collection_methods(
    slug: &Slug,
    methods: &[AuthMethod],
    errors: &mut Vec<String>,
    always_by_surface: &mut HashMap<Surface, Vec<(String, String)>>,
) {
    let mut password_count = 0;
    let mut bearer_count = 0;
    for m in methods {
        if let Some(s) = method_surfaces(m)
            && s.is_empty()
        {
            errors.push(format!(
                "collection '{slug}': method has empty `surfaces` list — \
                 it can never fire. Drop the method or list at least one surface."
            ));
        }
        match m {
            AuthMethod::PasswordLogin {
                mfa,
                mfa_deliver,
                mfa_exempt_callbacks,
                ..
            } => {
                password_count += 1;
                check_mfa_pairing(slug, *mfa, mfa_deliver.is_some(), errors);
                check_mfa_exemptions(slug, *mfa, mfa_exempt_callbacks, errors);
            }
            AuthMethod::Bearer { .. } => bearer_count += 1,
            AuthMethod::Strategy {
                name,
                authenticate,
                activates_on,
                surfaces,
            } => check_strategy_shape(
                slug,
                name,
                authenticate.reference(),
                activates_on,
                surfaces,
                errors,
                always_by_surface,
            ),
            AuthMethod::SessionCookie { .. } => {}
        }
    }
    if password_count > 1 {
        errors.push(format!(
            "collection '{slug}': multiple password_login methods declared (one is enough)."
        ));
    }
    if bearer_count > 1 {
        errors.push(format!(
            "collection '{slug}': multiple bearer methods declared (one is enough)."
        ));
    }
}

/// `custom` MFA and its delivery hook come as a PAIR — a custom mode with no
/// hook strands every login (no code delivered), a hook without the mode is
/// silently dead config.
fn check_mfa_pairing(slug: &Slug, mfa: MfaMode, has_deliver: bool, errors: &mut Vec<String>) {
    if mfa == MfaMode::Custom && !has_deliver {
        errors.push(format!(
            "Collection '{slug}': mfa = \"custom\" requires an mfa_deliver hook"
        ));
    }

    if mfa != MfaMode::Custom && has_deliver {
        errors.push(format!(
            "Collection '{slug}': mfa_deliver is only valid with mfa = \"custom\" (got mfa = \"{}\")",
            match mfa {
                MfaMode::Email => "email",
                MfaMode::Off => "false",
                MfaMode::Custom => unreachable!(),
                MfaMode::Totp => "totp",
            }
        ));
    }
}

/// `mfa_exempt_callbacks` only relaxes an MFA step that exists — without an
/// MFA mode it is dead config that suggests a second factor the collection
/// never asks for.
fn check_mfa_exemptions(slug: &Slug, mfa: MfaMode, exempt: &[String], errors: &mut Vec<String>) {
    if mfa == MfaMode::Off && !exempt.is_empty() {
        errors.push(format!(
            "Collection '{slug}': mfa_exempt_callbacks is only valid with an mfa mode set"
        ));
    }
}

fn method_surfaces(m: &AuthMethod) -> Option<&SurfaceSet> {
    match m {
        AuthMethod::PasswordLogin { .. } => None,
        AuthMethod::Bearer { surfaces }
        | AuthMethod::SessionCookie { surfaces }
        | AuthMethod::Strategy { surfaces, .. } => Some(surfaces),
    }
}

fn check_strategy_shape(
    slug: &Slug,
    name: &str,
    authenticate: &str,
    activates_on: &Activation,
    surfaces: &SurfaceSet,
    errors: &mut Vec<String>,
    always_by_surface: &mut HashMap<Surface, Vec<(String, String)>>,
) {
    if authenticate.trim().is_empty() {
        errors.push(format!(
            "collection '{slug}': strategy '{name}' has empty `authenticate` \
             — no Lua hook to invoke."
        ));
    }
    if let Activation::Header { header } = activates_on
        && header.trim().is_empty()
    {
        errors.push(format!(
            "collection '{slug}': strategy '{name}' has empty \
             `activates_on.header` — no HTTP header has an empty \
             name, so the strategy could never fire."
        ));
    }
    if matches!(activates_on, Activation::Always { .. }) {
        warn!(
            "collection '{slug}': strategy '{name}' is always-active on every request. \
             Consider a header discriminator for safer scoping."
        );
        for surface in surfaces {
            always_by_surface
                .entry(*surface)
                .or_default()
                .push((slug.to_string(), name.to_string()));
        }
    }
}

fn warn_on_always_cross_collection_collisions(
    always_by_surface: &HashMap<Surface, Vec<(String, String)>>,
) {
    for (surface, owners) in always_by_surface {
        if owners.len() > 1 {
            let list = owners
                .iter()
                .map(|(s, n)| format!("'{s}'.'{n}'"))
                .collect::<Vec<_>>()
                .join(", ");
            warn!(
                "multiple always-active strategies on surface {surface:?}: {list}. \
                 Request authentication may depend on registration order — prefer \
                 header discriminators."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::{CollectionDefinition, HookRef, collection::Auth};

    use super::*;

    fn auth_def(slug: &str, methods: Vec<AuthMethod>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.auth = Some(Auth {
            enabled: true,
            methods,
            ..Default::default()
        });
        def
    }

    /// Register one collection and return the aggregated error message.
    fn auth_error(def: CollectionDefinition) -> String {
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(def);

        let err = validate_auth_methods(&registry.read().unwrap()).unwrap_err();
        format!("{err:#}")
    }

    /// The `mfa = "custom"` mode and its `mfa_deliver` hook come as a pair —
    /// both halves of the pairing are startup errors on their own.
    #[test]
    fn validate_auth_methods_enforces_custom_mfa_deliver_pairing() {
        // custom without deliver → error
        let msg = auth_error(auth_def(
            "users",
            vec![
                AuthMethod::password_login_builder()
                    .mfa(MfaMode::Custom)
                    .build(),
                AuthMethod::bearer(),
            ],
        ));
        assert!(msg.contains("requires an mfa_deliver hook"), "{msg}");

        // deliver without custom → error
        let msg = auth_error(auth_def(
            "users",
            vec![
                AuthMethod::password_login_builder()
                    .mfa(MfaMode::Email)
                    .mfa_deliver(Some(HookRef::new("hooks.mfa.send")))
                    .build(),
                AuthMethod::bearer(),
            ],
        ));
        assert!(msg.contains("only valid with mfa = \"custom\""), "{msg}");

        // the valid pair passes
        let registry = Registry::shared();
        registry.write().unwrap().register_collection(auth_def(
            "users",
            vec![
                AuthMethod::password_login_builder()
                    .mfa(MfaMode::Custom)
                    .mfa_deliver(Some(HookRef::new("hooks.mfa.send")))
                    .build(),
                AuthMethod::bearer(),
            ],
        ));
        validate_auth_methods(&registry.read().unwrap()).expect("valid pairing passes");
    }

    /// MFA-exempt callbacks without an MFA mode are a startup error.
    #[test]
    fn validate_auth_methods_rejects_exemptions_without_mfa() {
        let exempt = || vec!["okta".to_string()];

        let msg = auth_error(auth_def(
            "users",
            vec![
                AuthMethod::password_login_builder()
                    .mfa_exempt_callbacks(exempt())
                    .build(),
            ],
        ));
        assert!(msg.contains("mfa_exempt_callbacks is only valid"), "{msg}");

        let registry = Registry::shared();
        registry.write().unwrap().register_collection(auth_def(
            "users",
            vec![
                AuthMethod::password_login_builder()
                    .mfa(MfaMode::Totp)
                    .mfa_exempt_callbacks(exempt())
                    .build(),
            ],
        ));
        validate_auth_methods(&registry.read().unwrap()).expect("exemption with mfa passes");
    }

    #[test]
    fn validate_auth_methods_rejects_empty_surfaces() {
        let msg = auth_error(auth_def(
            "users",
            vec![
                AuthMethod::password_login(),
                AuthMethod::Bearer {
                    surfaces: SurfaceSet::from_list(vec![]),
                },
            ],
        ));
        assert!(
            msg.contains("empty `surfaces`"),
            "expected empty-surfaces error: {msg}"
        );
    }

    #[test]
    fn validate_auth_methods_rejects_empty_header_activation() {
        let msg = auth_error(auth_def(
            "users",
            vec![
                AuthMethod::password_login(),
                AuthMethod::bearer(),
                AuthMethod::Strategy {
                    name: "bogus".to_string(),
                    authenticate: HookRef::new("hooks.auth.bogus"),
                    activates_on: Activation::Header {
                        header: "  ".to_string(),
                    },
                    surfaces: SurfaceSet::admin_only(),
                },
            ],
        ));
        assert!(
            msg.contains("empty") && msg.contains("activates_on.header"),
            "expected empty-header error: {msg}"
        );
    }

    #[test]
    fn validate_auth_methods_rejects_empty_authenticate_ref() {
        let msg = auth_error(auth_def(
            "users",
            vec![
                AuthMethod::password_login(),
                AuthMethod::bearer(),
                AuthMethod::Strategy {
                    name: "incomplete".to_string(),
                    authenticate: HookRef::new(""),
                    activates_on: Activation::always(),
                    surfaces: SurfaceSet::admin_only(),
                },
            ],
        ));
        assert!(
            msg.contains("empty `authenticate`"),
            "expected empty-authenticate error: {msg}"
        );
    }

    #[test]
    fn validate_auth_methods_accepts_well_formed_default_set() {
        let registry = Registry::shared();
        registry
            .write()
            .unwrap()
            .register_collection(auth_def("users", Auth::default_methods()));
        validate_auth_methods(&registry.read().unwrap())
            .expect("default methods should pass validation");
    }
}
