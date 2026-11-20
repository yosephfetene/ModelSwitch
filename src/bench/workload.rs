//! A benchmark workload: a versioned JSON Lines file of prompts with
//! answer keys (ADR-038). One item per line; blank lines are skipped.
//!
//! ```text
//! {"id": "math-01", "task": "arithmetic", "input": "...", "expect": {"kind": "number", "answer": 42}}
//! ```
//!
//! A run stores the file's name and SHA-256, so results can always be
//! traced to the exact prompts that produced them. Changing a workload
//! means a new file (`workload-v2.jsonl`), never editing an old one.

use std::collections::HashSet;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::check::Expect;

const ID_MAX_CHARS: usize = 64;

/// What kind of work an item measures. Reports can be read per task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    /// Pull facts out of text into JSON.
    Extraction,
    /// Pick one label from a fixed set.
    Classification,
    /// Short reasoning with a numeric answer.
    Arithmetic,
    /// Follow an exact output format.
    Format,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub task: Task,
    pub input: String,
    pub expect: Expect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workload {
    /// The file name, such as `workload-v1.jsonl`.
    pub name: String,
    /// SHA-256 of the file's bytes, in lowercase hex.
    pub sha256: String,
    pub items: Vec<Item>,
}

/// Why a workload file was refused. Names the line, never its text, so
/// a bad prompt is not echoed into logs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkloadError {
    #[error("line {line}: {reason}")]
    Line { line: usize, reason: String },
    #[error("the workload has no items")]
    Empty,
}

impl Workload {
    pub fn parse(name: &str, text: &str) -> Result<Self, WorkloadError> {
        let mut items = Vec::new();
        let mut ids = HashSet::new();
        for (index, line) in text.lines().enumerate() {
            let refuse = |reason: String| WorkloadError::Line {
                line: index + 1,
                reason,
            };
            if line.trim().is_empty() {
                continue;
            }
            let item: Item = serde_json::from_str(line).map_err(|error| {
                // serde's message can quote the offending value; keep
                // only its position and category.
                refuse(format!("not a valid item ({:?} error)", error.classify()))
            })?;
            if !valid_id(&item.id) {
                return Err(refuse(format!(
                    "id must be 1-{ID_MAX_CHARS} lowercase letters, digits, and hyphens"
                )));
            }
            if item.input.trim().is_empty() {
                return Err(refuse("input must not be empty".to_owned()));
            }
            if !ids.insert(item.id.clone()) {
                return Err(refuse(format!("duplicate id {:?}", item.id)));
            }
            items.push(item);
        }
        if items.is_empty() {
            return Err(WorkloadError::Empty);
        }
        let sha256 = Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            name: name.to_owned(),
            sha256,
            items,
        })
    }
}

fn valid_id(id: &str) -> bool {
    (1..=ID_MAX_CHARS).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const MATH: &str = r#"{"id": "math-01", "task": "arithmetic", "input": "What is 6 times 7? Answer with only the number.", "expect": {"kind": "number", "answer": 42}}"#;
    const CAPITAL: &str = r#"{"id": "fmt-01", "task": "format", "input": "Capital of France, one word.", "expect": {"kind": "exact", "answer": "Paris"}}"#;

    #[test]
    fn items_are_read_in_order_and_blank_lines_skipped() {
        let text = format!("{MATH}\n\n{CAPITAL}\n");

        let workload = Workload::parse("w.jsonl", &text).unwrap();

        assert_eq!(workload.name, "w.jsonl");
        let ids: Vec<_> = workload.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["math-01", "fmt-01"]);
        assert_eq!(workload.items[0].task, Task::Arithmetic);
        assert_eq!(workload.items[0].expect, Expect::Number { answer: 42.0 });
    }

    #[test]
    fn the_fingerprint_is_the_sha256_of_the_file() {
        let workload = Workload::parse("w.jsonl", MATH).unwrap();
        let other = Workload::parse("w.jsonl", &format!("{MATH}\n")).unwrap();

        assert_eq!(workload.sha256.len(), 64);
        assert!(
            workload
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_ne!(workload.sha256, other.sha256, "any byte change shows");
        assert_eq!(
            Workload::parse("other-name.jsonl", MATH).unwrap().sha256,
            workload.sha256,
            "the name is not part of it"
        );
    }

    #[test]
    fn bad_items_name_their_line_but_not_their_text() {
        let secret = "do-not-echo-this";
        for (text, line) in [
            (format!("{MATH}\n{{\"id\": \"{secret}\""), 2),
            (format!("{MATH}\n{MATH}"), 2),
            (MATH.replace("math-01", "Math 01"), 1),
            (MATH.replace("math-01", ""), 1),
            (MATH.replace("arithmetic", "poetry"), 1),
            (
                MATH.replace("What is 6 times 7? Answer with only the number.", "  "),
                1,
            ),
            (MATH.replace("\"number\"", "\"regex\""), 1),
            (
                MATH.replace("}}", &format!("}}, \"note\": \"{secret}\"}}")),
                1,
            ),
            (format!("\n\n{}", MATH.replace("\"task\"", "\"tsk\"")), 3),
        ] {
            let error = Workload::parse("w.jsonl", &text).unwrap_err();

            assert!(
                matches!(&error, WorkloadError::Line { line: l, .. } if *l == line),
                "{text}: {error}"
            );
            assert!(!error.to_string().contains(secret), "{error}");
        }
    }

    #[test]
    fn a_workload_needs_items() {
        assert_eq!(Workload::parse("w.jsonl", ""), Err(WorkloadError::Empty));
        assert_eq!(
            Workload::parse("w.jsonl", "\n  \n"),
            Err(WorkloadError::Empty)
        );
    }
}
