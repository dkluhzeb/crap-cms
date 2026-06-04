//! Service context — calling environment for all service operations.

use std::borrow::Cow;

use anyhow::{Context as _, anyhow};
use tracing::warn;

use crate::core::collection::Auth;
use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, DocumentFields, FieldDefinition, GlobalDefinition,
        LiveMode, SharedCache, SharedEventTransport, SharedInvalidationTransport,
        event::{EventOperation, EventTarget, EventUser},
    },
    db::{BoxedConnection, DbConnection, DbPool, query::helpers::global_table},
    hooks::HookRunner,
    hooks::lifecycle::PublishEventInput,
    service::{
        ServiceError,
        hooks::{ReadHooks, WriteHooks},
        types::{EmailContext, EventQueue, PendingEvent, PendingVerification, VerificationQueue},
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
    /// Email configuration for verification emails on auth collection
    /// creates. `None` = verification emails are skipped.
    pub email_ctx: Option<EmailContext>,
    /// Populate cache. When set, service-layer write operations clear
    /// the cache after commit to prevent stale relationship data.
    pub cache: Option<SharedCache>,
    /// Transport for publishing mutation events to live-update subscribers.
    /// `None` = event publishing is a no-op.
    pub event_transport: Option<SharedEventTransport>,
    /// Whether this operation emits its own per-document mutation events.
    /// `true` (default) for single ops; surfaces set it from the `events`
    /// flag (single default `true`, bulk default `false`). When `false`,
    /// `publish_mutation_event` is a no-op for this op — but nested-hook
    /// events (drained via `flush_queue`) and user/session invalidation
    /// still fire.
    pub emit_events: bool,
    /// Queue for events accumulated during a transaction. When set,
    /// `publish_mutation_event` pushes to this queue instead of publishing
    /// immediately. The caller flushes after commit via `flush_event_queue`.
    pub event_queue: Option<EventQueue>,
    /// Queue for verification emails accumulated during a transaction.
    /// Flushed after commit by the parent alongside events.
    pub verification_queue: Option<VerificationQueue>,
    /// Transport for publishing user-invalidation signals (live-stream
    /// tear-down on lock / hard-delete). `None` = publishing is a no-op.
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    /// Collection or global slug.
    pub slug: &'a str,
    /// Collection or global definition.
    pub def: Def<'a>,
    /// Locale configuration. Required when the def has localized fields and
    /// the operation may need to read raw rows without an explicit
    /// `LocaleContext` (e.g. unpublish, version snapshot). Without this,
    /// internal `find_by_id_raw` calls fall back to non-locale-aware column
    /// names, generating SELECTs that reference `title` instead of
    /// `title__en` and failing with `no such column`.
    pub locale_config: Option<&'a LocaleConfig>,
}

