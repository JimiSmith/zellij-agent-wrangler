//! This module announces one call to the user.
//!
//! This module waits for the notifier, and does not leave it to run on its own.
//! The notifier is a program that prints nothing and then exits. The wait reaps
//! it. A notification abandoned at its start leaves a dead child behind, once
//! per call, for as long as this process runs.
//!
//! Two limits bound the cost of the notifier. First, the notifier runs at most
//! once every [`QUIET`], because agents call in bursts, and a burst is one
//! interruption rather than several. Second, the notifier runs with the
//! location of the agent that called, and not with the location of this
//! process. A notifier that reaches back into a multiplexer must know which
//! session to speak to. The environment of this process is not an answer to
//! that question.
//!
//! The wait is bounded as well. Somebody else writes the notifier. A notifier
//! that never exits holds the thread that served the hook, for as long as the
//! daemon runs.

use std::process::Stdio;
use std::time::{Duration, Instant};

use agent_wrangler_core::notify::Notifier;
use agent_wrangler_core::origin::LOCATION_VARS;

use crate::daemon::state::Call;
use crate::platform::{command, ran, Ran};

/// The time after one announcement in which this module announces nothing.
///
/// A notification is an interruption, and interruptions do not add up. An agent
/// that asks twice while the user is away from the machine asked once, as far
/// as the user is concerned. Agents also call in flurries. One agent finishes
/// while another agent stops for permission. Each notification is a process,
/// and for some notifiers it is also a message back into the multiplexer. A
/// flurry announced call by call costs far more than it tells anyone.
///
/// This limit changes nothing about the state that the daemon holds or
/// delivers. It passes over only the announcement.
const QUIET: Duration = Duration::from_secs(5);

/// The time that a notifier gets to finish. After this time the daemon kills it.
///
/// [`QUIET`] sets this time. The daemon announces nothing else inside [`QUIET`]
/// anyway. A wait bounded by [`QUIET`] therefore holds at most one notifier
/// that never exits at a time, rather than one more per call. The count is the
/// same for the processes left in the table and for the threads that wait on
/// them.
const PATIENCE: Duration = QUIET;

/// The moment of the last announcement, which is the whole of what turns a run
/// of calls into one notification.
#[derive(Debug, Default)]
pub struct Announced {
    last: Option<Instant>,
}

impl Announced {
    /// Whether the daemon announces a call that arrives at `now`. If the
    /// answer is yes, this method keeps `now` as the last announcement.
    ///
    /// The caller passes the moment in, and this method reads no clock. A test
    /// can then check this behavior without a wait of [`QUIET`].
    pub fn worth_saying(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last {
            if now.duration_since(last) < QUIET {
                return false;
            }
        }
        self.last = Some(now);
        true
    }
}

/// What the notifier must see for every location variable. The notifier sees
/// the value of the call. For a variable that the call carries no value for,
/// the notifier sees nothing at all.
///
/// This function decides every name, and not only the names that the call knows
/// about. That rule is the whole point. The notifier reads an undecided
/// variable off the environment of this process.
fn where_it_is(call: &Call) -> Vec<(&'static str, Option<&str>)> {
    let named = call.origin.values_by_variable_name();
    LOCATION_VARS
        .iter()
        .map(|name| (*name, named.get(name).copied()))
        .collect()
}

