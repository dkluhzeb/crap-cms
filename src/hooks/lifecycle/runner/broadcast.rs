//! `HookRunner` methods for event broadcasting.

use anyhow::Result;
use mlua::{LuaSerdeExt as _, Value};
use tracing::{debug, warn};

use crate::{
    core::{
        DocumentFields, DocumentId, Hooks, LiveSetting, MutationEventInput, SharedEventTransport,
        Slug,
        event::{EventOperation, EventTarget, EventUser},
    },
    hooks::{
        HookContext, HookEvent, HookRunner,
        lifecycle::{
            LiveFilterContext,
            execution::{
                call_before_broadcast_hook, call_registered_before_broadcast, get_hook_refs,
                resolve_hook_function,
            },
        },
    },
};

/// Bundled parameters for a mutation event to publish.
pub struct PublishEventInput {
    pub target: EventTarget,
    pub operation: EventOperation,
    pub collection: Slug,
    pub document_id: DocumentId,
    pub data: DocumentFields,
    pub edited_by: Option<EventUser>,
}

impl PublishEventInput {
    /// Create a builder with the required target and operation.
    #[must_use]
    pub fn builder(target: EventTarget, operation: EventOperation) -> PublishEventInputBuilder {
        PublishEventInputBuilder::new(target, operation)
    }

    /// Convert into the transport-facing [`MutationEventInput`].
    fn into_transport_input(self) -> MutationEventInput {
        MutationEventInput {
            target: self.target,
            operation: self.operation,
            collection: self.collection,
            document_id: self.document_id,
            data: self.data,
            edited_by: self.edited_by,
        }
    }
}

/// Builder for [`PublishEventInput`].
pub struct PublishEventInputBuilder {
    target: EventTarget,
    operation: EventOperation,
    collection: Option<Slug>,
    document_id: Option<DocumentId>,
    data: DocumentFields,
    edited_by: Option<EventUser>,
}

impl PublishEventInputBuilder {
    pub(crate) fn new(target: EventTarget, operation: EventOperation) -> Self {
        Self {
            target,
            operation,
            collection: None,
            document_id: None,
            data: DocumentFields::new(),
            edited_by: None,
        }
    }

    pub fn collection(mut self, collection: impl Into<Slug>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    pub fn document_id(mut self, document_id: impl Into<DocumentId>) -> Self {
        self.document_id = Some(document_id.into());
        self
    }

    pub fn data(mut self, data: impl Into<DocumentFields>) -> Self {
        self.data = data.into();
        self
    }

    pub fn edited_by(mut self, edited_by: Option<EventUser>) -> Self {
        self.edited_by = edited_by;
        self
    }

    pub fn build(self) -> PublishEventInput {
        PublishEventInput {
            target: self.target,
            operation: self.operation,
            collection: self.collection.expect("collection is required"),
            document_id: self.document_id.expect("document_id is required"),
            data: self.data,
            edited_by: self.edited_by,
        }
    }
}

impl HookRunner {
    /// Run `before_broadcast` hooks. Returns Ok(Some(data)) to broadcast (possibly
    /// with transformed data), or Ok(None) to suppress the event.
    /// No CRUD access (fires after commit, same as `after_change`).
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition or any hook call fails.
    pub fn run_before_broadcast(
        &self,
        hooks: &Hooks,
        collection: &str,
        operation: &str,
        data: DocumentFields,
        document_id: &str,
        edited_by: Option<&EventUser>,
    ) -> Result<Option<DocumentFields>> {
        let hook_refs = get_hook_refs(hooks, HookEvent::BeforeBroadcast);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for("before_broadcast") {
            return Ok(Some(data));
        }

        let mut ctx = HookContext::builder(collection, operation)
            .data(data)
            .document_id(document_id)
            .edited_by(edited_by.cloned())
            .build();

        let lua = self.pool.acquire()?;

        // Run collection-level hook refs first
        for hook_ref in hook_refs {
            debug!(
                "Running before_broadcast hook: {} for {}",
                hook_ref.reference(),
                ctx.collection
            );

            match call_before_broadcast_hook(&lua, hook_ref, ctx.clone())? {
                Some(new_ctx) => ctx = new_ctx,
                None => return Ok(None), // suppressed
            }
        }

        // Run global registered hooks
        match call_registered_before_broadcast(&lua, ctx)? {
            Some(ctx) => Ok(Some(ctx.data)),
            None => Ok(None),
        }
    }

