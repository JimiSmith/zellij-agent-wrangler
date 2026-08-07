//! The hook client an agent invokes: it reads one hook payload on standard
//! input and pipes a single record to the sidebars of the zellij session it was
//! invoked in.
//!
//! The whole path is best effort. A hook runs inside the agent's own turn, so
//! nothing here is allowed to fail loudly or to take long: outside zellij it
//! does nothing at all, and a pipe that cannot be delivered is dropped. The only
//! non-zero exit is a missing argument, which is a misconfiguration no event
//! could describe.

use std::io::Read;
use std::process::{Command, ExitCode, Stdio};

use zellij_agent_wrangler::agents::{
    Agent, Meta, Turn, ATTENTION_MESSAGE, END_MESSAGE, START_MESSAGE, WORKING_MESSAGE,
};
use zellij_agent_wrangler::install;
use zellij_agent_wrangler::model::SessionId;
use zellij_agent_wrangler::payload::{dir, Payload};
use zellij_agent_wrangler::titles;

/// The pane the hook was invoked in, which zellij sets on every terminal pane
/// it spawns and every process started in one inherits.
fn pane() -> Option<u32> {
    std::env::var("ZELLIJ_PANE_ID").ok()?.trim().parse().ok()
}

/// Send one message down a zellij pipe, addressed to no plugin so that every
/// sidebar of this session hears it.
///
/// Side effect: runs `zellij`, whose own output is discarded. The call waits for
/// zellij to accept the message, and gives up silently if it cannot.
fn pipe(name: &str, payload: &str) {
    let _ = Command::new("zellij")
        .args(["pipe", "--name", name, "--", payload])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Milliseconds since the epoch, which is what orders one call for the user
/// against another. Read here because this process sees each call exactly once,
/// where every sidebar sees all of them.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

fn read_stdin() -> String {
    let mut body = String::new();
    let _ = std::io::stdin().read_to_string(&mut body);
    body
}

/// The pipe an agent's own event name reports on.
///
/// An error is the one event whose meaning depends on who raised it: Copilot
/// says whether it can carry on, and one that can is still working rather than
/// waiting. Every other agent's error is something the user has to look at.
/// Anything unrecognised is a session announcing itself, which is also how a
/// session already known re-states where it is.
fn message(agent: &str, event: &str, recoverable: Option<bool>) -> &'static str {
    match event {
        "end" => END_MESSAGE,
        "working" => WORKING_MESSAGE,
        "needsAttention" => ATTENTION_MESSAGE,
        "error" if agent == "copilot" && recoverable == Some(true) => WORKING_MESSAGE,
        "error" => ATTENTION_MESSAGE,
        _ => START_MESSAGE,
    }
}

/// Everything this session is called by, gathered from the hook body and from
/// whatever the agent keeps on disk.
///
/// Side effect: reads the agent's own files. An agent this client does not know
/// is described by its working directory alone, which every hook body carries.
fn meta(agent: &str, payload: &Payload) -> Meta {
    let found = match agent {
        "claude" => titles::claude(&payload.transcript_path),
        "copilot" => match std::env::var_os("HOME") {
            Some(home) => titles::copilot(std::path::Path::new(&home), &payload.session_id),
            None => Meta::default(),
        },
        _ => Meta::default(),
    };
    Meta {
        dir: dir(&payload.cwd),
        ..found
    }
}

/// The turn a pipe reports, and the moment it was reported.
///
/// A call for the user carries when it was raised, which is what orders one
/// call against another; nothing else needs a clock, and reading one would only
/// make two identical reports look different.
fn turn(message: &str) -> (Turn, u64) {
    match message {
        WORKING_MESSAGE => (Turn::Working, 0),
        ATTENTION_MESSAGE => (Turn::Attention, now()),
        _ => (Turn::Idle, 0),
    }
}

fn hook(agent: &str, event: &str) {
    let payload = Payload::parse(&read_stdin());
    // An event naming no session describes nothing the sidebar can file.
    let Some(session) = SessionId::new(&payload.session_id) else {
        return;
    };
    let message = message(agent, event, payload.recoverable);
    // The end of a session leaves nothing to describe. Every other event
    // carries the whole record, so a title the session has taken on since it
    // started reaches the sidebar on the next event of any kind.
    if message == END_MESSAGE {
        pipe(END_MESSAGE, session.as_str());
        return;
    }
    let (turn, raised) = turn(message);
    let record = Agent {
        turn,
        raised,
        ..Agent::new(session, agent, meta(agent, &payload), pane())
    };
    pipe(message, &record.encode());
}

const USAGE: &str = "usage: zellij-wrangler hook <agent> <start|end|working|needsAttention|error>
       zellij-wrangler install-hooks [all|claude|copilot] [--uninstall]
       zellij-wrangler --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hook") => {
            let Some(agent) = args.get(1) else {
                eprintln!("zellij-wrangler hook: agent name required");
                return ExitCode::from(2);
            };
            // A session outside zellij has no sidebar to tell, and running the
            // CLI there would only produce an error nobody reads.
            if std::env::var("ZELLIJ").is_ok() {
                hook(agent, args.get(2).map(String::as_str).unwrap_or("start"));
            }
            ExitCode::SUCCESS
        }
        // The record format is printed beside the version because it, not the
        // version, is what has to match the sidebar this client reports to.
        Some("--version") | Some("-V") => {
            println!(
                "zellij-wrangler {} (record format {})",
                env!("CARGO_PKG_VERSION"),
                zellij_agent_wrangler::agents::FORMAT
            );
            ExitCode::SUCCESS
        }
        Some("install-hooks") => {
            let (said, ok) = install::run(&args[1..]);
            for line in said {
                println!("{line}");
            }
            match ok {
                true => ExitCode::SUCCESS,
                false => ExitCode::FAILURE,
            }
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_event_reports_on_its_own_pipe() {
        assert_eq!(message("claude", "start", None), START_MESSAGE);
        assert_eq!(message("claude", "end", None), END_MESSAGE);
        assert_eq!(message("claude", "working", None), WORKING_MESSAGE);
        assert_eq!(message("claude", "needsAttention", None), ATTENTION_MESSAGE);
    }

    #[test]
    fn an_unrecognised_event_registers_the_session() {
        assert_eq!(message("claude", "", None), START_MESSAGE);
        assert_eq!(message("claude", "wat", Some(true)), START_MESSAGE);
    }

    #[test]
    fn only_a_copilot_error_it_can_carry_on_from_is_still_working() {
        assert_eq!(message("copilot", "error", Some(true)), WORKING_MESSAGE);
        assert_eq!(message("copilot", "error", Some(false)), ATTENTION_MESSAGE);
        assert_eq!(message("copilot", "error", None), ATTENTION_MESSAGE);
        assert_eq!(message("claude", "error", Some(true)), ATTENTION_MESSAGE);
    }
}