/// Announces one call with one notifier, and reports whether the notifier ran.
///
/// Side effect: this function runs the program that the notifier names. The
/// agent and the label of the call are the last two arguments, which is the
/// shape that `notify-send` and similar programs take. The program gets no
/// input, and this function discards its output. The exit status is the whole
/// of the answer. If the program does not finish within [`PATIENCE`], this
/// function kills it.
///
/// Side effect: this function sets every name in [`LOCATION_VARS`] on that
/// program from the origin of the call. For a name that the call carries no
/// value for, this function clears the variable. The notifier then reads the
/// location of the agent that called, and never a value that this process
/// inherited. A notifier that speaks to a multiplexer reads exactly these
/// variables to find the session. The variables of this process are not the
/// variables of that agent.
pub fn raise(notifier: &Notifier, call: &Call) -> bool {
    let mut announce = command(notifier.program());
    announce.args(notifier.arguments_for_notification(&call.agent, &call.label));
    for (name, value) in where_it_is(call) {
        match value {
            Some(value) => announce.env(name, value),
            None => announce.env_remove(name),
        };
    }
    announce
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match ran(&mut announce, PATIENCE) {
        Ran::Worked => true,
        Ran::Failed => false,
        // The user heard the call, whatever the program then did about its exit.
        Ran::Abandoned => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::origin::Origin;

    fn call() -> Call {
        Call {
            agent: "claude".to_string(),
            label: "the port".to_string(),
            origin: Origin::from_lookup(|name| match name {
                "ZELLIJ_SESSION_NAME" => Some("wrangler-proto".to_string()),
                _ => None,
            }),
        }
    }

    #[test]
    fn a_notifier_that_is_not_there_is_a_notification_that_did_not_happen() {
        let missing = Notifier::new(vec!["/nonexistent/agent-wrangler/notifier".to_string()])
            .expect("a program was named");
        assert!(!raise(&missing, &call()));
    }

    /// A notifier that tests its last two arguments. If those arguments are
    /// exactly `test`, whatever they hold, the notifier exits with success.
    #[cfg(unix)]
    fn expecting(agent: &str, label: &str) -> Notifier {
        Notifier::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(r#"[ "$1" = '{agent}' ] && [ "$2" = '{label}' ]"#),
            "notifier".to_string(),
        ])
        .expect("a program was named")
    }

    #[cfg(unix)]
    #[test]
    fn the_agent_and_what_it_is_called_arrive_last_and_whole() {
        // The body holds a space. This test catches a notifier that gets the
        // body as two arguments rather than as one.
        assert!(raise(&expecting("claude", "the port"), &call()));
        assert!(!raise(&expecting("claude", "the"), &call()));
    }

    /// A notifier that tests one named variable. If that variable holds
    /// exactly this value, the notifier exits with success. An unset variable
    /// reads as empty.
    #[cfg(unix)]
    fn reading(name: &str, value: &str) -> Notifier {
        Notifier::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(r#"[ "${{{name}:-}}" = '{value}' ]"#),
        ])
        .expect("a program was named")
    }

    #[test]
    fn the_notifier_is_told_the_calling_agents_location_and_no_other() {
        let call = call();
        let told: std::collections::BTreeMap<&str, Option<&str>> =
            where_it_is(&call).into_iter().collect();
        assert_eq!(
            told.get("ZELLIJ_SESSION_NAME"),
            Some(&Some("wrangler-proto"))
        );
        // This module clears a variable that the call says nothing about. A
        // stale pane id is as wrong as a stale session name.
        assert_eq!(told.get("ZELLIJ_PANE_ID"), Some(&None));
        assert_eq!(
            told.len(),
            LOCATION_VARS.len(),
            "a variable left undecided is one read off this process"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_location_reaches_the_program_that_is_run() {
        // The asserted value comes from the call, and not from the environment
        // of this process. The test therefore holds wherever it runs.
        assert!(raise(
            &reading("ZELLIJ_SESSION_NAME", "wrangler-proto"),
            &call()
        ));
    }

    #[test]
    fn a_flurry_of_calls_is_one_notification() {
        let mut announced = Announced::default();
        let began = Instant::now();
        assert!(announced.worth_saying(began));
        assert!(!announced.worth_saying(began + QUIET / 2));
        assert!(!announced.worth_saying(began + QUIET - Duration::from_millis(1)));
        assert!(announced.worth_saying(began + QUIET));
    }

    #[test]
    fn the_quiet_runs_from_what_was_said_rather_than_from_what_was_passed_over() {
        // An agent that calls faster than the quiet still gets one
        // announcement. Otherwise each call passed over moves the next
        // announcement further off, and the daemon announces nothing at all.
        let mut announced = Announced::default();
        let began = Instant::now();
        assert!(announced.worth_saying(began));
        for tick in 1..5 {
            assert!(!announced.worth_saying(began + QUIET / 5 * tick));
        }
        assert!(announced.worth_saying(began + QUIET));
    }

    #[test]
    fn a_notifier_that_never_exits_is_one_of_it_at_a_time() {
        // The reason for the bound. The daemon cannot announce the next call
        // sooner than the quiet. A notifier that hangs therefore ends before
        // the daemon starts another one.
        assert!(PATIENCE <= QUIET);
    }

    #[test]
    fn the_first_call_of_all_is_said_at_once() {
        // The daemon announced nothing, so there is no quiet to be inside of.
        assert!(Announced::default().worth_saying(Instant::now()));
    }
}
