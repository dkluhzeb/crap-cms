//! Redis pub/sub transports for cross-node event and invalidation fanout.
//!
//! Publish: JSON-encode the payload and `PUBLISH` it to a channel.
//!
//! Subscribe: spawn a background task that holds a `redis::aio::PubSub` and
//! forwards decoded messages to a bounded mpsc channel. If the local queue is
//! full, a `Lagged` sentinel is sent so the subscriber sees the same
//! "lagged — dropping" semantic as the in-process broadcast.
//!
//! The background task reconnects with exponential backoff on disconnect and
//! logs errors at `error!` / successful reconnects at `info!`.

use std::time::Duration;

use anyhow::{Context, Result};
use redis::{Client, aio::PubSub};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_stream::StreamExt;
use tracing::{debug, error, info, warn};

use crate::core::DocumentFields;
use crate::core::event::{
    EventReceiver, EventTransport, InvalidationReceiver, InvalidationTransport, MutationEvent,
    MutationEventInput, RemoteMessage, SequenceGen, event_channel, invalidation_channel,
};

/// Ceiling for one published mutation-event payload.
///
/// Every subscriber on every node receives a copy of what is published, so a
/// single outsized document would be fanned out across the whole cluster.
/// 512 KiB is far above any ordinary document and far below the size at which
/// that fanout becomes a problem.
const MAX_EVENT_PAYLOAD_BYTES: usize = 512 * 1024;

/// Local mpsc buffer capacity fed from the Redis pub/sub pump.
/// Matches the default in-process broadcast channel capacity.
const DEFAULT_MPSC_CAPACITY: usize = 1024;

/// Smaller buffer for the invalidation channel — signalling is low-volume.
const INVALIDATION_MPSC_CAPACITY: usize = 64;

/// Reconnect backoff bounds.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Redis transport for mutation events.
///
/// Publishes are blocking (the `Client::get_connection` path); they run from
/// within the `spawn_blocking` context used by `HookRunner::publish_event` so
/// blocking is expected.
pub struct RedisEventTransport {
    client: Client,
    channel: String,
    sequence: SequenceGen,
    mpsc_capacity: usize,
}

impl RedisEventTransport {
    /// Create a new Redis event transport. Validates connectivity on creation.
    ///
    /// `channel_prefix` is `[live] channel_prefix`; the publisher and the
    /// subscriber both derive their channel from it through
    /// [`event_channel`], so they cannot address different channels.
    pub fn new(url: &str, channel_prefix: &str) -> Result<Self> {
        let client = Client::open(url).context("Failed to create Redis client for events")?;

        // Validate connectivity with a PING on a sync connection.
        let mut conn = client
            .get_connection()
            .context("Failed to connect to Redis for events")?;

        redis::cmd("PING")
            .query::<String>(&mut conn)
            .context("Redis PING failed (events)")?;

        Ok(Self {
            client,
            channel: event_channel(channel_prefix),
            sequence: SequenceGen::new(),
            mpsc_capacity: DEFAULT_MPSC_CAPACITY,
        })
    }
}

impl EventTransport for RedisEventTransport {
    fn publish(&self, input: MutationEventInput) -> Option<MutationEvent> {
        let event = self.sequence.stamp(input);

        let body = match encode_event(&event) {
            Ok(body) => body,
            Err(e) => {
                error!("Redis event encode failed: {:#}", e);
                return None;
            }
        };

        if let Err(e) = publish_body(&self.client, &self.channel, &body) {
            error!("Redis event publish failed: {:#}", e);
            return None;
        }

        Some(event)
    }

    fn subscribe(&self) -> EventReceiver {
        let (tx, rx) = mpsc::channel::<RemoteMessage<MutationEvent>>(self.mpsc_capacity);
        spawn_subscribe_loop(self.client.clone(), self.channel.clone(), tx, false);

        EventReceiver::from_mpsc(rx)
    }

    fn kind(&self) -> &'static str {
        "redis"
    }
}

/// Redis transport for user-invalidation signals.
pub struct RedisInvalidationTransport {
    client: Client,
    channel: String,
    mpsc_capacity: usize,
}

