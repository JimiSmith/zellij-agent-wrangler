//! The tmux control client, which reports to this sidebar that something moved.
//!
//! `tmux -C attach` is a client that speaks text rather than drawing. It sends
//! a line for every change in the session, and it runs a command written on its
//! standard input. This module holds that child, classifies the lines that come
//! back, and turns them into events.
//!
//! # The two flags that the handshake sets
//!
//! A control client is an attached client, so tmux counts it when it sizes the
//! windows of the session. Its own size is 80 by 24, because its output is a
//! pipe and not a terminal. Under the default `window-size latest`, the windows
//! of the user change to that size the moment the sidebar starts.
//! `refresh-client -f ignore-size` stops tmux counting this client.
//!
//! A control client also receives `%output` for every byte that every pane
//! writes. This sidebar reads the changes and nothing else. An agent that
//! prints a long transcript would send all of it down this pipe.
//! `refresh-client -f no-output` stops that.
//!
//! A server that does not know a flag accepts the command and does nothing, so
//! an error check finds nothing. The handshake therefore asks the server to
//! name its flags back. If the answer does not name `no-output`, this module
//! ends the control client and answers `None`. The caller then asks tmux on a
//! timer instead.
//!
//! # How tmux parses the command lines below
//!
//! Tmux parses these command lines itself. `#` starts a comment there, so every
//! format string is quoted. Tmux also reads an argument that starts with a dash
//! as a flag, so every marker below starts with a letter.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use crate::sidebar::ClientEvent;
use crate::tmux_location::TMUX_PROGRAM;
use crate::tmux_query::ANSWER_BREAK;
use crate::topology::{PANE_FORMAT, WINDOW_FORMAT};

/// The flags that this client asks the server to set on it.
const WANTED_FLAGS: &str = "no-output,ignore-size";

/// The flag that the server must name back before this module keeps the client.
///
/// The handshake checks `no-output` and not `ignore-size`. A server can know
/// one flag and not the other. A server that does not set `no-output` sends one
/// line for every byte that every pane writes. An agent that prints a long
/// transcript then costs the sidebar the time to read all of it.
const PROOF_FLAG: &str = "no-output";

/// The line that says the handshake is answered.
const HANDSHAKE_DONE: &str = "wrangler:handshake-done";

/// The line that says one whole answer to the topology question has arrived.
const ANSWER_DONE: &str = "wrangler:answer-done";

/// What one line from a control client is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlLine {
    /// A command reply starts here. The lines until the end belong to it.
    BlockStarts,
    /// A command reply ends here.
    BlockEnds,
    /// The server is going.
    ServerGone,
    /// Something in the session moved.
    SomethingMoved,
    /// A line of a command's output.
    Output(String),
}

/// What one line from a control client is.
///
/// Tmux writes every notification and every block marker with a leading percent
/// sign. A line of command output can start with one too, because a pane id
/// does. Only a line inside a block is output, so the caller tracks that and
/// this function reports what the line looks like on its own.
pub fn classify(line: &str) -> ControlLine {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.starts_with("%begin") {
        return ControlLine::BlockStarts;
    }
    if line.starts_with("%end") || line.starts_with("%error") {
        return ControlLine::BlockEnds;
    }
    if line.starts_with("%exit") {
        return ControlLine::ServerGone;
    }
    if line.starts_with('%') {
        return ControlLine::SomethingMoved;
    }
    ControlLine::Output(line.to_string())
}

/// The command that starts a control client for one session.
///
/// `-C` and not `-CC`. The second form puts the terminal in raw mode, and it
/// refuses to start with "tcgetattr failed: Not a tty" when its output is a
/// pipe. This client has no terminal of its own.
///
/// These words are a contract with another program. A mistake in them compiles,
/// and it fails only against a real tmux.
pub fn build_attach_command(session: &str) -> Command {
    let mut command = Command::new(TMUX_PROGRAM);
    command.args(["-C", "attach", "-t", session]);
    command
}

/// The commands that set the flags and ask the server to name them back.
///
/// The last line is a marker, so the reader knows that the answer is complete
/// without counting reply blocks. Tmux sends a block of its own when a client
/// attaches, and the count would be wrong by one.
pub fn handshake_commands() -> String {
    format!(
        "refresh-client -f {WANTED_FLAGS}\n\
         display-message -p '#{{client_flags}}'\n\
         display-message -p '{HANDSHAKE_DONE}'\n"
    )
}

