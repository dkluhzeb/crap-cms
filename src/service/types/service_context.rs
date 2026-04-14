//! Service context — calling environment for all service operations.

use anyhow::{Context as _, anyhow};

use std::borrow::Cow;

use crate::{
    core::{
        CollectionDefinition, Document, FieldDefinition, collection::GlobalDefinition,
        event::SharedInvalidationTransport,
    },
    db::{BoxedConnection, DbConnection, DbPool, query::helpers::global_table},
    hooks::HookRunner,
    service::{
        ServiceError,
        hooks::{ReadHooks, WriteHooks},
    },
};

/// The target definition for a service operation.
pub enum Def<'a> {
    Collection(&'a CollectionDefinition),
    Global(&'a GlobalDefinition),
    /// No definition — for operations that only need slug + infrastructure
    /// (jobs, persist helpers).
    None,
}

/// Calling environment for all service operations.
///
/// Carries infrastructure (connection, hooks), identity (user, access mode),
/// and the target (slug, definition).
pub struct ServiceContext<'a> {
    /// Connection pool. `None` when called from Lua CRUD.
    pub pool: Option<&'a DbPool>,
    /// Pre-existing connection/transaction. When set, functions use this
    /// instead of acquiring from the pool.
    pub conn: Option<&'a dyn DbConnection>,
    /// Hook runner. Required for pool-based write operations (creates
    /// `RunnerWriteHooks` internally after opening a transaction).
    pub runner: Option<&'a HookRunner>,
    /// Hooks for read operations.
    pub read_hooks: Option<&'a dyn ReadHooks>,
    /// Hooks for write operations.
    pub write_hooks: Option<&'a dyn WriteHooks>,
    /// Authenticated user document.
    pub user: Option<&'a Document>,
    /// Bypass all access checks (MCP, Lua `overrideAccess`).
    pub override_access: bool,
    /// Transport for publishing user-invalidation signals (live-stream
    /// tear-down on lock / hard-delete). `None` = publishing is a no-op.
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    /// Collection or global slug.
    pub slug: &'a str,
    /// Collection or global definition.
    pub def: Def<'a>,
}

impl<'a> ServiceContext<'a> {
    /// Create a builder with required slug and definition.
    pub fn collection(slug: &'a str, def: &'a CollectionDefinition) -> ServiceContextBuilder<'a> {
        ServiceContextBuilder::new(slug, Def::Collection(def))
    }

    /// Create a builder for a global operation.
    pub fn global(slug: &'a str, def: &'a GlobalDefinition) -> ServiceContextBuilder<'a> {
        ServiceContextBuilder::new(slug, Def::Global(def))
    }

    /// Create a builder with slug only — no definition. For operations that
    /// don't need a collection/global definition (jobs, low-level persist).
    pub fn slug_only(slug: &'a str) -> ServiceContextBuilder<'a> {
        ServiceContextBuilder::new(slug, Def::None)
    }

    /// Resolve a connection — use `self.conn` if set, otherwise acquire from pool.
    pub fn resolve_conn(&self) -> Result<ResolvedConn<'_>, ServiceError> {
        match self.conn {
            Some(c) => Ok(ResolvedConn::Borrowed(c)),
            None => {
                let pool = self.pool.context("service requires pool or conn")?;
                let conn = pool.get().context("DB connection")?;
                Ok(ResolvedConn::Owned(conn))
            }
        }
    }

    /// Get read hooks or error.
    pub fn read_hooks(&self) -> Result<&dyn ReadHooks, ServiceError> {
        self.read_hooks
            .ok_or_else(|| ServiceError::Internal(anyhow!("read_hooks not set")))
    }

    /// Get write hooks or error.
    pub fn write_hooks(&self) -> Result<&dyn WriteHooks, ServiceError> {
        self.write_hooks
            .ok_or_else(|| ServiceError::Internal(anyhow!("write_hooks not set")))
    }

    /// Get the hook runner or error.
    pub fn runner(&self) -> Result<&HookRunner, ServiceError> {
        self.runner
            .ok_or_else(|| ServiceError::Internal(anyhow!("runner not set")))
    }

    /// Get the definition as a `CollectionDefinition`. Panics if not a collection.
    pub fn collection_def(&self) -> &CollectionDefinition {
        match &self.def {
            Def::Collection(d) => d,
            _ => panic!("expected Def::Collection, got {:?}", self.def_variant()),
        }
    }

    /// Get the definition as a `GlobalDefinition`. Panics if not a global.
    pub fn global_def(&self) -> &GlobalDefinition {
        match &self.def {
            Def::Global(d) => d,
            _ => panic!("expected Def::Global, got {:?}", self.def_variant()),
        }
    }

    /// Derive the version table name: slug for collections, `_global_{slug}` for globals.
    pub fn version_table(&self) -> Cow<'_, str> {
        match &self.def {
            Def::Collection(_) | Def::None => Cow::Borrowed(self.slug),
            Def::Global(_) => Cow::Owned(global_table(self.slug)),
        }
    }

    /// Get the read access reference from the definition.
    pub fn read_access_ref(&self) -> Option<&str> {
        match &self.def {
            Def::Collection(d) => d.access.read.as_deref(),
            Def::Global(d) => d.access.read.as_deref(),
            Def::None => None,
        }
    }

    /// Get field definitions from either collection or global def.
    /// Panics if `Def::None`.
    pub fn fields(&self) -> &[FieldDefinition] {
        match &self.def {
            Def::Collection(d) => &d.fields,
            Def::Global(d) => &d.fields,
            Def::None => panic!("fields() called on Def::None"),
        }
    }

    /// Publish a user-invalidation signal if an invalidation transport is
    /// configured. Fire-and-forget — no-op when no transport is attached.
    ///
    /// Called from the service layer (e.g. `lock_user`, `delete_document_core`
    /// for hard-delete of auth collections) so every surface that routes
    /// through the service layer gets live-stream tear-down for free.
    pub fn publish_user_invalidation(&self, user_id: &str) {
        if let Some(transport) = &self.invalidation_transport {
            transport.publish(user_id.to_string());
        }
    }

    fn def_variant(&self) -> &'static str {
        match &self.def {
            Def::Collection(_) => "Collection",
            Def::Global(_) => "Global",
            Def::None => "None",
        }
    }
}