impl<'a> ServiceContext<'a> {
    /// Create a builder with required slug and definition.
    #[must_use]
    pub fn collection(slug: &'a str, def: &'a CollectionDefinition) -> ServiceContextBuilder<'a> {
        ServiceContextBuilder::new(slug, Def::Collection(def))
    }

    /// Create a builder for a global operation.
    #[must_use]
    pub fn global(slug: &'a str, def: &'a GlobalDefinition) -> ServiceContextBuilder<'a> {
        ServiceContextBuilder::new(slug, Def::Global(def))
    }

    /// Create a builder with slug only — no definition. For operations that
    /// don't need a collection/global definition (jobs, low-level persist).
    #[must_use]
    pub fn slug_only(slug: &'a str) -> ServiceContextBuilder<'a> {
        ServiceContextBuilder::new(slug, Def::None)
    }

    /// Resolve a connection — use `self.conn` if set, otherwise acquire from pool.
    ///
    /// # Errors
    ///
    /// Returns an internal error if neither a connection nor a pool was
    /// attached to the context, or if the pool fails to hand out a connection.
    pub fn resolve_conn(&self) -> Result<ResolvedConn<'_>, ServiceError> {
        if let Some(c) = self.conn {
            Ok(ResolvedConn::Borrowed(c))
        } else {
            let pool = self.pool.context("service requires pool or conn")?;
            let conn = pool.get().context("DB connection")?;
            Ok(ResolvedConn::Owned(conn))
        }
    }

    /// Get read hooks or error.
    ///
    /// # Errors
    ///
    /// Returns an internal error if `read_hooks` were not attached to the context.
    pub fn read_hooks(&self) -> Result<&dyn ReadHooks, ServiceError> {
        self.read_hooks
            .ok_or_else(|| ServiceError::Internal(anyhow!("read_hooks not set")))
    }

    /// Get write hooks or error.
    ///
    /// # Errors
    ///
    /// Returns an internal error if `write_hooks` were not attached to the context.
    pub fn write_hooks(&self) -> Result<&dyn WriteHooks, ServiceError> {
        self.write_hooks
            .ok_or_else(|| ServiceError::Internal(anyhow!("write_hooks not set")))
    }

    /// Get the hook runner or error.
    ///
    /// # Errors
    ///
    /// Returns an internal error if the hook `runner` was not attached.
    pub fn runner(&self) -> Result<&HookRunner, ServiceError> {
        self.runner
            .ok_or_else(|| ServiceError::Internal(anyhow!("runner not set")))
    }

    /// Build a default `LocaleContext` from the attached locale config.
    /// Used by write paths that need to read raw rows (e.g. unpublish,
    /// version snapshot) on collections with localized fields.
    ///
    /// Returns `None` when no locale config is attached or when
    /// localization is disabled — in both cases the SELECT fallback to
    /// bare column names is correct.
    ///
    /// Uses `LocaleMode::Default` (resolved at the default locale, flat
    /// keys) rather than `LocaleMode::All` (grouped `{en, de}` objects).
    /// `All` triggered `group_locale_fields` and produced
    /// `title: {"en": "X", "de": null}` shape that:
    /// - diverged from what `persist_draft_version` snapshots (Single
    ///   mode, flat resolved value),
    /// - broke user hooks expecting flat keys,
    /// - leaked through broadcast events.
    ///
    /// The default-locale-resolved shape matches every other write path.
    /// Snapshot fidelity for non-default locales is the same as regular
    /// draft saves (lossy for non-default-locale columns) — preserving
    /// all locales in snapshots is a separate change.
    #[must_use]
    pub fn default_locale_ctx(&self) -> Option<crate::db::query::LocaleContext> {
        let config = self.locale_config?;
        if !config.is_enabled() {
            return None;
        }
        Some(crate::db::query::LocaleContext {
            mode: crate::db::query::LocaleMode::Default,
            config: config.clone(),
        })
    }

    /// Get the definition as a `CollectionDefinition`. Errors if the context
    /// was built with `Def::Global` or `Def::None`.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the context was built with a global or
    /// no definition rather than a collection.
    pub fn collection_def(&self) -> Result<&CollectionDefinition, ServiceError> {
        match &self.def {
            Def::Collection(d) => Ok(d),
            _ => Err(ServiceError::Internal(anyhow!(
                "expected Def::Collection, got {}",
                self.def_variant()
            ))),
        }
    }

    /// Get the definition as a `GlobalDefinition`. Errors if the context was
    /// built with `Def::Collection` or `Def::None`.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the context was built with a collection
    /// or no definition rather than a global.
    pub fn global_def(&self) -> Result<&GlobalDefinition, ServiceError> {
        match &self.def {
            Def::Global(d) => Ok(d),
            _ => Err(ServiceError::Internal(anyhow!(
                "expected Def::Global, got {}",
                self.def_variant()
            ))),
        }
    }

    /// Derive the version table name: slug for collections, `_global_{slug}` for globals.
    #[must_use]
    pub fn version_table(&self) -> Cow<'_, str> {
        match &self.def {
            Def::Collection(_) | Def::None => Cow::Borrowed(self.slug),
            Def::Global(_) => Cow::Owned(global_table(self.slug)),
        }
    }

    /// Get the read access reference from the definition.
    #[must_use]
    pub fn read_access_ref(&self) -> Option<&str> {
        match &self.def {
            Def::Collection(d) => d.access.read.as_deref(),
            Def::Global(d) => d.access.read.as_deref(),
            Def::None => None,
        }
    }

    /// Get field definitions from either collection or global def. Errors
    /// if the context was built with `Def::None`.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the context has no attached definition.
    pub fn fields(&self) -> Result<&[FieldDefinition], ServiceError> {
        match &self.def {
            Def::Collection(d) => Ok(&d.fields),
            Def::Global(d) => Ok(&d.fields),
            Def::None => Err(ServiceError::Internal(anyhow!(
                "fields() called on Def::None"
            ))),
        }
    }

    /// Send a verification email if this is an auth collection with
    /// `verify_email` enabled and the document has an email field.
    /// No-op when email context is not attached.
    pub fn maybe_send_verification(&self, doc: &Document) {
        let Def::Collection(def) = &self.def else {
            return;
        };

        let should_verify =
            def.is_auth_collection() && def.auth.as_ref().is_some_and(Auth::requires_verify_email);

        if !should_verify {
            return;
        }

        let Some(email) = doc.get_str("email") else {
            return;
        };

        if let (Some(pool), Some(email_ctx)) = (self.pool, &self.email_ctx) {
            email_ctx.send_verification(
                pool.clone(),
                self.slug.to_string(),
                doc.id.to_string(),
                email.to_string(),
            );
            return;
        }

        if let Some(ref queue) = self.verification_queue {
            queue.borrow_mut().push(PendingVerification {
                slug: self.slug.to_string(),
                doc_id: doc.id.to_string(),
                email: email.to_string(),
            });
        }
    }

    /// Clear the populate cache after a write operation.
    /// No-op when no cache is attached.
    pub fn clear_cache(&self) {
        if let Some(ref cache) = self.cache
            && let Err(e) = cache.clear()
        {
            warn!("Cache clear failed: {e:#}");
        }
    }

    /// Publish (or queue) a mutation event.
    ///
    /// When an `event_queue` is set (inside a transaction), the event is
    /// queued for later flushing. Otherwise it publishes immediately.
    /// No-op when no event transport is attached.
    pub fn publish_mutation_event(
        &self,
        operation: EventOperation,
        doc_id: &str,
        data: &DocumentFields,
    ) {
        if !self.emit_events {
            return;
        }

        if self.event_transport.is_none() {
            return;
        }

        let (hooks, live, live_mode) = match &self.def {
            Def::Collection(d) => (d.hooks.clone(), d.live.clone(), d.live_mode),
            Def::Global(d) => (d.hooks.clone(), d.live.clone(), d.live_mode),
            Def::None => return,
        };

        let data = if live_mode == LiveMode::Full {
            data.clone()
        } else {
            DocumentFields::new()
        };

        let edited_by = self.user.map(|u| {
            let email = u.get_str("email").unwrap_or_default().to_string();
            EventUser::new(u.id.to_string(), email)
        });

        let target = match &self.def {
            Def::Collection(_) | Def::None => EventTarget::Collection,
            Def::Global(_) => EventTarget::Global,
        };

        let pending = PendingEvent {
            target,
            operation,
            collection: self.slug.to_string(),
            document_id: doc_id.to_string(),
            data,
            edited_by,
            hooks,
            live,
        };

        if let Some(ref queue) = self.event_queue {
            queue.borrow_mut().push(pending);
            return;
        }

        let Some(runner) = self.runner else { return };
        runner.publish_event(
            &self.event_transport,
            &pending.hooks,
            pending.live.as_ref(),
            PublishEventInput::builder(pending.target, pending.operation)
                .collection(pending.collection)
                .document_id(pending.document_id)
                .data(pending.data)
                .edited_by(pending.edited_by)
                .build(),
        );
    }

    /// Publish a user-invalidation signal if an invalidation transport is
    /// configured. Fire-and-forget — no-op when no transport is attached.
    ///
    /// Called from the service layer (e.g. `lock_user`, `delete_document_in_conn`
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
    email_ctx: Option<EmailContext>,
    cache: Option<SharedCache>,
    event_transport: Option<SharedEventTransport>,
    emit_events: bool,
    event_queue: Option<EventQueue>,
    verification_queue: Option<VerificationQueue>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    locale_config: Option<&'a LocaleConfig>,
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
            email_ctx: None,
            cache: None,
            event_transport: None,
            emit_events: true,
            event_queue: None,
            verification_queue: None,
            invalidation_transport: None,
            locale_config: None,
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

    /// Attach email context for verification emails on auth collection creates.
    pub fn email_ctx(mut self, ctx: Option<EmailContext>) -> Self {
        self.email_ctx = ctx;
        self
    }

    /// Attach a populate cache. When set, service-layer write operations
    /// clear the cache after commit to prevent stale relationship data.
    pub fn cache(mut self, cache: Option<SharedCache>) -> Self {
        self.cache = cache;
        self
    }

    /// Attach a mutation event transport. When set, service-layer write
    /// operations publish events to all Subscribe/SSE clients.
    pub fn event_transport(mut self, transport: Option<SharedEventTransport>) -> Self {
        self.event_transport = transport;
        self
    }

    /// Set whether this operation emits its own per-document mutation events.
    /// Defaults to `true`. Surfaces pass the `events` flag here — single ops
    /// default `true`, bulk ops default `false`. Does not affect nested-hook
    /// events or user/session invalidation.
    pub fn emit_events(mut self, emit: bool) -> Self {
        self.emit_events = emit;
        self
    }

    /// Attach an event queue for deferred publishing (used inside transactions).
    pub fn event_queue(mut self, queue: EventQueue) -> Self {
        self.event_queue = Some(queue);
        self
    }

    /// Attach a verification queue for deferred email sending (used inside transactions).
    pub fn verification_queue(mut self, queue: VerificationQueue) -> Self {
        self.verification_queue = Some(queue);
        self
    }

    /// Apply infrastructure from a `LuaCrudInfra` bundle (event transport,
    /// cache, event queue, verification queue). Used by Lua CRUD functions
    /// to transfer the parent's infrastructure in a single call. Optional
    /// shape mirrors the other per-context attachments so callers can pass
    /// the result of `hook_lua_infra(lua).as_ref()` directly without an
    /// `if let` wrapper.
    pub fn lua_infra(mut self, infra: Option<&crate::hooks::LuaCrudInfra>) -> Self {
        let Some(infra) = infra else { return self };
        if infra.event_transport.is_some() {
            self.event_transport.clone_from(&infra.event_transport);
        }
        if infra.cache.is_some() {
            self.cache.clone_from(&infra.cache);
        }
        self.event_queue.clone_from(&infra.event_queue);
        self.verification_queue
            .clone_from(&infra.verification_queue);
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

    /// Attach the locale configuration. Required for write paths
    /// (`unpublish_document`, `persist_unpublish`) on collections with
    /// localized fields when locales are enabled — without it, the raw
    /// SELECT inside the read step misses the `__en` / `__de` suffixes
    /// and fails with `no such column`. Optional shape mirrors the other
    /// per-context attachments (`cache`, `event_transport`, …) so callers
    /// can forward `ctx.locale_config` straight into a child builder
    /// without an `if let`.
    pub fn locale_config(mut self, config: Option<&'a LocaleConfig>) -> Self {
        self.locale_config = config;
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
            email_ctx: self.email_ctx,
            cache: self.cache,
            event_transport: self.event_transport,
            emit_events: self.emit_events,
            event_queue: self.event_queue,
            verification_queue: self.verification_queue,
            invalidation_transport: self.invalidation_transport,
            slug: self.slug,
            def: self.def,
            locale_config: self.locale_config,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::core::{
        CollectionDefinition, DocumentFields,
        event::{InProcessInvalidationBus, SharedEventTransport, SharedInvalidationTransport},
    };

    use super::*;

    #[test]
    fn publish_user_invalidation_is_noop_without_transport() {
        let def = CollectionDefinition::new("users");
        let ctx = ServiceContext::collection("users", &def).build();

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

    /// SAFE-DEFAULT GUARD: `emit_events` defaults to `true` so single ops keep
    /// publishing their mutation events unless a surface explicitly opts out.
    #[test]
    fn builder_emits_events_by_default() {
        let def = CollectionDefinition::new("posts");
        assert!(
            ServiceContext::collection("posts", &def)
                .build()
                .emit_events
        );
    }

    /// `emit_events(true)` (the default) enqueues the mutation event.
    #[test]
    fn emit_events_true_enqueues_mutation_event() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use crate::core::event::InProcessEventBus;

        let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
        let queue = Rc::new(RefCell::new(Vec::new()));
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport))
            .event_queue(queue.clone())
            .build();

        ctx.publish_mutation_event(EventOperation::Update, "doc-1", &DocumentFields::new());
        assert_eq!(
            queue.borrow().len(),
            1,
            "default emit_events should enqueue"
        );
    }

    /// `emit_events(false)` makes `publish_mutation_event` a no-op — nothing is
    /// enqueued even with a transport and queue attached.
    #[test]
    fn emit_events_false_suppresses_mutation_event() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use crate::core::event::InProcessEventBus;

        let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
        let queue = Rc::new(RefCell::new(Vec::new()));
        let def = CollectionDefinition::new("posts");
        let ctx = ServiceContext::collection("posts", &def)
            .event_transport(Some(transport))
            .event_queue(queue.clone())
            .emit_events(false)
            .build();

        ctx.publish_mutation_event(EventOperation::Update, "doc-1", &DocumentFields::new());
        assert!(
            queue.borrow().is_empty(),
            "emit_events=false must suppress the event"
        );
    }

    /// SAFE-DEFAULT GUARD (most critical): `override_access` bypasses ALL
    /// access checks. It must default to `false` so a surface that forgets to
    /// set it enforces access rather than skipping it. A regression flipping
    /// this default to `true` would silently disable authorization on every
    /// surface that builds a context without opting out.
    #[test]
    fn builder_does_not_override_access_by_default() {
        let def = CollectionDefinition::new("users");
        assert!(
            !ServiceContext::collection("users", &def)
                .build()
                .override_access
        );
        assert!(!ServiceContext::slug_only("users").build().override_access);
    }
}
