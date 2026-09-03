//! The one binary: the hook that an agent invokes, the daemon that those hooks
//! feed, and the installer that wires the two together.
//!
//! A hook runs inside the turn of the agent, so nothing on that path can fail
//! loudly or take long. The hook says what it saw and then exits, and the daemon
//! reads the files. The only non-zero exit is a missing argument, which is a
//! misconfiguration that no event describes.
//!
//! Every subcommand starts in [`run`], which is what makes this crate a library.
//! The program links twice under two names. The two names differ in how they
//! link and in nothing that they do.

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

use proto::{DeliveryTarget, Hook, Inbound};

/// The largest number of ancestry levels that the search for the agent climbs.
///
/// A shell invokes a hook, sometimes two shells do, and the agent sits above
/// them. Eight is far more than any of that, and eight still stops long before
/// the root of a deep process tree.
const HOPS: u32 = 8;

/// The milliseconds since the epoch, which order one call for the user against
/// another. This process reads the clock, because it sees each call once.
pub(crate) fn now() -> u64 {
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

/// The process of the agent that invoked this hook. The search climbs to the
/// nearest ancestor that runs that agent.
///
/// Side effect: this function reads the process table of the machine. When the
/// agent is not in the ancestry of this process at all, the answer is `None`.
/// Nothing can check the liveness of such a record later.
fn agent_process(agent: &str) -> Option<Process> {
    let table = platform::processes();
    platform::agent_running(std::process::id(), agent, &table, HOPS)
}

/// Reports one event to the daemon.
///
/// Side effect: this function reads the environment and the process table. If no
/// daemon runs, this function starts one. A failure is silent by design. The
/// turn of an agent is not the place to report that a sidebar heard nothing.
fn hook(agent: &str, event: &str) {
    let origin = Origin::capture();
    // A process in no multiplexer that this program knows about is in nothing
    // that draws it. A report of that process fills the daemon with sessions
    // that nobody ever asks for.
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
            agent_id: payload.agent_id,
            agent_type: payload.agent_type,
            origin: origin.encode(),
            process: agent_process(agent),
            at: now(),
        },
    };
    let _ = client::tell(&message);
}

/// The sink that a client names for itself on the command line.
///
/// A name with nothing in it is refused here, so it never reaches the wire. The
/// daemon cannot bind such a name, and the person who typed it is at this end.
fn sink(kind: &str, id: &str) -> Option<DeliveryTarget> {
    if id.is_empty() {
        return None;
    }
    match kind {
        "zellij" => Some(DeliveryTarget::Zellij {
            session: id.to_string(),
        }),
        "socket" => Some(DeliveryTarget::Socket {
            name: id.to_string(),
        }),
        _ => None,
    }
}

/// What a client that registers announces a call with: everything after
/// `--notify`, as the words to run.
///
/// The words arrive already separate, and not as one line to split. The author
/// of the words already decided where the path of the program ends and where its
/// arguments begin. An absent `--notify`, and an empty run of words after it,
/// both announce nothing.
fn notify(args: &[String]) -> Vec<String> {
    match args.first().map(String::as_str) {
        Some("--notify") => args[1..].to_vec(),
        _ => Vec::new(),
    }
}

const USAGE: &str = "usage: agent-wrangler hook <agent> <start|end|working|needsAttention|error>
       agent-wrangler daemon
       agent-wrangler register <zellij|socket> <session|name> [--notify <command> [argument...]]
       agent-wrangler seen <session>
       agent-wrangler agents
       agent-wrangler monitor
       agent-wrangler install-hooks [all|claude|copilot] [--uninstall]
       agent-wrangler --version";

/// Does what the command line asks for, and reports the outcome.
pub fn run() -> ExitCode {
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
        // What the daemon does, one record to a line, for as long as this
        // subcommand runs. It answers three questions that a snapshot cannot.
        // How often does the state go out? What sent it out? How long did each
        // client take to reach?
        Some("monitor") => {
            let mut out = std::io::stdout().lock();
            match client::watch(&mut out) {
                Ok(()) => ExitCode::SUCCESS,
                // A pipe that went away is the ordinary end of this
                // subcommand. The reader was a `head`, or a pager that the
                // user left.
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("agent-wrangler monitor: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        // What the daemon holds, in the form that it sends. When a row is not
        // the expected one, this subcommand shows the true state.
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
        // This subcommand prints the record format beside the version. The
        // record format, and not the version, must match at both ends of the
        // wire.
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
            Some(DeliveryTarget::Zellij {
                session: "proto".to_string()
            })
        );
        assert_eq!(
            sink("socket", "wrangler-tmux-work"),
            Some(DeliveryTarget::Socket {
                name: "wrangler-tmux-work".to_string()
            })
        );
    }

    #[test]
    fn a_kind_of_client_this_cannot_reach_is_not_registered() {
        assert_eq!(sink("carrier-pigeon", "coop"), None);
        assert_eq!(sink("", ""), None);
    }

    #[test]
    fn a_sink_with_no_name_is_refused_at_the_command_line() {
        // The daemon cannot bind such a name, and the person who typed it is at
        // this end. A refusal here is a message, and a refusal there is silence.
        assert_eq!(sink("socket", ""), None);
        assert_eq!(sink("zellij", ""), None);
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
