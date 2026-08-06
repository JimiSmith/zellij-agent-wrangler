//! Reading an agent's hook payload.
//!
//! Two agents write the same fields under different spellings, so both are
//! accepted, snake_case preferred. A body that is not a JSON object yields an
//! empty payload rather than an error: an event that names no session is
//! dropped, and that is the only field the caller needs to test.

use serde_json::Value;

/// The fields the sidebar takes from a hook body. `recoverable` is present only
/// when the body carried a genuine JSON boolean, since an agent that says
/// nothing about it is not saying `false`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Payload {
    pub session_id: String,
    pub cwd: String,
    pub recoverable: Option<bool>,
}

impl Payload {
    pub fn parse(body: &str) -> Self {
        let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let first_nonempty = |keys: &[&str]| -> String {
            keys.iter()
                .filter_map(|key| value.get(key).and_then(Value::as_str))
                .find(|text| !text.is_empty())
                .unwrap_or_default()
                .to_string()
        };
        Payload {
            session_id: first_nonempty(&["session_id", "sessionId"]),
            cwd: first_nonempty(&["cwd"]),
            recoverable: value.get("recoverable").and_then(Value::as_bool),
        }
    }
}

/// What to call an agent working in `cwd`: the directory's own name.
///
/// A path with no final component (the root, or nothing at all) has no name to
/// take, and the agent is labelled by what it is instead.
pub fn label(cwd: &str, agent: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| agent.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_claude_spelling_is_read() {
        let body = r#"{"session_id":"abc","cwd":"/home/u/repo","transcript_path":"/t.jsonl"}"#;
        assert_eq!(
            Payload::parse(body),
            Payload {
                session_id: "abc".to_string(),
                cwd: "/home/u/repo".to_string(),
                recoverable: None,
            }
        );
    }

    #[test]
    fn recoverable_is_read_only_from_a_boolean() {
        assert_eq!(
            Payload::parse(r#"{"sessionId":"s","recoverable":true}"#).recoverable,
            Some(true)
        );
        assert_eq!(
            Payload::parse(r#"{"sessionId":"s","recoverable":false}"#).recoverable,
            Some(false)
        );
        // A string is not a boolean, so it says nothing either way.
        assert_eq!(
            Payload::parse(r#"{"sessionId":"s","recoverable":"true"}"#).recoverable,
            None
        );
    }

    #[test]
    fn the_copilot_spelling_is_read() {
        assert_eq!(Payload::parse(r#"{"sessionId":"xyz"}"#).session_id, "xyz");
    }

    #[test]
    fn snake_case_wins_over_camel() {
        let body = r#"{"session_id":"snake","sessionId":"camel"}"#;
        assert_eq!(Payload::parse(body).session_id, "snake");
    }

    #[test]
    fn an_empty_value_falls_through_to_the_other_spelling() {
        let body = r#"{"session_id":"","sessionId":"camel"}"#;
        assert_eq!(Payload::parse(body).session_id, "camel");
    }

    #[test]
    fn a_body_that_is_not_an_object_reads_as_empty() {
        for body in ["", "not json", "[1,2,3]", "null"] {
            assert_eq!(Payload::parse(body), Payload::default(), "{body}");
        }
    }

    #[test]
    fn a_label_is_the_working_directorys_own_name() {
        assert_eq!(label("/home/u/repo", "claude"), "repo");
        assert_eq!(label("/home/u/repo/", "claude"), "repo");
    }

    #[test]
    fn a_path_with_no_name_leaves_the_agent_labelled_by_what_it_is() {
        assert_eq!(label("/", "claude"), "claude");
        assert_eq!(label("", "copilot"), "copilot");
    }
}
