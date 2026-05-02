//! Service context — calling environment for all service operations.

use std::{borrow::Cow, cell::RefCell, collections::HashMap, rc::Rc};

use anyhow::{Context as _, anyhow};
use serde_json::Value as JsonValue;
use tracing::warn;

use std::sync::Arc;

use crate::{
    config::{EmailConfig, LocaleConfig, ServerConfig},
    core::{
        CollectionDefinition, Document, FieldDefinition,
        cache::SharedCache,
        collection::{GlobalDefinition, Hooks, LiveMode, LiveSetting},
        email::EmailRenderer,
        event::{
            EventOperation, EventTarget, EventUser, SharedEventTransport,
            SharedInvalidationTransport,
        },
    },
    db::{BoxedConnection, DbConnection, DbPool, query::helpers::global_table},
    hooks::HookRunner,
    hooks::lifecycle::PublishEventInput,
    service::{
        ServiceError,
        hooks::{ReadHooks, WriteHooks},
    },
};

/// Bundled email configuration for verification emails.
/// Cloning is cheap (configs are small, renderer is Arc).
#[derive(Clone)]
pub struct EmailContext {
    pub email_config: EmailConfig,
    pub email_renderer: Arc<EmailRenderer>,
    pub server_config: ServerConfig,
}

/// A mutation event waiting to be published after transaction commit.
pub struct PendingEvent {
    pub target: EventTarget,
    pub operation: EventOperation,
    pub collection: String,
    pub document_id: String,
    pub data: HashMap<String, JsonValue>,
    pub edited_by: Option<EventUser>,
    pub hooks: Hooks,
    pub live: Option<LiveSetting>,
}

/// Shared queue for events accumulated during a transaction.
/// Cloning is cheap (Rc + RefCell).
pub type EventQueue = Rc<RefCell<Vec<PendingEvent>>>;

/// A verification email waiting to be sent after transaction commit.
pub struct PendingVerification {
    pub slug: String,
    pub doc_id: String,
    pub email: String,
}

/// Shared queue for verification emails accumulated during a transaction.
pub type VerificationQueue = Rc<RefCell<Vec<PendingVerification>>>;

/// Flush all events from a queue, publishing each via the given context's runner + transport.
pub fn flush_queue(ctx: &ServiceContext, queue: &EventQueue) {
    let Some(runner) = ctx.runner else { return };

    let events: Vec<PendingEvent> = queue.borrow_mut().drain(..).collect();

    for pending in events {
        runner.publish_event(
            &ctx.event_transport,
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
}

/// Flush all queued verification emails, sending each via the parent's pool + email context.
pub fn flush_verification_queue(ctx: &ServiceContext, queue: &VerificationQueue) {
    let Some(pool) = ctx.pool else { return };
    let Some(ref email_ctx) = ctx.email_ctx else {
        return;
    };

    let pending: Vec<PendingVerification> = queue.borrow_mut().drain(..).collect();

    for v in pending {
        crate::service::send_verification_email(
            pool.clone(),
            email_ctx.email_config.clone(),
            email_ctx.email_renderer.clone(),
            email_ctx.server_config.clone(),
            v.slug,
            v.doc_id,
            v.email,
        );
    }
}

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

    /// Publish a mutation event to all Subscribe/SSE clients.
    ///
    /// Fire-and-forget: spawns a background task for hooks + broadcast.
    /// Send a verification email if this is an auth collection with
    /// `verify_email` enabled and the document has an email field.
    /// No-op when email context is not attached.
    pub fn maybe_send_verification(&self, doc: &Document) {
        let def = match &self.def {
            Def::Collection(d) => d,
            _ => return,
        };

        let should_verify =
            def.is_auth_collection() && def.auth.as_ref().is_some_and(|a| a.verify_email);

        if !should_verify {
            return;
        }

        let Some(email) = doc.get_str("email") else {
            return;
        };

        // Pool mode: send immediately.
        if let (Some(pool), Some(email_ctx)) = (self.pool, &self.email_ctx) {
            crate::service::send_verification_email(
                pool.clone(),
                email_ctx.email_config.clone(),
                email_ctx.email_renderer.clone(),
                email_ctx.server_config.clone(),
                self.slug.to_string(),
                doc.id.to_string(),
                email.to_string(),
            );
            return;
        }

        // Conn mode: queue for the parent to send after commit.
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
        data: &HashMap<String, JsonValue>,
    ) {
        if self.event_transport.is_none() {
            return;
        }

        let (hooks, live, live_mode) = match &self.def {
            Def::Collection(d) => (d.hooks.clone(), d.live.clone(), d.live_mode),
            Def::Global(d) => (d.hooks.clone(), d.live.clone(), d.live_mode),
            Def::None => return,
        };

        // Only clone document data for Full mode — Metadata mode subscribers
        // ignore it, so cloning fields would be wasted work.
        let data = if live_mode == LiveMode::Full {
            data.clone()
        } else {
            HashMap::new()
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

        // If inside a transaction, queue for later flush.
        if let Some(ref queue) = self.event_queue {
            queue.borrow_mut().push(pending);
            return;
        }

        // Otherwise publish immediately (post-commit path).
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

    /// Flush all queued events (call after transaction commit).
    pub fn flush_event_queue(&self) {
        let Some(ref queue) = self.event_queue else {
            return;
        };
        let Some(runner) = self.runner else { return };

        let events: Vec<PendingEvent> = queue.borrow_mut().drain(..).collect();

        for pending in events {
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
    email_ctx: Option<EmailContext>,
    cache: Option<SharedCache>,
    event_transport: Option<SharedEventTransport>,
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
            self.event_transport = infra.event_transport.clone();
        }
        if infra.cache.is_some() {
            self.cache = infra.cache.clone();
        }
        self.event_queue = infra.event_queue.clone();
        self.verification_queue = infra.verification_queue.clone();
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
