//! Saying a call for the user out loud.
//!
//! Running the notifier is waited for rather than left to run on its own. It is
//! a program that prints nothing and exits, and waiting is what reaps it: a
//! notification abandoned as it starts would leave a dead child behind, once per
//! call, for as long as this runs.

use std::process::{Command, Stdio};

use agent_wrangler_core::notify::Notifier;

use crate::daemon::state::Call;

/// Announce one call with one notifier, and say whether it ran.
///
/// Side effect: runs the program the notifier names, with the agent and what it
/// is called as the last two arguments, which is the shape `notify-send` and
/// its like take. It is given no input and its output is discarded; the exit
/// status is the whole of what is read back.
pub fn raise(notifier: &Notifier, call: &Call) -> bool {
    Command::new(notifier.program())
        .args(notifier.arguments(&call.agent, &call.label))
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

    fn call() -> Call {
        Call {
            agent: "claude".to_string(),
            label: "the port".to_string(),
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
}
