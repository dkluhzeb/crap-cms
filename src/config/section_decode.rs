//! Decoding one `crap.toml` section with every problem in it reported.
//!
//! A decode stops at the first unknown or mistyped key. To find the next one,
//! the failing key is taken out of the section and the decode runs again,
//! until it succeeds or the failure can't be pinned to a key. Every failure is
//! reported by its path in the file as written, so taking an array element out
//! doesn't shift the index the next problem in that array is reported under.
//! A failure the removal itself caused — a required key now missing from the
//! table it was taken out of — is not reported.

use serde::de::DeserializeOwned;
use serde_path_to_error::{Path as ErrorPath, Segment, deserialize};
use toml::Value;

use crate::config::ErrorReport;

/// One step of a path into a section.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

/// A path as `section.key[0].sub`.
fn render(section: &str, path: &[Step]) -> String {
    let mut out = section.to_string();

    for step in path {
        match step {
            Step::Key(key) => {
                out.push('.');
                out.push_str(key);
            }
            Step::Index(index) => {
                out.push('[');
                out.push_str(&index.to_string());
                out.push(']');
            }
        }
    }

    out
}

/// The failing value's path, `None` when a step can't be followed in a TOML
/// value (an enum variant, an unknown step).
fn steps(path: &ErrorPath) -> Option<Vec<Step>> {
    path.iter()
        .map(|segment| match segment {
            Segment::Map { key } => Some(Step::Key(key.clone())),
            Segment::Seq { index } => Some(Step::Index(*index)),
            Segment::Enum { .. } | Segment::Unknown => None,
        })
        .collect()
}

/// Take the value at `path` out of `value`. `false` when there is nothing
/// there to take.
fn remove_at(value: &mut Value, path: &[Step]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };

    let mut node = value;

    for step in parents {
        let next = match (node, step) {
            (Value::Table(table), Step::Key(key)) => table.get_mut(key),
            (Value::Array(items), Step::Index(index)) => items.get_mut(*index),
            _ => None,
        };

        let Some(next) = next else {
            return false;
        };
        node = next;
    }

    match (node, last) {
        (Value::Table(table), Step::Key(key)) => table.remove(key).is_some(),
        (Value::Array(items), Step::Index(index)) if *index < items.len() => {
            items.remove(*index);
            true
        }
        _ => false,
    }
}

/// The paths taken out of a section so far, as written in the file.
#[derive(Default)]
struct Removed {
    paths: Vec<Vec<Step>>,
}

impl Removed {
    /// The path as written of `current`, a path into the section as it is
    /// after the removals: an array index moves past every element taken out
    /// before it.
    fn as_written(&self, current: &[Step]) -> Vec<Step> {
        let mut written = Vec::with_capacity(current.len());

        for step in current {
            let written_step = match step {
                Step::Index(index) => Step::Index(self.written_index(&written, *index)),
                Step::Key(_) => step.clone(),
            };
            written.push(written_step);
        }

        written
    }

    /// The index as written of element `index` of the array at `array`.
    fn written_index(&self, array: &[Step], index: usize) -> usize {
        let mut taken: Vec<usize> = self
            .paths
            .iter()
            .filter(|path| path.len() == array.len() + 1 && path.starts_with(array))
            .filter_map(|path| match path.last() {
                Some(Step::Index(i)) => Some(*i),
                _ => None,
            })
            .collect();
        taken.sort_unstable();

        taken
            .into_iter()
            .fold(index, |at, i| if i <= at { at + 1 } else { at })
    }

    /// Whether a failure at `written` is one a removal caused: it lies above
    /// a path taken out (a table missing the key taken out of it).
    fn caused(&self, written: &[Step]) -> bool {
        self.paths
            .iter()
            .any(|path| path.len() > written.len() && path.starts_with(written))
    }
}

/// Decode `value`, the `crap.toml` section `section`, into `T`, recording
/// every key that fails in `report`. The value comes back only when nothing
/// failed.
pub(super) fn decode_section<T: DeserializeOwned>(
    section: &str,
    mut value: Value,
    report: &mut ErrorReport,
) -> Option<T> {
    let mut removed = Removed::default();

    loop {
        let err = match deserialize::<_, T>(value.clone()) {
            Ok(decoded) => return removed.paths.is_empty().then_some(decoded),
            Err(err) => err,
        };

        let current = steps(err.path()).unwrap_or_default();
        let written = removed.as_written(&current);

        if !removed.caused(&written) {
            report.push_message(format!(
                "{}: {}",
                render(section, &written),
                err.inner().message()
            ));
        }

        if !remove_at(&mut value, &current) {
            return None;
        }

        removed.paths.push(written);
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Default, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Section {
        port: u16,
        name: String,
        origins: Vec<String>,
        rules: Vec<Rule>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Rule {
        path: String,
        limit: u32,
    }

    fn decode(toml: &str) -> (Option<Section>, Vec<String>) {
        let mut report = ErrorReport::new();
        let value: Value = toml::from_str(toml).unwrap();
        let decoded = decode_section::<Section>("server", value, &mut report);

        let problems = report
            .into_result()
            .err()
            .map(|e| e.to_string().lines().map(str::to_string).collect())
            .unwrap_or_default();

        (decoded, problems)
    }

    #[test]
    fn a_valid_section_decodes() {
        let (decoded, problems) = decode("port = 80\nname = \"x\"\n");

        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(decoded.unwrap().port, 80);
    }

    #[test]
    fn a_valid_nested_table_array_decodes() {
        let (decoded, problems) = decode("[[rules]]\npath = \"/\"\nlimit = 5\n");

        assert!(problems.is_empty(), "{problems:?}");

        let rules = decoded.unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert_eq!((rules[0].path.as_str(), rules[0].limit), ("/", 5));
    }

    /// Regression: a section stopped at its first problem, so a second one in
    /// the same section surfaced only after the first was fixed. Both an
    /// unknown key and a mistyped one are reported, and the section is not
    /// decoded.
    #[test]
    fn every_problem_in_one_section_is_reported() {
        let (decoded, problems) = decode("port = \"eighty\"\nnmae = \"x\"\n");

        assert!(decoded.is_none());
        let text = problems.join("\n");
        assert!(text.contains("server.port: invalid type"), "{text}");
        assert!(text.contains("server.nmae: unknown field"), "{text}");
    }

    /// Two bad elements of one array are each reported at the index they are
    /// written at.
    #[test]
    fn array_problems_keep_their_written_index() {
        let (_, problems) = decode("origins = [\"a\", 1, \"b\", 2]\n");
        let text = problems.join("\n");

        assert!(text.contains("server.origins[1]:"), "{text}");
        assert!(text.contains("server.origins[3]:"), "{text}");
    }

    /// A key taken out of a table is not reported again as that table's
    /// missing key.
    #[test]
    fn a_removal_is_not_reported_as_a_missing_key() {
        let (_, problems) = decode("[[rules]]\npath = \"/\"\nlimit = \"many\"\n");
        let text = problems.join("\n");

        assert!(
            text.contains("server.rules[0].limit: invalid type"),
            "{text}"
        );
        assert!(!text.contains("missing field"), "{text}");
    }
}
