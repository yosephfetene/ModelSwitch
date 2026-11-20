//! Answer keys and how an output is graded against one (ADR-038).
//!
//! Checks are deterministic: the same output always gets the same
//! verdict. They are deliberately strict about format, because following
//! the prompt's format is part of what a workload measures. Only
//! surrounding whitespace, a trailing full stop, and (for JSON) a
//! Markdown code fence are forgiven.

use serde::Deserialize;
use serde_json::{Map, Value};

/// What a correct answer looks like.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expect {
    /// The whole answer is this text.
    Exact {
        answer: String,
        #[serde(default)]
        ignore_case: bool,
    },
    /// The whole answer is this number. Thousands separators are allowed.
    Number { answer: f64 },
    /// The answer is a JSON object with at least these fields and values.
    /// Other fields are allowed.
    Json { fields: Map<String, Value> },
}

impl Expect {
    pub fn passes(&self, output: &str) -> bool {
        match self {
            Self::Exact {
                answer,
                ignore_case,
            } => {
                let output = bare(output);
                if *ignore_case {
                    output.to_lowercase() == answer.to_lowercase()
                } else {
                    output == answer
                }
            }
            Self::Number { answer } => bare(output)
                .replace(',', "")
                .parse::<f64>()
                .is_ok_and(|number| same_number(number, *answer)),
            Self::Json { fields } => {
                let Ok(Value::Object(object)) = serde_json::from_str::<Value>(unfenced(output))
                else {
                    return false;
                };
                fields.iter().all(|(name, expected)| {
                    object
                        .get(name)
                        .is_some_and(|actual| same_value(actual, expected))
                })
            }
        }
    }
}

/// The output without surrounding whitespace or one trailing full stop.
fn bare(output: &str) -> &str {
    let output = output.trim();
    output.strip_suffix('.').unwrap_or(output).trim_end()
}

/// The output without a surrounding Markdown code fence, if it has one.
fn unfenced(output: &str) -> &str {
    let output = output.trim();
    let Some(rest) = output.strip_prefix("```") else {
        return output;
    };
    // Drop the fence's info string (such as `json`) up to the first line
    // break, then the closing fence.
    let body = rest.split_once('\n').map_or("", |(_, body)| body);
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

fn same_number(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0)
}

/// JSON equality, except that numbers compare by value, so `1843` and
/// `1843.0` are the same answer.
fn same_value(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(actual), Value::Number(expected)) => actual
            .as_f64()
            .zip(expected.as_f64())
            .is_some_and(|(actual, expected)| same_number(actual, expected)),
        (Value::Array(actual), Value::Array(expected)) => {
            actual.len() == expected.len()
                && actual.iter().zip(expected).all(|(a, e)| same_value(a, e))
        }
        (Value::Object(actual), Value::Object(expected)) => {
            actual.len() == expected.len()
                && expected
                    .iter()
                    .all(|(name, e)| actual.get(name).is_some_and(|a| same_value(a, e)))
        }
        _ => actual == expected,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn exact(answer: &str, ignore_case: bool) -> Expect {
        Expect::Exact {
            answer: answer.to_owned(),
            ignore_case,
        }
    }

    fn fields(value: Value) -> Expect {
        let Value::Object(fields) = value else {
            panic!("fields must be an object")
        };
        Expect::Json { fields }
    }

    #[test]
    fn exact_forgives_only_whitespace_and_a_full_stop() {
        let key = exact("Paris", false);

        for output in ["Paris", "  Paris\n", "Paris.", " Paris. "] {
            assert!(key.passes(output), "{output:?}");
        }
        for output in [
            "paris",
            "Paris!",
            "The answer is Paris",
            "Paris..",
            "\"Paris\"",
            "",
        ] {
            assert!(!key.passes(output), "{output:?}");
        }
    }

    #[test]
    fn exact_can_ignore_case() {
        let key = exact("positive", true);

        assert!(key.passes("Positive."));
        assert!(key.passes("POSITIVE"));
        assert!(!key.passes("positively"));
    }

    #[test]
    fn a_number_must_be_the_whole_answer() {
        let key = Expect::Number { answer: 1250.0 };

        for output in ["1250", "1,250", " 1250.\n", "1250.0", "1.25e3"] {
            assert!(key.passes(output), "{output:?}");
        }
        for output in [
            "1251",
            "$1250",
            "1250 apples",
            "The answer is 1250",
            "",
            "NaN",
        ] {
            assert!(!key.passes(output), "{output:?}");
        }
        assert!(Expect::Number { answer: 0.1 }.passes("0.1"));
        assert!(Expect::Number { answer: -3.5 }.passes("-3.5"));
    }

    #[test]
    fn json_needs_every_expected_field_with_its_value() {
        let key = fields(json!({ "name": "Ada Lovelace", "year": 1843 }));

        for output in [
            r#"{"name": "Ada Lovelace", "year": 1843}"#,
            r#"{"year": 1843.0, "name": "Ada Lovelace", "extra": true}"#,
            "```json\n{\"name\": \"Ada Lovelace\", \"year\": 1843}\n```",
            "```\n{\"name\": \"Ada Lovelace\", \"year\": 1843}\n```\n",
        ] {
            assert!(key.passes(output), "{output:?}");
        }
        for output in [
            r#"{"name": "Ada Lovelace"}"#,
            r#"{"name": "ada lovelace", "year": 1843}"#,
            r#"{"name": "Ada Lovelace", "year": "1843"}"#,
            r#"[{"name": "Ada Lovelace", "year": 1843}]"#,
            r#"Here it is: {"name": "Ada Lovelace", "year": 1843}"#,
            "not json",
            "",
        ] {
            assert!(!key.passes(output), "{output:?}");
        }
    }

    #[test]
    fn json_compares_nested_values_exactly() {
        let key = fields(json!({ "tags": ["a", "b"], "size": { "w": 2, "h": 3 } }));

        assert!(key.passes(r#"{"tags": ["a", "b"], "size": {"h": 3.0, "w": 2}}"#));
        assert!(!key.passes(r#"{"tags": ["b", "a"], "size": {"w": 2, "h": 3}}"#));
        assert!(!key.passes(r#"{"tags": ["a", "b"], "size": {"w": 2, "h": 3, "d": 1}}"#));
        assert!(!key.passes(r#"{"tags": ["a"], "size": {"w": 2, "h": 3}}"#));
    }

    #[test]
    fn answer_keys_are_read_from_json() {
        let key: Expect =
            serde_json::from_value(json!({ "kind": "exact", "answer": "Paris" })).unwrap();
        assert_eq!(key, exact("Paris", false));

        let key: Expect = serde_json::from_value(json!({ "kind": "number", "answer": 7 })).unwrap();
        assert_eq!(key, Expect::Number { answer: 7.0 });

        assert!(
            serde_json::from_value::<Expect>(json!({ "kind": "regex", "answer": "x" })).is_err()
        );
    }
}
