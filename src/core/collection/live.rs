//! Live event broadcasting configuration: per-collection enable/disable
//! ([`LiveSetting`]) and event-payload mode ([`LiveMode`]).

use serde::{Deserialize, Serialize};

/// Controls live event broadcasting for a collection or global.
/// `None` = enabled (broadcast all events).
/// `Some(LiveSetting::Disabled)` = never broadcast.
/// `Some(LiveSetting::Function(ref))` = Lua function decides per-event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LiveSetting {
    /// Disable all live event broadcasting for this collection/global.
    Disabled,
    /// Use a Lua function to determine if an event should be broadcast.
    Function(String),
}

/// Controls what data events carry for a collection or global.
///
/// - `Metadata` (default): events carry only sequence, timestamp, target, operation,
///   collection, `document_id`, `edited_by`. No document data, no `after_read` hooks.
///   Client re-fetches via `FindByID` if needed. Safe, fast, zero hook overhead.
/// - `Full`: events carry full document data through `after_read` hooks + field-level
///   stripping. Opt-in per collection for real-time data delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LiveMode {
    /// Metadata only — no document data, no `after_read` hooks (default).
    #[default]
    Metadata,
    /// Full data — runs `after_read` hooks, includes document data with field stripping.
    Full,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_mode_defaults_to_metadata() {
        // Safety invariant: the default must stay Metadata. Full would run
        // after_read hooks and broadcast document data on every event.
        assert_eq!(LiveMode::default(), LiveMode::Metadata);
    }

    #[test]
    fn live_mode_serde_round_trips() {
        for mode in [LiveMode::Metadata, LiveMode::Full] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: LiveMode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, mode);
        }
    }
}
