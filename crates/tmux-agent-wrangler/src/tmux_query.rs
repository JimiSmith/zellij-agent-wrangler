//! Asking tmux to describe one session.
//!
//! Three commands report windows, panes and attached clients. This module runs
//! the program and separates their output. The topology module reads the fields.
//!
//! One process answers all questions. Tmux joins commands on one command line
//! with a lone semicolon, so a change costs one fork.

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

/// The mark before the attached-client report.
pub const CLIENTS_BREAK: &str = "wrangler:clients-follow";
pub const CLIENT_FORMAT: &str = "#{client_control_mode}";

/// The reports that describe one session and its attached clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyAnswer {
    /// What `list-windows` wrote.
    pub windows: String,
    /// What `list-panes -s` wrote.
    pub panes: String,
    /// What `list-clients` wrote, including this sidebar's control client.
    pub clients: String,
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
        COMMAND_BREAK,
        "display-message",
        "-p",
        CLIENTS_BREAK,
        COMMAND_BREAK,
        "list-clients",
        "-t",
        session,
        "-F",
        CLIENT_FORMAT,
    ]);
    command
}

/// Split window, pane and client reports on their markers.
/// A missing marker means the answer stopped early and describes nothing.
pub fn split_answer(output: &str) -> Option<TopologyAnswer> {
    let (windows, panes) = output.split_once(&format!("{ANSWER_BREAK}\n"))?;
    let (panes, clients) = panes.split_once(&format!("{CLIENTS_BREAK}\n"))?;
    Some(TopologyAnswer {
        windows: windows.to_string(),
        panes: panes.to_string(),
        clients: clients.to_string(),
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

/// Builds a stable-ID activation. A closed pane is not replaced by an index.
pub fn build_activation_command(
    session: &str,
    effect: &agent_wrangler_sidebar::Effect,
    panes: &[crate::topology::ReportedPane],
) -> Option<Command> {
    use agent_wrangler_sidebar::Effect;
    let mut command = Command::new(TMUX_PROGRAM);
    match effect {
        Effect::FocusPane(id) => {
            let pane = panes.iter().find(|pane| pane.id == id.as_str())?;
            let window = format!("{session}:{}", pane.window_id);
            let target = format!("{window}.{}", pane.id);
            command.args([
                "select-pane",
                "-t",
                &target,
                COMMAND_BREAK,
                "select-window",
                "-t",
                &window,
            ]);
        }
        Effect::SwitchTab(id) => {
            command.args(["select-window", "-t", &format!("{session}:{}", id.as_str())]);
        }
        _ => return None,
    }
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_client_report_fails_closed() {
        assert_eq!(
            split_answer(&format!(
                "@1\t1\t1\teditor\n{ANSWER_BREAK}\n@1\t%0\t1\tbash\n"
            )),
            None
        );
        let args = args(&build_topology_command("$3"));
        assert!(args.contains(&"list-clients".to_string()));
        assert!(crate::control::query_command_line("$3").contains("list-clients -t $3"));
    }

    #[test]
    fn activation_uses_stable_ids_not_window_indexes() {
        let panes = crate::topology::read_panes("@7\t%12\t1\teditor\n");
        let focus =
            agent_wrangler_sidebar::Effect::FocusPane(agent_wrangler_sidebar::PaneId::new("%12"));
        let pane = build_activation_command("$3", &focus, &panes).unwrap();
        assert_eq!(
            args(&pane),
            [
                "select-pane",
                "-t",
                "$3:@7.%12",
                ";",
                "select-window",
                "-t",
                "$3:@7"
            ]
        );
        assert!(build_activation_command("$3", &focus, &[]).is_none());
        let window = build_activation_command(
            "$3",
            &agent_wrangler_sidebar::Effect::SwitchTab(agent_wrangler_sidebar::TabId::new("@7")),
            &panes,
        )
        .unwrap();
        assert_eq!(args(&window), ["select-window", "-t", "$3:@7"]);
        assert!(
            build_activation_command("$3", &agent_wrangler_sidebar::Effect::Repaint, &panes)
                .is_none()
        );
    }

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn one_command_line_asks_all_questions() {
        // All reports share one process for each change.
        let command = build_topology_command("$3");
        assert_eq!(command.get_program().to_string_lossy(), "tmux");
        let args = args(&command);
        assert_eq!(args.iter().filter(|arg| *arg == COMMAND_BREAK).count(), 4);
        assert_eq!(args[0], "list-windows");
        assert!(args.contains(&"list-panes".to_string()));
    }

    #[test]
    fn all_questions_name_the_session_and_none_names_a_window() {
        // `list-panes` without `-s` lists the panes of one window. This sidebar
        // draws the whole session, so it must carry `-s`.
        let args = args(&build_topology_command("$3"));
        assert_eq!(args.iter().filter(|arg| *arg == "$3").count(), 3);
        assert!(args.contains(&"-s".to_string()));
    }

    #[test]
    fn the_questions_ask_for_the_formats_that_the_reader_expects() {
        let args = args(&build_topology_command("$3"));
        assert!(args.contains(&WINDOW_FORMAT.to_string()));
        assert!(args.contains(&PANE_FORMAT.to_string()));
    }

    #[test]
    fn the_answer_splits_on_the_marks_between_reports() {
        let output =
            format!("@1\t1\t1\teditor\n{ANSWER_BREAK}\n@1\t%0\t1\tbash\n{CLIENTS_BREAK}\n0\n1\n");
        assert_eq!(
            split_answer(&output),
            Some(TopologyAnswer {
                windows: "@1\t1\t1\teditor\n".to_string(),
                panes: "@1\t%0\t1\tbash\n".to_string(),
                clients: "0\n1\n".to_string(),
            })
        );
    }

    #[test]
    fn a_session_with_no_panes_still_splits() {
        let output = format!("@1\t1\t1\teditor\n{ANSWER_BREAK}\n{CLIENTS_BREAK}\n");
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