    /// Check if a live event should be broadcast for this mutation.
    /// Returns Ok(true) to broadcast, Ok(false) to suppress.
    /// Runs WITHOUT transaction access (after write committed).
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition or the live-setting evaluation fails.
    pub fn check_live_setting(
        &self,
        live: Option<&LiveSetting>,
        collection: &str,
        operation: &str,
        data: &DocumentFields,
        document_id: &str,
        edited_by: Option<&EventUser>,
    ) -> Result<bool> {
        match live {
            None => Ok(true), // absent = broadcast all
            Some(LiveSetting::Disabled) => Ok(false),
            Some(LiveSetting::Function(hook)) => {
                let lua = self.pool.acquire()?;

                let func = resolve_hook_function(&lua, hook.reference())?;

                // Typed `crap.LiveFilterContext` — single source of truth for
                // the Lua-facing shape (incl. per-config `ctx.options`).
                let ctx = LiveFilterContext {
                    collection,
                    operation,
                    data,
                    document_id,
                    edited_by,
                    options: hook.options(),
                };
                let ctx_value = lua.to_value(&ctx)?;

                let result: Value = func.call(ctx_value)?;
                match result {
                    Value::Boolean(b) => Ok(b),
                    Value::Nil => Ok(false),
                    _ => Ok(true),
                }
            }
        }
    }

    /// Publish a mutation event: check live setting → run `before_broadcast` hooks → `transport.publish()`.
    /// Spawns into a background task (non-blocking, like `fire_after_event`).
    /// Untestable: spawns `tokio::task::spawn_blocking` for async event dispatch.
    #[cfg(not(tarpaulin_include))]
    pub fn publish_event(
        &self,
        event_transport: &Option<SharedEventTransport>,
        hooks: &Hooks,
        live_setting: Option<&LiveSetting>,
        input: PublishEventInput,
    ) {
        let Some(transport) = event_transport else {
            return;
        };

        // Run inline — callers are already on a blocking thread
        // (spawn_blocking in gRPC/admin handlers). Avoids spawning a
        // nested blocking task that competes for the thread pool.
        publish_event_blocking(self, transport, hooks, live_setting, input);
    }
}

/// Background worker for [`HookRunner::publish_event`]:
/// check live setting → run `before_broadcast` hooks → `transport.publish()`.
fn publish_event_blocking(
    runner: &HookRunner,
    transport: &SharedEventTransport,
    hooks: &Hooks,
    live: Option<&LiveSetting>,
    input: PublishEventInput,
) {
    let op_str = match &input.operation {
        EventOperation::Create => "create",
        EventOperation::Update => "update",
        EventOperation::Delete => "delete",
    };

    match runner.check_live_setting(
        live,
        &input.collection,
        op_str,
        &input.data,
        input.document_id.as_ref(),
        input.edited_by.as_ref(),
    ) {
        Ok(false) => return,
        Err(e) => {
            warn!("live setting check error for {}: {e}", input.collection);

            return;
        }
        Ok(true) => {}
    }

    let PublishEventInput {
        target,
        operation,
        collection,
        document_id,
        data,
        edited_by,
    } = input;

    let broadcast_data = match runner.run_before_broadcast(
        hooks,
        &collection,
        op_str,
        data,
        document_id.as_ref(),
        edited_by.as_ref(),
    ) {
        Ok(Some(d)) => d,
        Ok(None) => return,
        Err(e) => {
            warn!("before_broadcast hook error for {collection}: {e}");

            return;
        }
    };

    let transport_input = PublishEventInput {
        target,
        operation,
        collection,
        document_id,
        data: broadcast_data,
        edited_by,
    }
    .into_transport_input();

    transport.publish(transport_input);
}
