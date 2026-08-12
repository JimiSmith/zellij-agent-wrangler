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

/// How many runs in a row have to fail before a client that has answered is
/// given up on.
///
/// A failed run says two very different things depending on what came before
/// it. A client that has never answered is one whose path names nothing far
/// more often than not, and no number of attempts will find it, so the first
/// failure settles it. A client that has answered is known to be there, so a
/// run that failed is more likely a fact about the moment than about the
/// client: the machine briefly out of file descriptors, a fork that could not
/// be made. Those pass, and a sidebar retired by one of them is a sidebar drawing
/// nothing for the rest of the session because the machine was busy for a second.
///
/// Five is more than a burst of calls answered at once can produce and far
/// fewer than a session's worth of state changes, so a client that has really
/// gone still stops being run almost at once.
const GIVING_UP: u32 = 5;

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
    /// A run failed, but this client has answered before, so it is not yet
    /// taken for gone. Runs are attempted, and the next one that answers puts
    /// it back to working. Nothing is drawn about it: a failure that passes is
    /// not something the user was ever waiting to hear.
    Failing { path: String, failures: u32 },
    /// Runs have failed, and the same run would fail the same way: the client
    /// is not where it was said to be, or it refused what it was asked.
    /// Nothing further is run, because the failure is a fact about the client
    /// rather than about the call, and a sidebar that kept trying would spawn a
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
            Client::Untried(path) | Client::Working(path) | Client::Failing { path, .. } => {
                Some(path)
            }
            Client::Unnamed | Client::Broken { .. } => None,
        }
    }

    /// Take in that a run reached the client. `true` when this is news.
    ///
    /// A run that worked is not taken as evidence about a client already given
    /// up on: nothing is run in that state, so an answer arriving in it is one
    /// that was already in flight when the client broke.
    ///
    /// It clears the failures rather than counting against them, which is what
    /// makes [`GIVING_UP`] a run of failures rather than a tally: a client
    /// answering every other call has not gone.
    pub fn reached(&mut self) -> bool {
        match self {
            Client::Untried(path) | Client::Failing { path, .. } => {
                *self = Client::Working(path.clone());
                true
            }
            _ => false,
        }
    }

    /// Take in that a run failed, said by whichever call it was and by what the
    /// run put on its error stream. `true` when this is news.
    ///
    /// What a failure costs the client depends on whether it has ever answered:
    /// see [`GIVING_UP`].
    pub fn failed(&mut self, call: &str, said: &str) -> bool {
        let why = match said.is_empty() {
            true => call.to_string(),
            false => format!("{call}: {said}"),
        };
        let next = match self {
            // Nothing is run without a path, so a failure here is a failure of
            // something this never asked for.
            Client::Unnamed => return false,
            Client::Untried(_) => Client::Broken { why },
            Client::Working(path) => Client::Failing {
                path: path.clone(),
                failures: 1,
            },
            Client::Failing { path, failures } if *failures + 1 < GIVING_UP => Client::Failing {
                path: path.clone(),
                failures: *failures + 1,
            },
            Client::Failing { .. } | Client::Broken { .. } => Client::Broken { why },
        };
        match *self == next {
            true => false,
            false => {
                *self = next;
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
    fn a_client_that_has_never_answered_is_given_up_on_at_once() {
        // The usual reason a first run fails is a path that names nothing, and
        // looking again costs a process to find out the same thing.
        let mut client = Client::new("/opt/bin/agent-wrangler");
        assert_eq!(client.path(), Some("/opt/bin/agent-wrangler"));
        assert!(client.failed("register", "No such file or directory"));
        assert_eq!(client.path(), None);
    }

    #[test]
    fn a_client_that_has_answered_is_not_given_up_on_for_one_failure() {
        // A client that answered once is there. A run that failed after that
        // is far more often the machine having a bad moment - out of file
        // descriptors, out of processes - and those pass.
        let mut client = Client::new("/opt/bin/agent-wrangler");
        assert!(client.reached());
        assert!(client.failed("seen", "Too many open files (os error 24)"));
        assert_eq!(client.path(), Some("/opt/bin/agent-wrangler"));
        // Nothing is said about it, because nothing has been given up on: the
        // next run is what the user is waiting for and it is still coming.
        assert_eq!(client.why(), None);
    }

    #[test]
    fn a_run_that_answers_starts_the_count_over() {
        // What retires a client is a run of failures with nothing working in
        // between, not a tally: a client answering every other call has not
        // gone anywhere.
        let mut client = Client::new("agent-wrangler");
        client.reached();
        for _ in 0..GIVING_UP * 3 {
            client.failed("seen", "Too many open files (os error 24)");
            assert!(client.reached());
        }
        assert_eq!(client.path(), Some("agent-wrangler"));
    }

    #[test]
    fn a_client_that_keeps_failing_is_given_up_on() {
        // The whole point of giving up at all: nothing is run again, so a call
        // that cannot be answered is not answered once per state change for as
        // long as the session lasts.
        let mut client = Client::new("agent-wrangler");
        client.reached();
        for _ in 0..GIVING_UP {
            assert_eq!(client.why(), None, "still trying");
            client.failed("seen", "gone");
        }
        assert_eq!(client.path(), None);
        assert_eq!(client.why(), Some("seen: gone"));
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
