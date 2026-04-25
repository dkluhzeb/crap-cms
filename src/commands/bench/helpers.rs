//! Shared utilities for bench subcommands: data resolution, synthetic generation, timing.

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{CollectionDefinition, FieldDefinition, FieldType},
    db::{DbConnection, FindQuery, query},
};

/// Where the benchmark data came from.
#[derive(Debug, Clone, Copy)]
pub enum DataSource {
    UserProvided,
    ExistingDocument,
    Synthetic,
}

impl DataSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::UserProvided => "user JSON",
            Self::ExistingDocument => "existing document",
            Self::Synthetic => "synthetic data",
        }
    }
}

/// Resolve benchmark data for a collection. Priority: user JSON > existing doc > synthetic.
pub fn resolve_bench_data(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    user_data: Option<&str>,
) -> Result<(HashMap<String, Value>, DataSource)> {
    // 1. User-provided JSON
    if let Some(json_str) = user_data {
        let val: Value = serde_json::from_str(json_str)?;
        let map = match val {
            Value::Object(m) => m.into_iter().collect(),
            _ => anyhow::bail!("--data must be a JSON object"),
        };
        return Ok((map, DataSource::UserProvided));
    }

    // 2. Existing document from DB
    let find_query = FindQuery::builder().limit(Some(1)).build();

    if let Ok(docs) = query::find(conn, slug, def, &find_query, None)
        && let Some(doc) = docs.first()
    {
        let mut data = doc.fields.clone();
        randomize_unique_fields(&mut data, &def.fields);
        return Ok((data, DataSource::ExistingDocument));
    }

    // 3. Synthetic fallback
    let mut data = generate_synthetic_data(&def.fields);
    randomize_unique_fields(&mut data, &def.fields);
    Ok((data, DataSource::Synthetic))
}

/// Convert `HashMap<String, Value>` to `HashMap<String, String>` for WriteInput.
pub fn to_string_map(data: &HashMap<String, Value>) -> HashMap<String, String> {
    data.iter()
        .filter_map(|(k, v)| {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
                Value::Null => return None,
                other => other.to_string(),
            };
            Some((k.clone(), s))
        })
        .collect()
}

/// Append a random suffix to unique fields so benchmarks don't hit uniqueness violations.
pub fn randomize_unique_fields(data: &mut HashMap<String, Value>, fields: &[FieldDefinition]) {
    for field in fields {
        if !field.unique {
            continue;
        }

        let Some(val) = data.get(&field.name) else {
            continue;
        };

        if let Value::String(s) = val {
            let suffix = nanoid::nanoid!(8);
            data.insert(
                field.name.clone(),
                Value::String(format!("{s}-bench-{suffix}")),
            );
        }
    }
}

/// Generate plausible synthetic data from a collection's field definitions.
fn generate_synthetic_data(fields: &[FieldDefinition]) -> HashMap<String, Value> {
    let mut data = HashMap::new();

    for field in fields {
        let value = match field.field_type {
            FieldType::Text | FieldType::Code => Value::String("sample text".into()),
            FieldType::Textarea => Value::String("sample textarea content".into()),
            FieldType::Richtext => Value::String("<p>sample content</p>".into()),
            FieldType::Number => Value::Number(serde_json::Number::from(42)),
            FieldType::Checkbox => Value::Number(serde_json::Number::from(0)),
            FieldType::Email => Value::String("bench@example.com".into()),
            FieldType::Date => Value::String("2026-01-01".into()),
            FieldType::Select | FieldType::Radio => {
                let val = field
                    .options
                    .first()
                    .map(|o| o.value.clone())
                    .unwrap_or_else(|| "option_a".into());
                Value::String(val)
            }
            FieldType::Json => Value::Object(Default::default()),
            // Complex types — skip (empty/null is valid for optional fields)
            FieldType::Relationship
            | FieldType::Upload
            | FieldType::Array
            | FieldType::Blocks
            | FieldType::Group
            | FieldType::Row
            | FieldType::Collapsible
            | FieldType::Tabs
            | FieldType::Join => continue,
        };

        data.insert(field.name.clone(), value);
    }

    data
}

/// Format a duration for display.
pub fn format_duration(d: Duration) -> String {
    if d.as_micros() < 1000 {
        format!("{}us", d.as_micros())
    } else if d.as_millis() < 1000 {
        format!("{:.1}ms", d.as_secs_f64() * 1000.0)
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

/// Compute min, avg, max from a slice of durations.
pub fn timing_stats(durations: &[Duration]) -> (Duration, Duration, Duration) {
    let min = durations.iter().copied().min().unwrap_or_default();
    let max = durations.iter().copied().max().unwrap_or_default();
    let sum: Duration = durations.iter().sum();
    let avg = sum / durations.len().max(1) as u32;
    (min, avg, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_microseconds() {
        assert_eq!(format_duration(Duration::from_micros(500)), "500us");
    }

    #[test]
    fn format_duration_milliseconds() {
        assert_eq!(format_duration(Duration::from_millis(42)), "42.0ms");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_secs(2)), "2.00s");
    }

    #[test]
    fn timing_stats_computes_correctly() {
        let durations = vec![
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(30),
        ];
        let (min, avg, max) = timing_stats(&durations);
        assert_eq!(min, Duration::from_millis(10));
        assert_eq!(avg, Duration::from_millis(20));
        assert_eq!(max, Duration::from_millis(30));
    }

    #[test]
    fn to_string_map_converts_values() {
        let mut data = HashMap::new();
        data.insert("name".into(), Value::String("test".into()));
        data.insert("count".into(), Value::Number(42.into()));
        data.insert("active".into(), Value::Bool(true));
        data.insert("empty".into(), Value::Null);

        let result = to_string_map(&data);
        assert_eq!(result.get("name").unwrap(), "test");
        assert_eq!(result.get("count").unwrap(), "42");
        assert_eq!(result.get("active").unwrap(), "1");
        assert!(!result.contains_key("empty"));
    }
}
