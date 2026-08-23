//! Asking tmux to describe one session.
//!
//! Two commands ask the two questions: `list-windows` names the windows, and
//! `list-panes -s` names every pane of the session. Nothing here reads the
//! answers for meaning. This module runs the program and splits the output into
//! the two halves that it holds.
//!
//! One process answers both questions. Tmux joins two commands on one command
//! line with a lone semicolon, so a change costs one fork and not two.

use std::process::Command;

use crate::tmux_location::TMUX_PROGRAM;
use crate::topology::{PANE_FORMAT, WINDOW_FORMAT};
use crate::FatalError;

/// The word that separates two commands on one tmux command line.
///
/// A shell would read a bare semicolon as its own separator, so tmux takes the
/// semicolon as one argument. Nothing here spawns a shell, so the argument
/// reaches tmux as it stands.
const COMMAND_BREAK: &str = ";";

/// The mark that tells the two answers apart in one stream of output.
///
/// Tmux writes the output of both commands to one stream with nothing between
/// them. A `display-message` between the two prints this line, so the split
/// below knows where the windows stop and the panes start.
///
/// A window name or a pane title could hold this text. Neither can be a whole
/// line of it, because tmux writes more fields on every line that it reports.
///
/// The mark starts with a letter. Tmux reads an argument that starts with a
/// dash as a flag, and refuses the whole command with "invalid flag".
pub const ANSWER_BREAK: &str = "wrangler:panes-follow";

/// The two answers that describe one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyAnswer {
    /// What `list-windows` wrote.
    pub windows: String,
    /// What `list-panes -s` wrote.
    pub panes: String,
}

/// The command that asks tmux to describe one session.
///
/// The words are built here and run by the caller. A test can therefore read
/// the program and its arguments on any system, with tmux installed or not.
/// These words are a contract with another program, and a mistake in them
/// compiles and passes every test that does not read them.
pub fn build_topology_command(session: &str) -> Command {
    let mut command = Command::new(TMUX_PROGRAM);
    command.args([
        "list-windows",
        "-t",
        session,
        "-F",
        WINDOW_FORMAT,
        COMMAND_BREAK,
        "display-message",
        "-p",
        ANSWER_BREAK,
        COMMAND_BREAK,
        "list-panes",
        "-s",
        "-t",
        session,
        "-F",
        PANE_FORMAT,
    ]);
    command
}

/// The two halves of one answer, split on the mark between them.
///
/// Everything before the mark came from `list-windows`. Everything after it
/// came from `list-panes`. An answer with no mark is an answer that stopped
/// early, and it describes nothing.
pub fn split_answer(output: &str) -> Option<TopologyAnswer> {
    let (windows, panes) = output.split_once(&format!("{ANSWER_BREAK}\n"))?;
    Some(TopologyAnswer {
        windows: windows.to_string(),
        panes: panes.to_string(),
    })
}

/// Asks tmux to describe one session, and reads the answer.
///
/// Side effect: this function runs `tmux`. It costs one process for each call.
pub fn read_topology(session: &str) -> Result<TopologyAnswer, FatalError> {
    let answer = build_topology_command(session)
        .output()
        .map_err(FatalError::TmuxDidNotRun)?;
    if !answer.status.success() {
        return Err(FatalError::TmuxRefusedQuestion(
            String::from_utf8_lossy(&answer.stderr).trim().to_string(),
        ));
    }
    let said = String::from_utf8_lossy(&answer.stdout).into_owned();
    split_answer(&said).ok_or(FatalError::AnswerIsNotATopology(said))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn one_command_line_asks_both_questions() {
        // One process for both answers. Two processes would cost twice as much
        // for every change that a user makes.
        let command = build_topology_command("$3");
        assert_eq!(command.get_program().to_string_lossy(), "tmux");
        let args = args(&command);
        assert_eq!(args.iter().filter(|arg| *arg == COMMAND_BREAK).count(), 2);
        assert_eq!(args[0], "list-windows");
        assert!(args.contains(&"list-panes".to_string()));
    }

    #[test]
    fn both_questions_name_the_session_and_neither_names_a_window() {
        // `list-panes` without `-s` lists the panes of one window. This sidebar
        // draws the whole session, so it must carry `-s`.
        let args = args(&build_topology_command("$3"));
        assert_eq!(args.iter().filter(|arg| *arg == "$3").count(), 2);
        assert!(args.contains(&"-s".to_string()));
    }

    #[test]
    fn the_questions_ask_for_the_formats_that_the_reader_expects() {
        let args = args(&build_topology_command("$3"));
        assert!(args.contains(&WINDOW_FORMAT.to_string()));
        assert!(args.contains(&PANE_FORMAT.to_string()));
    }

    #[test]
    fn the_answer_splits_on_the_mark_between_the_two_halves() {
        let output = format!("@1\t1\t1\teditor\n{ANSWER_BREAK}\n@1\t%0\t1\tbash\n");
        assert_eq!(
            split_answer(&output),
            Some(TopologyAnswer {
                windows: "@1\t1\t1\teditor\n".to_string(),
                panes: "@1\t%0\t1\tbash\n".to_string(),
            })
        );
    }

    #[test]
    fn a_session_with_no_panes_still_splits() {
        let output = format!("@1\t1\t1\teditor\n{ANSWER_BREAK}\n");
        let split = split_answer(&output).expect("a split");
        assert_eq!(split.panes, "");
        assert_eq!(split.windows, "@1\t1\t1\teditor\n");
    }

    #[test]
    fn the_mark_starts_with_a_letter() {
        // Tmux reads an argument that starts with a dash as a flag. A mark such
        // as `--follow--` makes tmux refuse the whole command with "invalid
        // flag", and the sidebar then draws nothing with no reason on the pane.
        assert!(
            ANSWER_BREAK.starts_with(|c: char| c.is_ascii_alphabetic()),
            "{ANSWER_BREAK:?}"
        );
    }

    #[test]
    fn an_answer_that_stopped_before_the_mark_describes_nothing() {
        // Reading such an answer would draw a session with windows and no
        // panes. That describes the session wrongly, and the user cannot tell
        // it from a session that really holds no panes.
        assert_eq!(split_answer("@1\t1\t1\teditor\n"), None);
        assert_eq!(split_answer(""), None);
    }
}