impl RedisInvalidationTransport {
    /// Create a new Redis invalidation transport, deriving its channel from
    /// `[live] channel_prefix` the same way the event transport does.
    ///
    /// # Errors
    ///
    /// Returns an error if the client cannot be built or the `PING` fails.
    pub fn new(url: &str, channel_prefix: &str) -> Result<Self> {
        let client = Client::open(url).context("Failed to create Redis client for invalidation")?;

        let mut conn = client
            .get_connection()
            .context("Failed to connect to Redis for invalidation")?;

        redis::cmd("PING")
            .query::<String>(&mut conn)
            .context("Redis PING failed (invalidation)")?;

        Ok(Self {
            client,
            channel: invalidation_channel(channel_prefix),
            mpsc_capacity: INVALIDATION_MPSC_CAPACITY,
        })
    }
}

impl InvalidationTransport for RedisInvalidationTransport {
    fn publish(&self, user_id: String) {
        if let Err(e) = publish_blocking(&self.client, &self.channel, &user_id) {
            error!("Redis invalidation publish failed: {:#}", e);
        }
    }

    fn subscribe(&self) -> InvalidationReceiver {
        let (tx, rx) = mpsc::channel::<RemoteMessage<String>>(self.mpsc_capacity);
        spawn_subscribe_loop(self.client.clone(), self.channel.clone(), tx, true);

        InvalidationReceiver::from_mpsc(rx)
    }

    fn kind(&self) -> &'static str {
        "redis"
    }
}

/// Encode one mutation event for the wire, downgrading an oversized `full`
/// payload to the metadata-only form.
///
/// A `full`-mode collection puts the whole document on the wire, and every
/// subscriber on every node gets a copy. Past the cap the document data is
/// dropped and the event published without it, so a subscriber still learns
/// that the document changed — exactly what a metadata-mode event carries —
/// instead of the cluster fanning out a payload of unbounded size.
fn encode_event(event: &MutationEvent) -> Result<String> {
    let body = encode_payload(event)?;

    if body.len() <= MAX_EVENT_PAYLOAD_BYTES {
        return Ok(body);
    }

    warn!(
        collection = %event.collection,
        document_id = %event.document_id,
        bytes = body.len(),
        limit = MAX_EVENT_PAYLOAD_BYTES,
        "Mutation event payload is over the publish limit -- publishing it without document data"
    );

    let mut stripped = event.clone();
    stripped.data = DocumentFields::new();

    encode_payload(&stripped)
}

/// JSON for one pub/sub payload — the single encoder both channels use.
fn encode_payload<T: Serialize>(payload: &T) -> Result<String> {
    serde_json::to_string(payload).context("Failed to encode pub/sub payload as JSON")
}

/// JSON-encode `payload` and PUBLISH it to `channel`.
fn publish_blocking<T: Serialize>(client: &Client, channel: &str, payload: &T) -> Result<()> {
    publish_body(client, channel, &encode_payload(payload)?)
}

/// PUBLISH an already-encoded body to `channel` on a fresh connection.
/// Fresh connections are fine: publishes are relatively rare, and this avoids
/// needing to synchronize a shared mutable connection across threads.
fn publish_body(client: &Client, channel: &str, body: &str) -> Result<()> {
    let mut conn = client
        .get_connection()
        .context("Failed to acquire Redis connection for publish")?;

    redis::cmd("PUBLISH")
        .arg(channel)
        .arg(body)
        .query::<i64>(&mut conn)
        .context("Redis PUBLISH failed")?;

    Ok(())
}

/// Spawn a background task that reads `channel` over Redis pub/sub and
/// Why a [`pump_messages`] run ended.
enum PumpOutcome {
    /// The connection broke mid-stream — reconnect after backoff.
    Disconnected,
    /// Stop the pump for good (the receiver was dropped, or — on an
    /// `evict_on_overflow` channel — an overflow couldn't even be signalled, so
    /// the subscriber is evicted fail-closed by dropping `tx` → receiver sees
    /// `Closed`).
    Stop,
}