/// The command line that asks for the shape of one session.
///
/// Each command separated by a semicolon gets a reply block of its own, so the
/// output arrives in three pieces. Joined, those pieces are exactly what the
/// same commands write when they run as a child process, marker and all.
pub fn query_command_line(session: &str) -> String {
    format!(
        "list-windows -t {session} -F '{WINDOW_FORMAT}' ; \
         display-message -p '{ANSWER_BREAK}' ; \
         list-panes -s -t {session} -F '{PANE_FORMAT}' ; \
         display-message -p '{ANSWER_DONE}'\n"
    )
}

/// Whether the server set the flags, read from the lines of the handshake.
///
/// The marker line ends the answer, and no timeout ends it. A server that named
/// the flag before the marker knows the flag. A server that reached the marker
/// without naming the flag does not know it, whatever else that server wrote.
pub fn handshake_took(lines: impl IntoIterator<Item = String>) -> bool {
    let mut took = false;
    for line in lines {
        let ControlLine::Output(said) = classify(&line) else {
            continue;
        };
        if said == HANDSHAKE_DONE {
            return took;
        }
        if said.split(',').any(|flag| flag == PROOF_FLAG) {
            took = true;
        }
    }
    false
}

/// Reads the lines of a control client and says what each one means.
///
/// The reader holds one piece of state: whether it is inside a reply block. A
/// line inside a block is output, and a line outside one is a notification.
/// Without that, a pane id at the start of a line reads as a notification.
#[derive(Default)]
pub struct ControlReader {
    inside_block: bool,
    answer: Vec<String>,
}

/// What one line of a control client asks the caller to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlOutcome {
    /// Nothing to do.
    Nothing,
    /// Something moved, so the session must be read again.
    AskAgain,
    /// One whole answer to the topology question, as the text that the same
    /// commands write when they run as a child process.
    Answered(String),
    /// The server is going.
    ServerGone,
}

impl ControlReader {
    /// Takes one line and says what it means.
    pub fn take(&mut self, line: &str) -> ControlOutcome {
        match classify(line) {
            ControlLine::BlockStarts => {
                self.inside_block = true;
                ControlOutcome::Nothing
            }
            ControlLine::BlockEnds => {
                self.inside_block = false;
                ControlOutcome::Nothing
            }
            ControlLine::ServerGone => ControlOutcome::ServerGone,
            ControlLine::SomethingMoved if !self.inside_block => ControlOutcome::AskAgain,
            // A line that starts with a percent sign INSIDE a block is output.
            // A pane id is written that way, so this arm carries real answers.
            ControlLine::SomethingMoved => {
                self.answer
                    .push(line.trim_end_matches(['\r', '\n']).to_string());
                ControlOutcome::Nothing
            }
            ControlLine::Output(said) if said == ANSWER_DONE => {
                let answer = std::mem::take(&mut self.answer).join("\n");
                ControlOutcome::Answered(format!("{answer}\n"))
            }
            ControlLine::Output(said) => {
                self.answer.push(said);
                ControlOutcome::Nothing
            }
        }
    }
}

/// A running control client, and the pipe that carries commands to it.
pub struct ControlClient {
    child: Child,
    to_tmux: ChildStdin,
}

impl ControlClient {
    /// Asks tmux for the shape of the session.
    ///
    /// Side effect: this writes on the pipe to the control client. The answer
    /// arrives later, as an event.
    pub fn ask_about_the_session(&mut self, session: &str) -> std::io::Result<()> {
        self.to_tmux
            .write_all(query_command_line(session).as_bytes())?;
        self.to_tmux.flush()
    }
}

