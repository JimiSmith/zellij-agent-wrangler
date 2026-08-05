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

use zellij_agent_wrangler::agents::{Agent, END_MESSAGE, START_MESSAGE};
use zellij_agent_wrangler::model::SessionId;
use zellij_agent_wrangler::payload::{label, Payload};

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

fn read_stdin() -> String {
    let mut body = String::new();
    let _ = std::io::stdin().read_to_string(&mut body);
    body
}

fn hook(agent: &str, event: &str) {
    let payload = Payload::parse(&read_stdin());
    // An event naming no session describes nothing the sidebar can file.
    let Some(session) = SessionId::new(&payload.session_id) else {
        return;
    };
    match event {
        "end" => pipe(END_MESSAGE, session.as_str()),
        // Anything else is a session announcing itself, which is also how a
        // session already known re-states where it is.
        _ => {
            let record = Agent::new(session, agent, &label(&payload.cwd, agent), pane());
            pipe(START_MESSAGE, &record.encode());
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hook") => {
            let Some(agent) = args.get(1) else {
                eprintln!("wrangler hook: agent name required");
                return ExitCode::from(2);
            };
            // A session outside zellij has no sidebar to tell, and running the
            // CLI there would only produce an error nobody reads.
            if std::env::var("ZELLIJ").is_ok() {
                hook(agent, args.get(2).map(String::as_str).unwrap_or("start"));
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: wrangler hook <agent> <start|end>");
            ExitCode::from(2)
        }
    }
}
