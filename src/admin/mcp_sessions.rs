//! MCP HTTP session tracking (`Mcp-Session-Id`) — per-session client
//! identity for audit logs.
//!
//! The stdio transport keeps one long-lived `McpServer`, so the client name
//! from `initialize` naturally sticks for the whole session. The HTTP
//! transport builds a fresh `McpServer` per request — without session
//! tracking every audit line falls back to `[client=(http)]`. This map
//! implements the MCP spec's `Mcp-Session-Id` header: `initialize` stores
//! the announced client name under a fresh id returned to the client; later
//! requests echo the header and get their `McpServer` pre-populated.
//!
//! Sessions are identity-for-audit only — the API key still authenticates
//! every request. A missing/unknown/expired id is never an error; the audit
//! label just falls back to `(http)`.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Idle lifetime: a session untouched for this long is evicted lazily.
const IDLE_TTL: Duration = Duration::from_mins(30);

/// Hard cap on tracked sessions; the oldest is evicted past this. Bounds
/// memory against clients that re-`initialize` forever.
const MAX_SESSIONS: usize = 1024;

struct McpSession {
    client_name: String,
    last_seen: Instant,
}

/// Session store for the HTTP MCP transport. A single `Mutex` (not
/// `RwLock`): every lookup also touches `last_seen`, so reads are writes
/// anyway — and MCP HTTP traffic is low-volume operator tooling.
#[derive(Default)]
pub struct McpSessions {
    inner: Mutex<HashMap<String, McpSession>>,
}

impl McpSessions {
    /// Store a fresh session for `client_name`, returning its id. Evicts
    /// expired sessions first, then the least-recently-seen one if the map
    /// is still at [`MAX_SESSIONS`].
    ///
    /// # Panics
    ///
    /// Panics if the session mutex is poisoned (a thread panicked while
    /// holding it — unrecoverable state).
    pub fn insert(&self, client_name: &str) -> String {
        let id = nanoid::nanoid!(21);
        let mut inner = self.inner.lock().expect("mcp session lock");

        let now = Instant::now();
        inner.retain(|_, s| now.duration_since(s.last_seen) < IDLE_TTL);

        if inner.len() >= MAX_SESSIONS
            && let Some(oldest) = inner
                .iter()
                .min_by_key(|(_, s)| s.last_seen)
                .map(|(k, _)| k.clone())
        {
            inner.remove(&oldest);
        }

        inner.insert(
            id.clone(),
            McpSession {
                client_name: client_name.to_string(),
                last_seen: now,
            },
        );

        id
    }

    /// Resolve a session id to its client name, refreshing `last_seen`.
    /// `None` for unknown or expired ids (expired ones are removed).
    ///
    /// # Panics
    ///
    /// Panics if the session mutex is poisoned.
    pub fn lookup_touch(&self, id: &str) -> Option<String> {
        let mut inner = self.inner.lock().expect("mcp session lock");

        let expired = inner
            .get(id)
            .is_some_and(|s| s.last_seen.elapsed() >= IDLE_TTL);
        if expired {
            inner.remove(id);
            return None;
        }

        let session = inner.get_mut(id)?;
        session.last_seen = Instant::now();

        Some(session.client_name.clone())
    }

    /// Terminate a session (MCP `DELETE` on the transport endpoint).
    /// Returns whether it existed.
    ///
    /// # Panics
    ///
    /// Panics if the session mutex is poisoned.
    pub fn remove(&self, id: &str) -> bool {
        self.inner
            .lock()
            .expect("mcp session lock")
            .remove(id)
            .is_some()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_lookup_round_trip() {
        let sessions = McpSessions::default();
        let id = sessions.insert("Claude Code");

        assert_eq!(sessions.lookup_touch(&id).as_deref(), Some("Claude Code"));
        assert!(sessions.lookup_touch("unknown").is_none());
    }

    #[test]
    fn remove_terminates() {
        let sessions = McpSessions::default();
        let id = sessions.insert("c");

        assert!(sessions.remove(&id));
        assert!(!sessions.remove(&id), "second delete finds nothing");
        assert!(sessions.lookup_touch(&id).is_none());
    }

    #[test]
    fn cap_evicts_oldest() {
        let sessions = McpSessions::default();
        let first = sessions.insert("first");

        for i in 0..MAX_SESSIONS {
            sessions.insert(&format!("c{i}"));
        }

        assert!(sessions.len() <= MAX_SESSIONS);
        assert!(
            sessions.lookup_touch(&first).is_none(),
            "the least-recently-seen session is the one evicted"
        );
    }

    #[test]
    fn ids_are_unique_and_opaque() {
        let sessions = McpSessions::default();
        let a = sessions.insert("x");
        let b = sessions.insert("x");
        assert_ne!(a, b);
        assert_eq!(a.len(), 21);
    }
}
