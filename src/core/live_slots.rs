//! Slots for long-lived live-update streams (admin SSE, gRPC `Subscribe`):
//! a cap on all open streams and a cap per client.
//!
//! Both surfaces acquire through [`LiveSlots::try_acquire`], keyed by
//! [`LiveSlots::client_key`], so one principal — or one anonymous address —
//! can never take every slot the way a single global counter allowed.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use crate::core::ClientIp;

/// Open-stream counts: the total and per client key.
#[derive(Default)]
struct SlotCounts {
    total: usize,
    per_client: HashMap<String, usize>,
}

/// Why a stream was refused a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRefusal {
    /// Every slot of the surface is taken.
    AllTaken,
    /// This client already holds its share.
    ClientLimit,
}

/// The slot table of one live-update surface. Cheap to clone — clones share
/// the table.
#[derive(Clone)]
pub struct LiveSlots {
    counts: Arc<Mutex<SlotCounts>>,
    limits: SlotLimits,
}

/// The two caps; `0` means unlimited.
#[derive(Debug, Clone, Copy)]
struct SlotLimits {
    total: usize,
    per_client: usize,
}

impl LiveSlots {
    /// A table allowing `max_total` streams in all and `max_per_client` per
    /// client; `0` leaves that cap off.
    #[must_use]
    pub fn new(max_total: usize, max_per_client: usize) -> Self {
        Self {
            counts: Arc::default(),
            limits: SlotLimits {
                total: max_total,
                per_client: max_per_client,
            },
        }
    }

    /// The key a subscriber's streams are counted under: the authenticated
    /// user when there is one (whatever address it connects from), otherwise
    /// the client address's rate-limit bucket (an IPv6 client counts per /64).
    #[must_use]
    pub fn client_key(user_id: Option<&str>, client: &ClientIp) -> String {
        match user_id {
            Some(id) => format!("user:{id}"),
            None => format!("ip:{}", client.rate_limit_key()),
        }
    }

    /// Take a slot for `client_key`, released when the returned guard drops.
    ///
    /// # Errors
    ///
    /// Returns the [`SlotRefusal`] naming the cap that is reached.
    pub fn try_acquire(&self, client_key: &str) -> Result<LiveSlot, SlotRefusal> {
        let mut counts = lock(&self.counts);

        if self.limits.total > 0 && counts.total >= self.limits.total {
            return Err(SlotRefusal::AllTaken);
        }

        let held = counts.per_client.get(client_key).copied().unwrap_or(0);
        if self.limits.per_client > 0 && held >= self.limits.per_client {
            return Err(SlotRefusal::ClientLimit);
        }

        counts.total += 1;
        counts.per_client.insert(client_key.to_string(), held + 1);

        Ok(LiveSlot {
            counts: Arc::clone(&self.counts),
            client_key: client_key.to_string(),
        })
    }

    /// Streams currently open.
    #[must_use]
    pub fn open(&self) -> usize {
        lock(&self.counts).total
    }
}

/// One held slot; dropping it frees the slot.
pub struct LiveSlot {
    counts: Arc<Mutex<SlotCounts>>,
    client_key: String,
}

impl Drop for LiveSlot {
    fn drop(&mut self) {
        let mut counts = lock(&self.counts);

        counts.total = counts.total.saturating_sub(1);

        let Some(held) = counts.per_client.get_mut(&self.client_key) else {
            return;
        };

        *held = held.saturating_sub(1);
        if *held == 0 {
            counts.per_client.remove(&self.client_key);
        }
    }
}

/// Lock the table; a panic elsewhere never leaves it unusable (the counts are
/// plain integers, always consistent between statements).
fn lock(counts: &Mutex<SlotCounts>) -> MutexGuard<'_, SlotCounts> {
    counts.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> ClientIp {
        ClientIp::new(s.parse().unwrap())
    }

    #[test]
    fn the_total_cap_refuses_once_every_slot_is_taken() {
        let slots = LiveSlots::new(2, 0);

        let _a = slots.try_acquire("a").unwrap();
        let _b = slots.try_acquire("b").unwrap();

        assert_eq!(slots.try_acquire("c").err(), Some(SlotRefusal::AllTaken));
    }

    /// Regression: the slots were one global pool, so one client could open
    /// every stream and lock every other subscriber out.
    #[test]
    fn one_client_cannot_take_more_than_its_share() {
        let slots = LiveSlots::new(100, 2);

        let _first = slots.try_acquire("ip:203.0.113.5").unwrap();
        let _second = slots.try_acquire("ip:203.0.113.5").unwrap();

        assert_eq!(
            slots.try_acquire("ip:203.0.113.5").err(),
            Some(SlotRefusal::ClientLimit)
        );
        assert!(slots.try_acquire("ip:203.0.113.6").is_ok());
    }

    #[test]
    fn dropping_a_slot_frees_it_for_its_client_and_the_total() {
        let slots = LiveSlots::new(1, 1);

        let held = slots.try_acquire("a").unwrap();
        assert!(slots.try_acquire("a").is_err());

        drop(held);

        assert_eq!(slots.open(), 0);
        assert!(slots.try_acquire("a").is_ok());
    }

    #[test]
    fn zero_caps_are_unlimited() {
        let slots = LiveSlots::new(0, 0);

        let held: Vec<_> = (0..50).map(|_| slots.try_acquire("a").unwrap()).collect();

        assert_eq!(slots.open(), held.len());
    }

    #[test]
    fn users_are_keyed_by_id_and_anonymous_clients_by_address_bucket() {
        let from = ip("2001:db8:1:2::7");

        assert_eq!(LiveSlots::client_key(Some("u1"), &from), "user:u1");
        assert_eq!(
            LiveSlots::client_key(None, &from),
            "ip:2001:db8:1:2::/64",
            "an IPv6 client shares one key across its /64"
        );
    }
}