impl Drop for ControlClient {
    /// Ends the control client.
    ///
    /// Dropping the pipe alone would do it, because tmux leaves when its
    /// standard input closes. The kill covers a tmux that does not.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts a control client, and keeps it only if the server knows the flags.
///
/// Side effect: this runs `tmux`, and it spawns a thread that reads the child
/// for as long as the client lives. It answers `None` when tmux does not run,
/// when the server refuses the attach, or when the server does not name
/// `no-output` back. The caller then asks on a timer instead.
pub fn start_control_client(session: &str, events: Sender<ClientEvent>) -> Option<ControlClient> {
    let mut child = build_attach_command(session)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut to_tmux = child.stdin.take()?;
    let from_tmux = child.stdout.take()?;
    if to_tmux.write_all(handshake_commands().as_bytes()).is_err() || to_tmux.flush().is_err() {
        let _ = child.kill();
        return None;
    }

    let mut lines = BufReader::new(from_tmux).lines();
    let mut said = Vec::new();
    for line in lines.by_ref() {
        let Ok(line) = line else { break };
        let done =
            matches!(classify(&line), ControlLine::Output(ref text) if text == HANDSHAKE_DONE);
        said.push(line);
        if done {
            break;
        }
    }
    if !handshake_took(said) {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }

    thread::spawn(move || {
        let mut reader = ControlReader::default();
        for line in lines {
            let Ok(line) = line else { break };
            let sent = match reader.take(&line) {
                ControlOutcome::Nothing => Ok(()),
                ControlOutcome::AskAgain => events.send(ClientEvent::TopologyChanged),
                ControlOutcome::Answered(text) => events.send(ClientEvent::TopologyAnswered(text)),
                ControlOutcome::ServerGone => {
                    let _ = events.send(ClientEvent::QuitRequested);
                    return;
                }
            };
            if sent.is_err() {
                return;
            }
        }
        // The child ended. Nothing will say that anything moved again, so the
        // sidebar must stop rather than hold a frame that never changes.
        let _ = events.send(ClientEvent::QuitRequested);
    });
    Some(ControlClient { child, to_tmux })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take_all(lines: &[&str]) -> Vec<ControlOutcome> {
        let mut reader = ControlReader::default();
        lines.iter().map(|line| reader.take(line)).collect()
    }

    #[test]
    fn the_attach_asks_for_control_mode_and_names_the_session() {
        // `-CC` refuses to start when its output is a pipe, with "tcgetattr
        // failed: Not a tty". This client has no terminal of its own.
        let command = build_attach_command("$3");
        assert_eq!(command.get_program().to_string_lossy(), "tmux");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-C", "attach", "-t", "$3"]);
    }

    /// Whether every `#` in a tmux command line sits inside single quotes.
    ///
    /// Tmux parses these lines itself, and an unquoted `#` starts a comment
    /// there. Everything after it is dropped, and the command then answers
    /// something else entirely with no error at all.
    fn every_hash_is_quoted(command: &str) -> bool {
        let mut quoted = false;
        for character in command.chars() {
            match character {
                '\'' => quoted = !quoted,
                '#' if !quoted => return false,
                _ => {}
            }
        }
        true
    }

    #[test]
    fn every_format_in_a_command_line_is_quoted() {
        for line in [handshake_commands(), query_command_line("$3")] {
            for command in line.lines() {
                assert!(every_hash_is_quoted(command), "unquoted # in {command:?}");
            }
        }
    }

    #[test]
    fn the_quoting_check_finds_an_unquoted_format() {
        // The check must fail on the mistake it exists to catch, or it passes
        // every command line whatever they hold.
        assert!(!every_hash_is_quoted("display-message -p #{client_flags}"));
        assert!(every_hash_is_quoted("display-message -p '#{client_flags}'"));
    }

    #[test]
    fn no_marker_starts_with_a_dash() {
        // Tmux reads an argument that starts with a dash as a flag, and refuses
        // the whole command with "invalid flag".
        for marker in [HANDSHAKE_DONE, ANSWER_DONE, ANSWER_BREAK] {
            assert!(
                marker.starts_with(|c: char| c.is_ascii_alphabetic()),
                "{marker:?}"
            );
        }
    }

    #[test]
    fn a_server_that_names_the_flag_knows_it() {
        let said = [
            "%begin 1787 296 1",
            "attached,focused,control-mode,ignore-size,no-output,UTF-8",
            "%end 1787 296 1",
            "%begin 1787 297 1",
            HANDSHAKE_DONE,
            "%end 1787 297 1",
        ];
        assert!(handshake_took(said.iter().map(|l| l.to_string())));
    }

    #[test]
    fn a_server_that_accepts_the_flag_and_ignores_it_does_not_know_it() {
        // Psmux answers this. It takes `-f` and does nothing with it, so an
        // error check finds nothing and only the named flags settle it.
        let said = [
            "%begin 1787 296 1",
            "focused",
            "%end 1787 296 1",
            "%begin 1787 297 1",
            HANDSHAKE_DONE,
            "%end 1787 297 1",
        ];
        assert!(!handshake_took(said.iter().map(|l| l.to_string())));
    }

    #[test]
    fn a_flag_name_inside_a_longer_word_is_not_the_flag() {
        let said = ["attached,not-no-output-really", HANDSHAKE_DONE];
        assert!(!handshake_took(said.iter().map(|l| l.to_string())));
    }

    #[test]
    fn a_handshake_that_never_reached_the_marker_did_not_take() {
        // A server that stopped during the handshake named no flags at all.
        // Reading that as a yes would keep a control client that reports no
        // change, and the sidebar would then hold one frame for ever.
        let said = ["%begin 1787 296 1", "no-output"];
        assert!(!handshake_took(said.iter().map(|l| l.to_string())));
    }

    #[test]
    fn a_notification_outside_a_block_asks_for_a_fresh_reading() {
        assert_eq!(
            take_all(&["%window-add @1", "%layout-change @0 6554,200x50,0,0 x -"]),
            [ControlOutcome::AskAgain, ControlOutcome::AskAgain]
        );
    }

    #[test]
    fn a_pane_id_inside_a_block_is_output_and_not_a_notification() {
        // A pane id starts with a percent sign, exactly as a notification does.
        // Only the block tells them apart, and a sidebar that got this wrong
        // would ask again for every pane that it was told about.
        let outcomes = take_all(&["%begin 1787 300 1", "@0\t%0\t1\tbash", "%end 1787 300 1"]);
        assert!(outcomes.iter().all(|o| *o == ControlOutcome::Nothing));
    }

    #[test]
    fn a_whole_answer_arrives_as_the_text_that_a_child_process_would_write() {
        // Three reply blocks, joined. The result must equal what the same
        // commands write on their own, because one reader parses both.
        let outcomes = take_all(&[
            "%begin 1787 298 1",
            "@0\t0\t0\teditor",
            "@1\t1\t1\tlogs",
            "%end 1787 298 1",
            "%begin 1787 299 1",
            ANSWER_BREAK,
            "%end 1787 299 1",
            "%begin 1787 300 1",
            "@0\t%0\t1\tbash",
            "%end 1787 300 1",
            "%begin 1787 301 1",
            ANSWER_DONE,
            "%end 1787 301 1",
        ]);
        let answered: Vec<&ControlOutcome> = outcomes
            .iter()
            .filter(|o| !matches!(o, ControlOutcome::Nothing))
            .collect();
        assert_eq!(
            answered,
            [&ControlOutcome::Answered(format!(
                "@0\t0\t0\teditor\n@1\t1\t1\tlogs\n{ANSWER_BREAK}\n@0\t%0\t1\tbash\n"
            ))]
        );
    }

    #[test]
    fn a_second_answer_carries_nothing_of_the_first() {
        let mut reader = ControlReader::default();
        for line in ["%begin 1 1 1", "one", ANSWER_DONE, "%end 1 1 1"] {
            reader.take(line);
        }
        let mut second = ControlOutcome::Nothing;
        for line in ["%begin 1 2 1", "two", ANSWER_DONE, "%end 1 2 1"] {
            let outcome = reader.take(line);
            if !matches!(outcome, ControlOutcome::Nothing) {
                second = outcome;
            }
        }
        assert_eq!(second, ControlOutcome::Answered("two\n".to_string()));
    }

    #[test]
    fn the_server_going_is_told_apart_from_everything_else() {
        assert_eq!(classify("%exit"), ControlLine::ServerGone);
        assert_eq!(classify("%exit killed"), ControlLine::ServerGone);
        assert_eq!(take_all(&["%exit"]), [ControlOutcome::ServerGone]);
    }

    #[test]
    fn the_query_asks_both_questions_and_marks_where_each_answer_stops() {
        let line = query_command_line("$3");
        assert!(line.contains("list-windows -t $3"));
        assert!(line.contains("list-panes -s -t $3"));
        assert!(line.contains(ANSWER_BREAK));
        assert!(line.contains(ANSWER_DONE));
        assert!(line.ends_with('\n'), "tmux reads one command per line");
    }
}
