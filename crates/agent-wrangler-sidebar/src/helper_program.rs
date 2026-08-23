//! The health of the `agent-wrangler` program that the sidebar runs.
//!
//! A sidebar reaches the daemon through that program, so a sidebar that cannot
//! run it draws nothing but the reason. This module holds no connection and no
//! process. It holds a path and a count of the calls that failed in a row.

/// How many calls may fail in a row before the sidebar gives up on the program.
const FAILURES_BEFORE_BROKEN: u32 = 5;

/// What the sidebar knows about the program it runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HelperProgramState {
    /// No path is known yet.
    #[default]
    Unnamed,
    /// A path is known and no call has run yet.
    Untried(String),
    /// The last call succeeded.
    Working(String),
    /// A call failed, and the count of failures in a row is under the limit.
    Failing { path: String, failures: u32 },
    /// The sidebar gave up. It runs the program no more, and the pane says why.
    Broken { why: String },
}

impl HelperProgramState {
    pub fn new(path: &str) -> Self {
        HelperProgramState::Untried(path.to_string())
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            HelperProgramState::Untried(path)
            | HelperProgramState::Working(path)
            | HelperProgramState::Failing { path, .. } => Some(path),
            HelperProgramState::Unnamed | HelperProgramState::Broken { .. } => None,
        }
    }

    pub fn record_success(&mut self) -> bool {
        match self {
            HelperProgramState::Untried(path) | HelperProgramState::Failing { path, .. } => {
                *self = HelperProgramState::Working(path.clone());
                true
            }
            _ => false,
        }
    }

    pub fn record_failure(&mut self, call: &str, said: &str) -> bool {
        let why = if said.is_empty() {
            call.to_string()
        } else {
            format!("{call}: {said}")
        };
        let next = match self {
            HelperProgramState::Unnamed => return false,
            HelperProgramState::Untried(_) => HelperProgramState::Broken { why },
            HelperProgramState::Working(path) => HelperProgramState::Failing {
                path: path.clone(),
                failures: 1,
            },
            HelperProgramState::Failing { path, failures }
                if *failures + 1 < FAILURES_BEFORE_BROKEN =>
            {
                HelperProgramState::Failing {
                    path: path.clone(),
                    failures: *failures + 1,
                }
            }
            HelperProgramState::Failing { .. } | HelperProgramState::Broken { .. } => {
                HelperProgramState::Broken { why }
            }
        };
        if *self == next {
            false
        } else {
            *self = next;
            true
        }
    }

    pub fn why(&self) -> Option<&str> {
        match self {
            HelperProgramState::Broken { why } => Some(why),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untried_client_is_given_up_on_at_the_first_failure() {
        let mut client = HelperProgramState::new("missing");
        assert!(client.record_failure("register", "not found"));
        assert_eq!(client.path(), None);
        assert_eq!(client.why(), Some("register: not found"));
    }

    #[test]
    fn a_working_client_survives_a_transient_failure() {
        let mut client = HelperProgramState::new("agent-wrangler");
        client.record_success();
        client.record_failure("seen", "busy");
        assert_eq!(client.path(), Some("agent-wrangler"));
        assert_eq!(client.why(), None);
        assert!(client.record_success());
    }

    #[test]
    fn repeated_failures_eventually_retire_a_working_client() {
        let mut client = HelperProgramState::new("agent-wrangler");
        client.record_success();
        for _ in 0..FAILURES_BEFORE_BROKEN {
            client.record_failure("seen", "gone");
        }
        assert_eq!(client.path(), None);
        assert_eq!(client.why(), Some("seen: gone"));
    }

    #[test]
    fn a_success_resets_the_run_of_failures() {
        let mut client = HelperProgramState::new("agent-wrangler");
        client.record_success();
        for _ in 0..FAILURES_BEFORE_BROKEN * 3 {
            client.record_failure("seen", "busy");
            assert_eq!(client.why(), None);
            assert!(client.record_success());
        }
        assert_eq!(client.path(), Some("agent-wrangler"));
    }
}
