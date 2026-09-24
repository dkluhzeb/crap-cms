//! Host surfaces an auth method can fire on.

use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAlias;

/// Which host surfaces a method can fire on. Surface filtering is
/// per-method: a method whose `surfaces` list omits the current
/// request's surface is skipped by the evaluator.
//
// Closed set — new host transports (MCP-acting-as-user, webhooks, etc.)
// would add a variant here. Per-variant Rust docs are kept internal
// (no `///`); the user-facing alias above is what the derive emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, LuaAlias)]
#[serde(rename_all = "lowercase")]
#[lua(alias = "crap.Surface", rename_all = "lowercase")]
pub enum Surface {
    // Admin HTTP — `/admin/**` routes, middleware-driven.
    Admin,
    // gRPC `ContentAPI` — unary RPCs and `Subscribe`
    // (evaluated at stream open).
    Grpc,
}

impl Surface {
    /// The lowercase wire/hook-context name (`"admin"` / `"grpc"`), matching
    /// the serde spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Grpc => "grpc",
        }
    }
}

/// Ordered list of surfaces this method is allowed on. Empty
/// means "no surface" (effectively disabled); not a default.
/// Default constructors below give sensible per-variant scopes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SurfaceSet(Vec<Surface>);

impl SurfaceSet {
    /// All known surfaces. The default for `Bearer` (the issued
    /// JWT typically works everywhere).
    #[must_use]
    pub fn all() -> Self {
        Self(vec![Surface::Admin, Surface::Grpc])
    }

    /// Admin only. Default for `SessionCookie` (cookies are an
    /// admin-HTTP concept) and `Strategy` (strategies historically
    /// fired only on admin).
    #[must_use]
    pub fn admin_only() -> Self {
        Self(vec![Surface::Admin])
    }

    /// gRPC only. Useful for machine-to-machine auth collections.
    #[must_use]
    pub fn grpc_only() -> Self {
        Self(vec![Surface::Grpc])
    }

    /// Construct from an explicit list of surfaces. Used by parsers
    /// converting Lua-table strings to typed surfaces; prefer
    /// [`Self::all`] / [`Self::admin_only`] / [`Self::grpc_only`]
    /// for programmatic construction.
    #[must_use]
    pub fn from_list(surfaces: Vec<Surface>) -> Self {
        Self(surfaces)
    }

    /// True iff `surface` is in this set.
    #[must_use]
    pub fn contains(&self, surface: Surface) -> bool {
        self.0.contains(&surface)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate the contained surfaces in declaration order.
    pub fn iter(&self) -> std::slice::Iter<'_, Surface> {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a SurfaceSet {
    type Item = &'a Surface;
    type IntoIter = std::slice::Iter<'a, Surface>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
