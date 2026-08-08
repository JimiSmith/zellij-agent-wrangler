//! The one binary: the hook an agent invokes, the daemon those hooks feed, and
//! the installer that wires the two together.
//!
//! A hook runs inside the agent's own turn, so nothing on that path is allowed
//! to fail loudly or to take long: it says what it saw and exits, and reading
//! files is the daemon's. The only non-zero exit is a missing argument, which is
//! a misconfiguration no event could describe.

use std::io::Read;
use std::process::ExitCode;

use agent_wrangler_core::agent::{Process, FORMAT};
use agent_wrangler_core::origin::Origin;
use agent_wrangler_core::payload::Payload;

mod client;
mod daemon;
mod install;
mod paths;
mod platform;
mod proto;

use proto::{Hook, Inbound, Sink};

/// The most ancestry levels climbed looking for the agent that invoked a hook.
///
/// A hook is invoked through a shell, sometimes two, and the agent is above
/// those. Eight is far more than any of that and still stops long before the
/// root of a deep process tree.
const HOPS: u32 = 8;

/// Milliseconds since the epoch, which is what orders one call for the user
/// against another. Read here because this process sees each call exactly once.
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

/// The process of the agent that invoked this hook, found by climbing to the
/// nearest ancestor running that agent.
///
/// Side effect: reads the machine's process table. `None` when the agent is not
/// in this process's ancestry at all, which is a record nothing can later check
/// the liveness of.
fn agent_process(agent: &str) -> Option<Process> {
    let table = platform::processes();
    platform::agent_running(std::process::id(), agent, &table, HOPS)
}

/// Report one event to the daemon.
///
/// Side effect: reads the environment and the process table, and starts a daemon
/// if none is running. Failure is silent by design: an agent's turn is not the
/// place to report that a sidebar could not be told something.
fn hook(agent: &str, event: &str) {
    let origin = Origin::capture();
    // A process in no multiplexer this knows about is in nothing that could draw
    // it. Reporting it would fill the daemon with sessions nothing will ever ask
    // for.
    if origin.is_empty() {
        return;
    }
    let payload = Payload::parse(&read_stdin());
    if payload.session_id.is_empty() {
        return;
    }
    let message = Inbound::Hook {
        format: FORMAT,
        hook: Hook {
            agent: agent.to_string(),
            event: event.to_string(),
            session_id: payload.session_id,
            cwd: payload.cwd,
            transcript: payload.transcript_path,
            recoverable: payload.recoverable,
            origin: origin.encode(),
            process: agent_process(agent),
            at: now(),
        },
    };
    let _ = client::tell(&message);
}

/// The sink a client names for itself on the command line.
fn sink(kind: &str, id: &str) -> Option<Sink> {
    match kind {
        "zellij" => Some(Sink::Zellij {
            session: id.to_string(),
        }),
        "pipe" => Some(Sink::Pipe {
            path: id.to_string(),
        }),
        _ => None,
    }
}

/// What a registering client says a call should be announced with: everything
/// after `--notify`, as the words to run.
///
/// The words arrive already separated rather than as one line to be split,
/// because whoever wrote them has already had to decide where a program's path
/// ends and its arguments begin. Nothing after a `--notify` that is not there
/// is nothing to announce with.
fn notify(args: &[String]) -> Vec<String> {
    match args.first().map(String::as_str) {
        Some("--notify") => args[1..].to_vec(),
        _ => Vec::new(),
    }
}

const USAGE: &str = "usage: agent-wrangler hook <agent> <start|end|working|needsAttention|error>
       agent-wrangler daemon
       agent-wrangler register <zellij|pipe> <session|path> [--notify <command> [argument...]]
       agent-wrangler seen <session>
       agent-wrangler agents
       agent-wrangler install-hooks [all|claude|copilot] [--uninstall]
       agent-wrangler --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hook") => {
            let Some(agent) = args.get(1) else {
                eprintln!("agent-wrangler hook: agent name required");
                return ExitCode::from(2);
            };
            hook(agent, args.get(2).map(String::as_str).unwrap_or("start"));
            ExitCode::SUCCESS
        }
        Some("daemon") => {
            let _ = daemon::run();
            ExitCode::SUCCESS
        }
        Some("register") => {
            let (Some(kind), Some(id)) = (args.get(1), args.get(2)) else {
                eprintln!("agent-wrangler register: a kind and an id are required");
                return ExitCode::from(2);
            };
            let Some(sink) = sink(kind, id) else {
                eprintln!("agent-wrangler register: {kind} is not a kind of client");
                return ExitCode::from(2);
            };
            let _ = client::tell(&Inbound::Register {
                format: FORMAT,
                sink,
                notify: notify(&args[3..]),
            });
            ExitCode::SUCCESS
        }
        Some("seen") => {
            let Some(session) = args.get(1) else {
                eprintln!("agent-wrangler seen: session id required");
                return ExitCode::from(2);
            };
            let _ = client::tell(&Inbound::Seen {
                session: session.clone(),
            });
            ExitCode::SUCCESS
        }
        // What the daemon holds, as it would send it. For looking at what is
        // there when a row is not what was expected.
        Some("agents") => match client::ask() {
            Ok(records) => {
                println!("{records}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("agent-wrangler agents: {error}");
                ExitCode::FAILURE
            }
        },
        // The record format is printed beside the version because it, not the
        // version, is what has to match at both ends of the wire.
        Some("--version") | Some("-V") => {
            println!(
                "agent-wrangler {} (record format {FORMAT})",
                env!("CARGO_PKG_VERSION"),
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
    fn each_kind_of_client_names_its_own_sink() {
        assert_eq!(
            sink("zellij", "proto"),
            Some(Sink::Zellij {
                session: "proto".to_string()
            })
        );
        assert_eq!(
            sink("pipe", "/tmp/w"),
            Some(Sink::Pipe {
                path: "/tmp/w".to_string()
            })
        );
    }

    #[test]
    fn a_kind_of_client_this_cannot_reach_is_not_registered() {
        assert_eq!(sink("carrier-pigeon", "coop"), None);
        assert_eq!(sink("", ""), None);
    }

    fn words(args: &[&str]) -> Vec<String> {
        notify(
            &args
                .iter()
                .map(|arg| arg.to_string())
                .collect::<Vec<String>>(),
        )
    }

    #[test]
    fn everything_after_the_flag_is_what_to_announce_with() {
        assert_eq!(
            words(&["--notify", "notify-send", "--urgency", "low"]),
            ["notify-send", "--urgency", "low"]
        );
    }

    #[test]
    fn a_client_that_asks_for_nothing_announces_nothing() {
        assert!(words(&[]).is_empty());
        assert!(words(&["--notify"]).is_empty());
        // Words with no flag in front of them are not a notifier by accident.
        assert!(words(&["notify-send"]).is_empty());
    }
}