/// forwards decoded `T` values into `tx`. On overflow sends `Lagged`;
/// `evict_on_overflow` makes an undeliverable overflow terminate the pump
/// (fail-closed) instead of best-effort dropping — used for the invalidation
/// channel, where a lost message is a lost revocation. Reconnects with
/// exponential backoff on connection failure.
fn spawn_subscribe_loop<T>(
    client: Client,
    channel: String,
    tx: mpsc::Sender<RemoteMessage<T>>,
    evict_on_overflow: bool,
) -> JoinHandle<()>
where
    T: DeserializeOwned + Clone + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = INITIAL_BACKOFF;
        let mut first_connect = true;

        loop {
            if tx.is_closed() {
                debug!("Redis pub/sub subscriber dropped; ending pump for {channel}");
                return;
            }

            match connect_pubsub(&client, &channel).await {
                Ok(pubsub) => {
                    if !first_connect {
                        info!("Reconnected to Redis pub/sub channel {}", channel);
                    }

                    first_connect = false;
                    backoff = INITIAL_BACKOFF;

                    match pump_messages(pubsub, &tx, evict_on_overflow).await {
                        // `tx` drops when this task returns → receiver sees `Closed`.
                        PumpOutcome::Stop => return,
                        // Connection broke mid-stream; reconnect after backoff.
                        PumpOutcome::Disconnected => {}
                    }
                }
                Err(e) => {
                    error!(
                        "Redis pub/sub connect failed for {} (retrying in {:?}): {:#}",
                        channel, backoff, e
                    );
                }
            }

            sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    })
}

/// Open a pub/sub connection and SUBSCRIBE to `channel`.
async fn connect_pubsub(client: &Client, channel: &str) -> Result<PubSub> {
    let mut pubsub = client
        .get_async_pubsub()
        .await
        .context("Failed to open async Redis pub/sub connection")?;

    pubsub
        .subscribe(channel)
        .await
        .context("Failed to SUBSCRIBE to Redis channel")?;

    Ok(pubsub)
}

/// Read messages from `pubsub` and forward them to `tx`, returning why the run
/// ended (see [`PumpOutcome`]).
async fn pump_messages<T>(
    pubsub: PubSub,
    tx: &mpsc::Sender<RemoteMessage<T>>,
    evict_on_overflow: bool,
) -> PumpOutcome
where
    T: DeserializeOwned + Clone + Send + 'static,
{
    let mut stream = Box::pin(pubsub.into_on_message());

    while let Some(msg) = stream.next().await {
        let payload: String = match msg.get_payload() {
            Ok(p) => p,
            Err(e) => {
                warn!("Skipping Redis pub/sub message with bad payload: {}", e);
                continue;
            }
        };

        let decoded: T = match serde_json::from_str(&payload) {
            Ok(v) => v,
            Err(e) => {
                warn!("Dropping undecodable Redis pub/sub message: {}", e);
                continue;
            }
        };

        if let Err(dropped_n) = try_forward(tx, decoded).await {
            // Local queue full — signal Lagged via `try_send` so we don't block
            // the pump on a slow reader. If the sentinel ALSO can't be enqueued
            // (the queue is still full), the subscriber never observes the
            // overflow: on a best-effort channel that just loses data, but on an
            // `evict_on_overflow` (invalidation) channel a silently-dropped
            // message is a silently-dropped REVOCATION — fail-open. So there we
            // terminate the pump (dropping `tx` → receiver sees `Closed`) to tear
            // the subscriber down fail-closed, rather than keep streaming to a
            // session whose revocation we just lost.
            if tx.try_send(RemoteMessage::Lagged(dropped_n)).is_err() {
                if evict_on_overflow {
                    warn!(
                        "Redis invalidation queue full and Lagged undeliverable — \
                         evicting subscriber (fail-closed)"
                    );
                    return PumpOutcome::Stop;
                }
                warn!(
                    "Redis pub/sub subscriber queue full — dropped a message \
                     (Lagged sentinel also undeliverable)"
                );
            } else {
                warn!(
                    "Redis pub/sub subscriber queue full — dropped 1 message and \
                     signalled Lagged to subscriber"
                );
            }
        }

        if tx.is_closed() {
            return PumpOutcome::Stop;
        }
    }

    PumpOutcome::Disconnected
}

