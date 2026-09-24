//! Activation discriminator for `strategy` auth methods.

use serde::{Deserialize, Deserializer, Serialize};

use crate::typegen::lua::{LuaAlias, LuaTaggedClass};

/// How a `Strategy` method is triggered for a given request.
///
/// `Always` is the explicit catch-all escape hatch (`mTLS`, multi-
/// signal strategies, `IdP` introspection). Required by the
/// validator to be the literal `{ always = true }` table —
/// `{ always = false }` is rejected at deserialize-time via the
/// custom `deserialize_true_only` helper. Use a `Header`
/// discriminator when the strategy fires on a specific request
/// header, which is the common case for API-key / SSO patterns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, LuaTaggedClass)]
#[serde(untagged)]
#[lua(class = "crap.Activation")]
pub enum Activation {
    /// Strategy is invoked on every request that passes the surface
    /// filter. The strategy itself decides per-request whether to
    /// authenticate. Emits a startup warning to make accidental
    /// always-active strategies loud.
    Always {
        /// Must be `true`. `false` is rejected at deserialize time.
        #[serde(deserialize_with = "deserialize_true_only")]
        #[lua(ty = "true")]
        always: bool,
    },
    /// Strategy fires only when the named header (lowercase, gRPC
    /// metadata or HTTP header) is present on the request.
    Header {
        /// Header name (lowercase) — e.g. `"x-api-key"`.
        header: String,
    },
}

impl Activation {
    /// Construct an `Always`-active activation. Use sparingly;
    /// prefer header-discriminated activation so each strategy is
    /// bound to its own request signal.
    #[must_use]
    pub fn always() -> Self {
        Activation::Always { always: true }
    }

    /// Construct a header-discriminated activation.
    #[must_use]
    pub fn header(name: impl Into<String>) -> Self {
        Activation::Header {
            header: name.into(),
        }
    }
}

fn deserialize_true_only<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let value = bool::deserialize(d)?;
    if !value {
        return Err(serde::de::Error::custom(
            "activates_on.always must be `true`; set to `false` is meaningless. \
             Remove the method or use an explicit `header` discriminator instead.",
        ));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_always_false_is_rejected() {
        // The sentinel rule prevents `{ always = false }` from being a
        // valid disabled-strategy declaration — it should be expressed
        // by removing the method entirely or using a header discriminator.
        let json = r#"{"always":false}"#;
        let err = serde_json::from_str::<Activation>(json).unwrap_err();
        assert!(
            err.to_string().contains("did not match any variant")
                || err.to_string().contains("must be `true`"),
            "expected helpful error, got: {err}"
        );
    }
}
