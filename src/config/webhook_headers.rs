//! `WebhookHeaders` — operator-supplied HTTP headers for the webhook
//! email provider, with value redaction.
//!
//! These headers routinely carry credentials (`Authorization: Bearer …`,
//! `X-Api-Key: …`). The real values are reachable only through
//! [`WebhookHeaders::to_map`] (the send path); `Debug` and `Serialize`
//! (the Lua-facing config exposure) show every value as `***`.

use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;

/// Webhook header map whose values never escape through `Debug` or
/// `Serialize`.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "HashMap<String, String>")]
pub struct WebhookHeaders(HashMap<String, String>);

impl WebhookHeaders {
    /// The real header map, for the send path only.
    #[must_use]
    pub fn to_map(&self) -> HashMap<String, String> {
        self.0.clone()
    }

    /// Whether any headers are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<HashMap<String, String>> for WebhookHeaders {
    fn from(map: HashMap<String, String>) -> Self {
        Self(map)
    }
}

impl fmt::Debug for WebhookHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&String> = self.0.keys().collect();
        names.sort();

        f.debug_map()
            .entries(names.into_iter().map(|k| (k, "***")))
            .finish()
    }
}

impl Serialize for WebhookHeaders {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut names: Vec<&String> = self.0.keys().collect();
        names.sort();

        let mut map = serializer.serialize_map(Some(names.len()))?;
        for name in names {
            map.serialize_entry(name, "***")?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WebhookHeaders {
        let mut m = HashMap::new();
        m.insert("Authorization".to_string(), "Bearer hunter2".to_string());
        WebhookHeaders::from(m)
    }

    #[test]
    fn values_masked_in_debug_and_serialize() {
        let h = sample();

        let debug = format!("{h:?}");
        assert!(
            debug.contains("Authorization"),
            "names stay visible: {debug}"
        );
        assert!(!debug.contains("hunter2"), "values must be masked: {debug}");

        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("Authorization"));
        assert!(!json.contains("hunter2"));
    }

    #[test]
    fn send_path_gets_real_values() {
        assert_eq!(
            sample().to_map().get("Authorization").map(String::as_str),
            Some("Bearer hunter2")
        );
    }
}
