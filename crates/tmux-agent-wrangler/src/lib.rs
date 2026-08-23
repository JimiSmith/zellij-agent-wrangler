//! The tmux client. It registers a socket sink, reads the state that the daemon
//! publishes to it, and writes out every record.
//!
//! This program draws no sidebar and reads no tmux topology. It writes out what
//! arrives, so that the transport is proven before anything draws with it.
//!
//! One socket serves one session. The name comes from the tmux server and from
//! the session that holds this pane, so every sidebar of that session derives
//! the same name and no election is necessary. Whichever one registers first
//! makes the daemon bind the name.
//!
//! # No assumption about the system
//!
//! Tmux does not run on Windows, but psmux does, and psmux ships a program
//! called `tmux`. This crate is built on every system. These are the places
//! where an assumption about one system could enter, and what keeps it out.
//!
//! 1. The socket name goes through `GenericNamespaced`, as every other name in
//!    this repository does. This crate builds no path, names no directory and
//!    writes no separator.
//! 2. The tmux program runs by name and never by a path. See `tmux_location::TMUX_PROGRAM`
//!    for what that costs on Windows.
//! 3. The first field of `$TMUX` is a path on unix and the name of a pipe on
//!    Windows. This crate never parses it. It hashes the whole string as bytes.
//! 4. This crate installs no signal handler. Crossterm registers one for a
//!    change of window size, but only for a program that reads its events, and
//!    this crate reads no crossterm event. The system ends this process on an
//!    interrupt.
//! 5. Nothing here spawns a shell. Every program runs by name, with its
//!    arguments already separate.
//! 6. The daemon frames each payload with `writeln!`, which writes one newline
//!    on every system, so the wire never carries a carriage return.
//! 7. There is no `cfg` for a system anywhere in this crate, and nothing here
//!    uses `std::fs` or `std::path`.
//! 8. Ratatui holds the difference between the systems for raw mode, for the
//!    alternate screen and for the size of a pane. It reads that size from the
//!    terminal, so it asks tmux nothing and costs no process.
//! 9. This crate starts a control mode client with `-C` and never with `-CC`.
//!    The second form puts the terminal in raw mode, and it refuses to start
//!    when its output is a pipe.
//!
//! A green Windows job compiles this crate. It does not run `tmux`, so nothing
//! in the build proves the Windows path from end to end.
//!
//! Psmux answers a fixed string for `#{client_flags}` and accepts
//! `refresh-client -f` without acting on it. So a sidebar there keeps no
//! control client, and it asks about the session on a timer instead.

use std::process::{ExitCode, ExitStatus};

use agent_wrangler_core::client_message::{ClientMessage, HEARTBEAT_INTERVAL};
use agent_wrangler_sidebar::Options;

use crate::heartbeat::HeartbeatSettings;

pub mod client;
pub mod control;
pub mod heartbeat;
pub mod sidebar;
pub mod socket_name;
pub mod tmux_location;
pub mod tmux_query;
pub mod topology;

#[cfg(test)]
mod test_daemon;

/// Why this program stopped.
///
/// Every failure that this program reports has a variant here, so a new one
/// cannot arrive as text that nobody planned. The message for each one is
/// written once, below.
///
/// A reader of this program's output that goes away is NOT here. That is not a
/// failure, and `client::ConnectionEnd` carries it instead.
#[derive(Debug)]
pub enum FatalError {
    /// `TMUX` is not set, so this program does not run inside tmux.
    NotInsideTmux,
    /// `TMUX` is set and `TMUX_PANE` is not.
    NoPaneId,
    /// The tmux program did not run.
    TmuxDidNotRun(std::io::Error),
    /// Tmux ran and refused the question, and this is what it said.
    TmuxRefusedQuestion(String),
    /// Tmux answered, and the answer is not a session id.
    AnswerIsNotASessionId(String),
    /// Tmux answered, and the answer does not describe a session.
    AnswerIsNotATopology(String),
    /// The terminal refused to be taken over, or refused a frame.
    TerminalRefused(std::io::Error),
    /// The thread that reads the socket gave up. This carries what it said.
    ClientStopped(String),
    /// The register program did not run.
    RegisterDidNotRun(std::io::Error),
    /// The register program ran and said no.
    RegisterFailed(ExitStatus),
    /// Nothing bound the socket inside the bounded wait.
    SocketNeverBound { name: String, why: std::io::Error },
}

impl std::fmt::Display for FatalError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FatalError::NotInsideTmux => {
                write!(out, "TMUX is not set. This program must run inside tmux.")
            }
            FatalError::NoPaneId => write!(
                out,
                "TMUX_PANE is not set. This program cannot name its own pane."
            ),
            FatalError::TmuxDidNotRun(why) => write!(out, "cannot run tmux: {why}"),
            FatalError::TmuxRefusedQuestion(said) => {
                write!(out, "tmux refused the question: {said}")
            }
            FatalError::AnswerIsNotASessionId(said) => write!(
                out,
                "tmux answered \"{said}\". A session id is a dollar sign and one or more digits."
            ),
            FatalError::AnswerIsNotATopology(said) => write!(
                out,
                "tmux answered \"{said}\", which does not describe a session."
            ),
            FatalError::TerminalRefused(why) => write!(out, "cannot draw on this terminal: {why}"),
            FatalError::ClientStopped(why) => write!(out, "{why}"),
            FatalError::RegisterDidNotRun(why) => write!(out, "cannot run agent-wrangler: {why}"),
            FatalError::RegisterFailed(status) => {
                write!(out, "agent-wrangler register said no: {status}")
            }
            FatalError::SocketNeverBound { name, why } => {
                write!(out, "nothing bound the socket {name}: {why}")
            }
        }
    }
}

/// Draws the sidebar until something stops this program, and reports the
/// outcome.
///
/// Side effect: this function takes the terminal of its pane, spawns threads,
/// and runs `tmux` and `agent-wrangler`. It gives the terminal back before it
/// returns.
///
/// A user who asked to stop is an ordinary end. This program says nothing and
/// exits with a success, because a non-zero code with nothing on the standard
/// error leaves the caller a failure and nothing to read.
///
/// On an interrupt from outside this program says nothing either. There is no
/// handler, so the system ends the process, and this program tells the daemon
/// nothing on the way out. The system closes the socket, and the heartbeat
/// stops with the process. The daemon then gives up on the client for saying
/// nothing.
pub fn run() -> ExitCode {
    // The daemon gives up on a client that says nothing, and a client that only
    // reads says nothing at all. Both ends take the interval and the line from
    // the crate that they share.
    let heartbeat = HeartbeatSettings {
        interval: HEARTBEAT_INTERVAL,
        line: ClientMessage::Beat.encode(),
    };
    match sidebar::run_sidebar(Options::default(), heartbeat) {
        Ok(()) => ExitCode::SUCCESS,
        Err(stopped) => {
            eprintln!("tmux-agent-wrangler: {stopped}");
            ExitCode::FAILURE
        }
    }
}
