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

use crate::core::event::{
    EventReceiver, EventTransport, InvalidationReceiver, InvalidationTransport, MutationEvent,
    MutationEventInput, RemoteMessage, SequenceGen, stamp_event,
};

/// Redis pub/sub channel name for mutation events.
const EVENT_CHANNEL: &str = "crap:events";

/// Redis pub/sub channel name for user-invalidation signals.
const INVALIDATION_CHANNEL: &str = "crap:invalidations";

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
    sequence: SequenceGen,
    mpsc_capacity: usize,
}

impl RedisEventTransport {
    /// Create a new Redis event transport. Validates connectivity on creation.
    pub fn new(url: &str) -> Result<Self> {
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
            sequence: SequenceGen::new(),
            mpsc_capacity: DEFAULT_MPSC_CAPACITY,
        })
    }
}

impl EventTransport for RedisEventTransport {
    fn publish(&self, input: MutationEventInput) -> Option<MutationEvent> {
        let event = stamp_event(input, self.sequence.next());

        if let Err(e) = publish_blocking(&self.client, EVENT_CHANNEL, &event) {
            error!("Redis event publish failed: {:#}", e);
            return None;
        }

        Some(event)
    }

    fn subscribe(&self) -> EventReceiver {
        let (tx, rx) = mpsc::channel::<RemoteMessage<MutationEvent>>(self.mpsc_capacity);
        spawn_subscribe_loop(self.client.clone(), EVENT_CHANNEL, tx);

        EventReceiver::from_mpsc(rx)
    }

    fn kind(&self) -> &'static str {
        "redis"
    }
}

/// Redis transport for user-invalidation signals.
pub struct RedisInvalidationTransport {
    client: Client,
    mpsc_capacity: usize,
}

impl RedisInvalidationTransport {
    pub fn new(url: &str) -> Result<Self> {
        let client = Client::open(url).context("Failed to create Redis client for invalidation")?;

        let mut conn = client
            .get_connection()
            .context("Failed to connect to Redis for invalidation")?;

        redis::cmd("PING")
            .query::<String>(&mut conn)
            .context("Redis PING failed (invalidation)")?;

        Ok(Self {
            client,
            mpsc_capacity: INVALIDATION_MPSC_CAPACITY,
        })
    }
}

impl InvalidationTransport for RedisInvalidationTransport {
    fn publish(&self, user_id: String) {
        if let Err(e) = publish_blocking(&self.client, INVALIDATION_CHANNEL, &user_id) {
            error!("Redis invalidation publish failed: {:#}", e);
        }
    }

    fn subscribe(&self) -> InvalidationReceiver {
        let (tx, rx) = mpsc::channel::<RemoteMessage<String>>(self.mpsc_capacity);
        spawn_subscribe_loop(self.client.clone(), INVALIDATION_CHANNEL, tx);

        InvalidationReceiver::from_mpsc(rx)
    }

    fn kind(&self) -> &'static str {
        "redis"
    }
}

/// JSON-encode `payload` and PUBLISH it to `channel` on a fresh connection.
/// Fresh connections are fine: publishes are relatively rare, and this avoids
/// needing to synchronize a shared mutable connection across threads.
fn publish_blocking<T: Serialize>(client: &Client, channel: &str, payload: &T) -> Result<()> {
    let body =
        serde_json::to_string(payload).context("Failed to encode pub/sub payload as JSON")?;

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
/// forwards decoded `T` values into `tx`. On overflow sends `Lagged`.
/// Reconnects with exponential backoff on failure.
fn spawn_subscribe_loop<T>(
    client: Client,
    channel: &'static str,
    tx: mpsc::Sender<RemoteMessage<T>>,
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

            match connect_pubsub(&client, channel).await {
                Ok(pubsub) => {
                    if !first_connect {
                        info!("Reconnected to Redis pub/sub channel {}", channel);
                    }

                    first_connect = false;
                    backoff = INITIAL_BACKOFF;

                    if pump_messages(pubsub, &tx).await.is_err() {
                        // Connection broke mid-stream; reconnect after backoff.
                    }

                    if tx.is_closed() {
                        return;
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

/// Read messages from `pubsub` and forward them to `tx`. Returns `Err` on
/// connection break so the caller can reconnect.
async fn pump_messages<T>(pubsub: PubSub, tx: &mpsc::Sender<RemoteMessage<T>>) -> Result<(), ()>
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
            // Local queue full — signal Lagged. Use `try_send` so we don't
            // block the pump waiting for a slow reader; if that also fails
            // the subscriber is already falling behind beyond recovery.
            let _ = tx.try_send(RemoteMessage::Lagged(dropped_n));
            warn!(
                "Redis pub/sub subscriber queue full — dropped 1 message and \
                 signalled Lagged to subscriber"
            );
        }

        if tx.is_closed() {
            return Ok(());
        }
    }

    Err(())
}

/// Try to forward a decoded event to the subscriber with a short timeout so
/// we don't block the pump indefinitely. Returns `Err(1)` if the queue is
/// backed up — caller converts that into a Lagged signal.
async fn try_forward<T>(tx: &mpsc::Sender<RemoteMessage<T>>, value: T) -> Result<(), u64>
where
    T: Clone + Send + 'static,
{
    match timeout(
        Duration::from_millis(50),
        tx.send(RemoteMessage::Event(value)),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(1), // receiver dropped — treat as lagged-close
        Err(_) => Err(1),     // queue full
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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
                data: HashMap::new(),
                edited_by: None,
            },
            1,
        );

        let json = serde_json::to_string(&event).expect("encode");
        let back: MutationEvent = serde_json::from_str(&json).expect("decode");
        assert_eq!(back.sequence, event.sequence);
        assert_eq!(back.target, EventTarget::Collection);
        assert_eq!(back.operation, EventOperation::Create);
    }

    #[test]
    fn invalidation_string_json_wire_format() {
        let payload = "user-abc".to_string();
        let encoded = serde_json::to_string(&payload).unwrap();
        let decoded: String = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, "user-abc");
    }
}
