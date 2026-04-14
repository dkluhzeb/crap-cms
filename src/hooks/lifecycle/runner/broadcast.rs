//! HookRunner methods for event broadcasting.

use std::collections::HashMap;

use anyhow::Result;
use mlua::Value;
use serde_json::Value as JsonValue;
use tracing::{debug, warn};

use crate::{
    core::{
        DocumentId, Slug,
        collection::{Hooks, LiveSetting},
        event::{EventOperation, EventTarget, EventUser, MutationEventInput, SharedEventTransport},
    },
    hooks::{
        HookContext, HookEvent, HookRunner, api,
        lifecycle::execution::{
            call_before_broadcast_hook, call_registered_before_broadcast, get_hook_refs,
            resolve_hook_function,
        },
    },
};

/// Bundled parameters for a mutation event to publish.
pub struct PublishEventInput {
    pub target: EventTarget,
    pub operation: EventOperation,
    pub collection: Slug,
    pub document_id: DocumentId,
    pub data: HashMap<String, JsonValue>,
    pub edited_by: Option<EventUser>,
}

impl PublishEventInput {
    /// Create a builder with the required target and operation.
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
    data: HashMap<String, JsonValue>,
    edited_by: Option<EventUser>,
}

impl PublishEventInputBuilder {
    pub(crate) fn new(target: EventTarget, operation: EventOperation) -> Self {
        Self {
            target,
            operation,
            collection: None,
            document_id: None,
            data: HashMap::new(),
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

    pub fn data(mut self, data: HashMap<String, JsonValue>) -> Self {
        self.data = data;
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
    /// Run before_broadcast hooks. Returns Ok(Some(data)) to broadcast (possibly
    /// with transformed data), or Ok(None) to suppress the event.
    /// No CRUD access (fires after commit, same as after_change).
    pub fn run_before_broadcast(
        &self,
        hooks: &Hooks,
        collection: &str,
        operation: &str,
        data: HashMap<String, JsonValue>,
    ) -> Result<Option<HashMap<String, JsonValue>>> {
        let hook_refs = get_hook_refs(hooks, &HookEvent::BeforeBroadcast);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for("before_broadcast") {
            return Ok(Some(data));
        }

        let mut ctx = HookContext::builder(collection, operation)
            .data(data)
            .build();

        let lua = self.pool.acquire()?;

        // Run collection-level hook refs first
        for hook_ref in hook_refs {
            debug!(
                "Running before_broadcast hook: {} for {}",
                hook_ref, ctx.collection
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
    pub fn check_live_setting(
        &self,
        live: Option<&LiveSetting>,
        collection: &str,
        operation: &str,
        data: &HashMap<String, JsonValue>,
    ) -> Result<bool> {
        match live {
            None => Ok(true), // absent = broadcast all
            Some(LiveSetting::Disabled) => Ok(false),
            Some(LiveSetting::Function(func_ref)) => {
                let lua = self.pool.acquire()?;

                let func = resolve_hook_function(&lua, func_ref)?;

                let ctx_table = lua.create_table()?;
                ctx_table.set("collection", collection)?;
                ctx_table.set("operation", operation)?;
                let data_table = lua.create_table()?;
                for (k, v) in data {
                    data_table.set(k.as_str(), api::json_to_lua(&lua, v)?)?;
                }
                ctx_table.set("data", data_table)?;

                let result: Value = func.call(ctx_table)?;
                match result {
                    Value::Boolean(b) => Ok(b),
                    Value::Nil => Ok(false),
                    _ => Ok(true),
                }
            }
        }
    }

    /// Publish a mutation event: check live setting → run before_broadcast hooks → transport.publish().
    /// Spawns into a background task (non-blocking, like fire_after_event).
    /// Untestable: spawns tokio::task::spawn_blocking for async event dispatch.
    #[cfg(not(tarpaulin_include))]
    pub fn publish_event(
        &self,
        event_transport: &Option<SharedEventTransport>,
        hooks: &Hooks,
        live_setting: Option<&LiveSetting>,
        input: PublishEventInput,
    ) {
        let transport = match event_transport {
            Some(t) => t.clone(),
            None => return,
        };

        tokio::task::spawn_blocking({
            let runner = self.clone();
            let hooks = hooks.clone();
            let live = live_setting.cloned();
            move || publish_event_blocking(runner, transport, hooks, live, input)
        });
    }
}

/// Background worker for [`HookRunner::publish_event`]:
/// check live setting → run before_broadcast hooks → transport.publish().
fn publish_event_blocking(
    runner: HookRunner,
    transport: SharedEventTransport,
    hooks: Hooks,
    live: Option<LiveSetting>,
    input: PublishEventInput,
) {
    let op_str = match &input.operation {
        EventOperation::Create => "create",
        EventOperation::Update => "update",
        EventOperation::Delete => "delete",
    };

    match runner.check_live_setting(live.as_ref(), &input.collection, op_str, &input.data) {
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

    let broadcast_data = match runner.run_before_broadcast(&hooks, &collection, op_str, data) {
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
