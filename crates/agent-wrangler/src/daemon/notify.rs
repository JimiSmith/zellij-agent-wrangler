//! Saying a call for the user out loud.
//!
//! Running the notifier is waited for rather than left to run on its own. It is
//! a program that prints nothing and exits, and waiting is what reaps it: a
//! notification abandoned as it starts would leave a dead child behind, once per
//! call, for as long as this runs.
//!
//! Two things bound what the notifier costs. It is run at most once every
//! [`QUIET`], because agents call in bursts and a burst is one interruption
//! rather than several. And it is run with the calling agent's location rather
//! than with this process's, because a notifier that reaches back into a
//! multiplexer has to be told which session, and this process's own environment
//! is not an answer to that.

use std::process::Stdio;
use std::time::{Duration, Instant};

use agent_wrangler_core::notify::Notifier;
use agent_wrangler_core::origin::LOCATION_VARS;

use crate::daemon::state::Call;
use crate::platform::command;

/// How long the notifier is left alone after a call has been said out loud.
///
/// A notification is an interruption, and interruptions do not add up: an agent
/// that asks twice while the user is away from the machine has asked once as
/// far as the user is concerned. Agents also call in flurries - one finishing
/// as another stops for permission - and each notification is a process, and
/// for some notifiers a message back into the multiplexer, so a flurry
/// announced call by call costs far more than it tells anyone.
///
/// Nothing here decides what a call does to what is held or to what is
/// delivered. Only the saying out loud is passed over.
const QUIET: Duration = Duration::from_secs(5);

/// When a call was last said out loud, which is the whole of what keeps a run
/// of calls from being a run of notifications.
#[derive(Debug, Default)]
pub struct Announced {
    last: Option<Instant>,
}

impl Announced {
    /// Whether a call arriving at `now` is one to say out loud, taking the
    /// moment if it is.
    ///
    /// The moment is passed in rather than read here, so that what this does
    /// can be said in a test without waiting [`QUIET`] to find out.
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

/// What the notifier should see for every location variable there is: the
/// call's own value, or nothing at all for one the call carries nothing for.
///
/// Every name is decided rather than only the ones the call knows about, which
/// is the whole of the rule: a variable left undecided is one the notifier
/// reads off whatever this process is carrying.
fn where_it_is(call: &Call) -> Vec<(&'static str, Option<&str>)> {
    let named = call.origin.named();
    LOCATION_VARS
        .iter()
        .map(|name| (*name, named.get(name).copied()))
        .collect()
}

/// Announce one call with one notifier, and say whether it ran.
///
/// Side effect: runs the program the notifier names, with the agent and what it
/// is called as the last two arguments, which is the shape `notify-send` and
/// its like take. It is given no input and its output is discarded; the exit
/// status is the whole of what is read back.
///
/// Side effect: every name in [`LOCATION_VARS`] is set on that program from the
/// call's own origin, and cleared where the call carries nothing for it. So the
/// notifier reads where the agent that called is, and never what this process
/// happens to have inherited: a notifier that speaks to a multiplexer reads
/// exactly these variables to know which session to speak to, and this
/// process's are not that agent's.
pub fn raise(notifier: &Notifier, call: &Call) -> bool {
    let mut announce = command(notifier.program());
    announce.args(notifier.arguments(&call.agent, &call.label));
    for (name, value) in where_it_is(call) {
        match value {
            Some(value) => announce.env(name, value),
            None => announce.env_remove(name),
        };
    }
    announce
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::origin::Origin;

    fn call() -> Call {
        Call {
            agent: "claude".to_string(),
            label: "the port".to_string(),
            origin: Origin::from(|name| match name {
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

    /// A notifier that exits successfully only if the two arguments it is
    /// handed last are exactly `test`, whatever they hold.
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
        // The body holds a space, so a notifier receiving it as two arguments
        // rather than one is the failure this catches.
        assert!(raise(&expecting("claude", "the port"), &call()));
        assert!(!raise(&expecting("claude", "the"), &call()));
    }

    /// A notifier that exits successfully only if one named variable holds
    /// exactly this, where a variable that is not set reads as empty.
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
        // A variable the call says nothing about is cleared rather than left
        // standing: a stale pane id is as wrong as a stale session name.
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
        // What is asserted comes from the call rather than from this process's
        // environment, so the test says nothing about where it is run.
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
        // Otherwise an agent calling faster than the quiet is an agent that is
        // never announced at all: every call passed over would put the next one
        // further off.
        let mut announced = Announced::default();
        let began = Instant::now();
        assert!(announced.worth_saying(began));
        for tick in 1..5 {
            assert!(!announced.worth_saying(began + QUIET / 5 * tick));
        }
        assert!(announced.worth_saying(began + QUIET));
    }

    #[test]
    fn the_first_call_of_all_is_said_at_once() {
        // Nothing has been said, so there is no quiet to be inside of.
        assert!(Announced::default().worth_saying(Instant::now()));
    }
}