/// Try to forward a decoded event to the subscriber with a short timeout so
/// we don't block the pump indefinitely. Returns `Err(1)` if the queue is
/// backed up — caller converts that into a Lagged signal.
async fn try_forward<T>(tx: &mpsc::Sender<RemoteMessage<T>>, value: T) -> Result<(), u64>
where
    T: Clone + Send + 'static,
{
    // Both error arms collapse to `Err(1)` — the channel is unusable
    // for this caller whether the receiver dropped (Ok(Err(_))) or the
    // send timed out because the queue was full (Err(_) from timeout).
    // Caller treats either as "subscriber too slow / gone, evict."
    match timeout(
        Duration::from_millis(50),
        tx.send(RemoteMessage::Event(value)),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(1),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::event::sequence::stamp_event;
    use crate::core::event::{EventOperation, EventTarget};
    use crate::core::{DocumentId, Slug};

    use super::*;

    #[test]
    fn mutation_event_json_wire_format_is_stable() {
        // The Redis transport publishes events as serde_json strings. This
        // pins down the wire format so accidental changes are caught.
        let event = stamp_event(
            MutationEventInput {
                target: EventTarget::Collection,
                operation: EventOperation::Create,
                collection: Slug::new("posts"),
                document_id: DocumentId::new("doc1"),
                data: DocumentFields::new(),
                edited_by: None,
                view: crate::core::EventViewMeta::default(),
            },
            1,
            "node-a",
        );

        let json = serde_json::to_string(&event).expect("encode");
        let back: MutationEvent = serde_json::from_str(&json).expect("decode");
        assert_eq!(back.sequence, event.sequence);
        assert_eq!(back.publisher, "node-a");
        assert_eq!(back.target, EventTarget::Collection);
        assert_eq!(back.operation, EventOperation::Create);
    }

    /// Build a stamped event carrying `bytes` worth of document data.
    fn event_with_payload(bytes: usize) -> MutationEvent {
        let mut data = DocumentFields::new();
        data.insert("body".to_string(), json!("x".repeat(bytes)));

        stamp_event(
            MutationEventInput {
                target: EventTarget::Collection,
                operation: EventOperation::Update,
                collection: Slug::new("posts"),
                document_id: DocumentId::new("doc1"),
                data,
                edited_by: None,
                view: crate::core::EventViewMeta::default(),
            },
            1,
            "node-a",
        )
    }

    /// Regression: a `full`-mode event was published whatever its size, so one
    /// outsized document was fanned out to every subscriber on every node.
    /// Over the cap the data is dropped and the event still published, so a
    /// subscriber learns the document changed.
    #[test]
    fn an_oversized_payload_is_published_without_its_document_data() {
        let event = event_with_payload(MAX_EVENT_PAYLOAD_BYTES + 1);

        let body = encode_event(&event).expect("encode");
        assert!(body.len() < MAX_EVENT_PAYLOAD_BYTES, "{}", body.len());

        let decoded: MutationEvent = serde_json::from_str(&body).expect("decode");
        assert!(decoded.data.is_empty());
        assert_eq!(decoded.document_id, event.document_id);
        assert_eq!(decoded.operation, EventOperation::Update);
        assert_eq!(decoded.sequence, event.sequence);
    }

    /// A payload within the cap keeps its data — the common case must be
    /// untouched.
    #[test]
    fn a_payload_within_the_cap_keeps_its_document_data() {
        let event = event_with_payload(1024);

        let body = encode_event(&event).expect("encode");
        let decoded: MutationEvent = serde_json::from_str(&body).expect("decode");

        assert_eq!(decoded.data.get_str("body"), event.data.get_str("body"));
    }

    #[test]
    fn invalidation_string_json_wire_format() {
        let payload = "user-abc".to_string();
        let encoded = serde_json::to_string(&payload).unwrap();
        let decoded: String = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, "user-abc");
    }
}