/// A resolved connection — either borrowed from ctx or owned from pool.
pub enum ResolvedConn<'a> {
    Borrowed(&'a dyn DbConnection),
    Owned(BoxedConnection),
}

impl ResolvedConn<'_> {
    pub fn as_ref(&self) -> &dyn DbConnection {
        match self {
            ResolvedConn::Borrowed(c) => *c,
            ResolvedConn::Owned(c) => c,
        }
    }
}

/// Builder for [`ServiceContext`].
pub struct ServiceContextBuilder<'a> {
    slug: &'a str,
    def: Def<'a>,
    pool: Option<&'a DbPool>,
    conn: Option<&'a dyn DbConnection>,
    runner: Option<&'a HookRunner>,
    read_hooks: Option<&'a dyn ReadHooks>,
    write_hooks: Option<&'a dyn WriteHooks>,
    user: Option<&'a Document>,
    override_access: bool,
    invalidation_transport: Option<SharedInvalidationTransport>,
}

impl<'a> ServiceContextBuilder<'a> {
    pub fn new(slug: &'a str, def: Def<'a>) -> Self {
        Self {
            slug,
            def,
            pool: None,
            conn: None,
            runner: None,
            read_hooks: None,
            write_hooks: None,
            user: None,
            override_access: false,
            invalidation_transport: None,
        }
    }

    pub fn pool(mut self, pool: &'a DbPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn conn(mut self, conn: &'a dyn DbConnection) -> Self {
        self.conn = Some(conn);
        self
    }

    pub fn runner(mut self, runner: &'a HookRunner) -> Self {
        self.runner = Some(runner);
        self
    }

    pub fn read_hooks(mut self, hooks: &'a dyn ReadHooks) -> Self {
        self.read_hooks = Some(hooks);
        self
    }

    pub fn write_hooks(mut self, hooks: &'a dyn WriteHooks) -> Self {
        self.write_hooks = Some(hooks);
        self
    }

    pub fn user(mut self, user: Option<&'a Document>) -> Self {
        self.user = user;
        self
    }

    pub fn override_access(mut self, override_access: bool) -> Self {
        self.override_access = override_access;
        self
    }

    /// Attach a user-invalidation transport. When set, service-layer
    /// operations that revoke user sessions (lock, hard-delete of auth
    /// documents) will publish a tear-down signal.
    pub fn invalidation_transport(
        mut self,
        transport: Option<SharedInvalidationTransport>,
    ) -> Self {
        self.invalidation_transport = transport;
        self
    }

    pub fn build(self) -> ServiceContext<'a> {
        ServiceContext {
            pool: self.pool,
            conn: self.conn,
            runner: self.runner,
            read_hooks: self.read_hooks,
            write_hooks: self.write_hooks,
            user: self.user,
            override_access: self.override_access,
            invalidation_transport: self.invalidation_transport,
            slug: self.slug,
            def: self.def,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::core::{
        CollectionDefinition,
        event::{InProcessInvalidationBus, SharedInvalidationTransport},
    };

    use super::*;

    #[test]
    fn publish_user_invalidation_is_noop_without_transport() {
        let def = CollectionDefinition::new("users");
        let ctx = ServiceContext::collection("users", &def).build();

        // No transport attached — must not panic and must complete silently.
        ctx.publish_user_invalidation("user-123");
        assert!(ctx.invalidation_transport.is_none());
    }

    #[tokio::test]
    async fn publish_user_invalidation_publishes_when_transport_set() {
        let bus = Arc::new(InProcessInvalidationBus::new());
        let transport: SharedInvalidationTransport = bus.clone();
        let mut rx = transport.subscribe();

        let def = CollectionDefinition::new("users");
        let ctx = ServiceContext::collection("users", &def)
            .invalidation_transport(Some(transport))
            .build();

        ctx.publish_user_invalidation("user-123");

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("expected an invalidation signal");
        assert_eq!(received, "user-123");
    }

    #[test]
    fn builder_default_transport_is_none() {
        let def = CollectionDefinition::new("users");
        let ctx = ServiceContext::collection("users", &def).build();
        assert!(ctx.invalidation_transport.is_none());
    }
}
