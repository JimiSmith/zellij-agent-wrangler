//! The hook client, and whether running it has ever worked.
//!
//! The sidebar reaches everything outside its own pane by running this: it is
//! how the agents are asked for, how a call is answered, and how the hooks are
//! installed. Running it is the one thing the sidebar does that can fail
//! silently and go on failing, so what a run came to is held rather than
//! discarded.
//!
//! The states are what they are so that a client which cannot be run has no
//! words to run: [`Client::path`] is the only way to name one, and the broken
//! state does not answer it. A caller cannot ask for a command line and then
//! forget to check.

/// A run's outcome, as the sidebar files it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Client {
    /// No client has been named yet, which is every moment before the layout
    /// has been read.
    #[default]
    Unnamed,
    /// A client is named and nothing has been run yet, so whether it can be is
    /// not yet known. Runs are attempted.
    Untried(String),
    /// The last run reached the client and it answered. Runs are attempted.
    Working(String),
    /// A run failed, and the same run would fail the same way: the client is
    /// not where it was said to be, or it refused what it was asked. Nothing
    /// further is run, because the failure is a fact about the client rather
    /// than about the call, and a sidebar that kept trying would spawn a
    /// process per state change for as long as the session lasted.
    Broken { why: String },
}

impl Client {
    /// The client the layout named, before anything has been run.
    pub fn new(path: &str) -> Self {
        Client::Untried(path.to_string())
    }

    /// What to run, for the states where running is still worth trying.
    pub fn path(&self) -> Option<&str> {
        match self {
            Client::Untried(path) | Client::Working(path) => Some(path),
            Client::Unnamed | Client::Broken { .. } => None,
        }
    }

    /// Take in that a run reached the client. `true` when this is news.
    ///
    /// A run that worked is not taken as evidence about a client already given
    /// up on: nothing is run in that state, so an answer arriving in it is one
    /// that was already in flight when the client broke.
    pub fn reached(&mut self) -> bool {
        match self {
            Client::Untried(path) => {
                *self = Client::Working(path.clone());
                true
            }
            _ => false,
        }
    }

    /// Take in that a run failed, said by whichever call it was and by what the
    /// run put on its error stream. `true` when this is news.
    pub fn failed(&mut self, call: &str, said: &str) -> bool {
        let why = match said.is_empty() {
            true => call.to_string(),
            false => format!("{call}: {said}"),
        };
        let broken = Client::Broken { why };
        match *self == broken {
            true => false,
            false => {
                *self = broken;
                true
            }
        }
    }

    /// Why the sidebar has stopped running the client, for the one state that
    /// has a reason. This is what the pane says instead of drawing nothing and
    /// explaining nothing.
    pub fn why(&self) -> Option<&str> {
        match self {
            Client::Broken { why } => Some(why),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_that_has_not_been_named_has_nothing_to_run() {
        assert_eq!(Client::default().path(), None);
    }

    #[test]
    fn a_named_client_is_run_until_a_run_fails() {
        let mut client = Client::new("/opt/bin/agent-wrangler");
        assert_eq!(client.path(), Some("/opt/bin/agent-wrangler"));
        assert!(client.reached());
        assert_eq!(client.path(), Some("/opt/bin/agent-wrangler"));
        assert!(client.failed("seen", "No such file or directory"));
        // The whole point: nothing is run again, so a call that cannot be
        // answered is not answered once per state change forever.
        assert_eq!(client.path(), None);
    }

    #[test]
    fn a_failure_says_which_call_it_was_and_what_was_said() {
        let mut client = Client::new("agent-wrangler");
        client.failed("register", "No such file or directory (os error 2)");
        assert_eq!(
            client.why(),
            Some("register: No such file or directory (os error 2)")
        );
    }

    #[test]
    fn a_failure_that_said_nothing_is_still_worth_saying() {
        let mut client = Client::new("agent-wrangler");
        client.failed("seen", "");
        assert_eq!(client.why(), Some("seen"));
    }

    #[test]
    fn the_same_failure_twice_is_not_news_twice() {
        // The pane is reprinted whole for every change, so a failure repeating
        // must not keep asking for one.
        let mut client = Client::new("agent-wrangler");
        assert!(client.failed("seen", "gone"));
        assert!(!client.failed("seen", "gone"));
        assert!(client.failed("register", "gone"));
    }

    #[test]
    fn an_answer_arriving_after_the_client_broke_does_not_revive_it() {
        // Runs already in flight when one of them failed still answer, and a
        // client given up on is not un-given-up by them.
        let mut client = Client::new("agent-wrangler");
        client.failed("seen", "gone");
        assert!(!client.reached());
        assert_eq!(client.path(), None);
    }

    #[test]
    fn only_a_broken_client_has_a_reason() {
        assert_eq!(Client::default().why(), None);
        assert_eq!(Client::new("agent-wrangler").why(), None);
    }
}
