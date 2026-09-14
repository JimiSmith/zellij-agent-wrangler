//! Where this program runs: the tmux server that holds it, the pane that it
//! sits in, and the session that holds the pane.
//!
//! The session comes from the pane and not from `$TMUX`. The third field of
//! `$TMUX` names the session that this process started in, and that field goes
//! stale when a window moves to another session. `TMUX_PANE` stays true.

use std::process::Command;

use crate::FatalError;

/// The program that answers for the tmux topology.
///
/// The name is not configurable in this story. The program runs by name and
/// never by path, so the system resolves it.
///
/// On Windows `Command` reaches `CreateProcessW`, which searches the path and
/// appends `.exe`. It does not do the wider search that a shell does, so a
/// `tmux.cmd` or a script of that name is out of reach.
///
/// Psmux ships a real `tmux.exe`, which is a checked fact and not an
/// assumption. The crates.io API lists the `psmux` crate's `bin_names` as
/// `pmux`, `psmux` and `tmux`. Those are Cargo binary targets, so they build to
/// `.exe` files, and the project README shows `target\release\tmux.exe` in its
/// release output.
///
/// Sources: <https://crates.io/crates/psmux> and
/// <https://github.com/psmux/psmux>.
pub const TMUX_PROGRAM: &str = "tmux";

/// The tmux session that holds one pane, as `#{session_id}` names it.
///
/// Tmux writes a session id as a dollar sign and one or more digits. This type
/// holds the digits alone, because the dollar sign has no place in a socket
/// name.
///
/// This type is the only boundary that the socket name rests on. Nothing else
/// can build one, so every value here is a run of ASCII digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxSessionId(String);

impl TmuxSessionId {
    /// The session that `text` names, or `None` for text that is not a session
    /// id.
    ///
    /// This function is what makes [`crate::socket_name::SocketName::new`] infallible. If
    /// you admit another character here, a socket name can hold a character that
    /// the namespace refuses, and the name must then be checked there.
    pub fn new(text: &str) -> Option<TmuxSessionId> {
        let digits = text.strip_prefix('$')?;
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(TmuxSessionId(digits.to_string()))
    }

    /// The digits of the id, without the dollar sign.
    pub fn digits(&self) -> &str {
        &self.0
    }

    /// The id as tmux writes it, which is what a `-t` argument takes.
    ///
    /// A bare run of digits names a window index to tmux and not a session.
    /// The dollar sign therefore makes the target name the session.
    pub fn as_target(&self) -> String {
        format!("${}", self.0)
    }
}

/// The tmux server that holds this process, and the pane that it runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxLocation {
    /// The server socket, which is the first field of `$TMUX`.
    ///
    /// This is a string and never a path. On Windows it is the name of a pipe.
    /// Nothing here reads it for meaning.
    server_socket: String,
    /// The pane, as `TMUX_PANE` gives it.
    pane_id: String,
}

impl TmuxLocation {
    /// Where the environment says that this process runs.
    ///
    /// Side effect: this function reads the environment. A hook and a sidebar
    /// both run inside the pane that they belong to, so these variables are that
    /// pane's variables.
    pub fn from_environment() -> Result<TmuxLocation, FatalError> {
        TmuxLocation::from_variables(|name| std::env::var(name).ok())
    }

    /// The same, from anything that can answer for a variable.
    pub fn from_variables(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<TmuxLocation, FatalError> {
        let tmux = lookup("TMUX").unwrap_or_default();
        // The first field of `$TMUX` is the server socket. The other two fields
        // are a process id and a session, and this program reads neither.
        let server = tmux.split(',').next().unwrap_or_default();
        if server.is_empty() {
            return Err(FatalError::NotInsideTmux);
        }
        let pane = lookup("TMUX_PANE").unwrap_or_default();
        if pane.is_empty() {
            return Err(FatalError::NoPaneId);
        }
        Ok(TmuxLocation {
            server_socket: server.to_string(),
            pane_id: pane.to_string(),
        })
    }

    /// The server socket that holds this process.
    pub fn server_socket(&self) -> &str {
        &self.server_socket
    }

    /// The session that holds this pane.
    ///
    /// Side effect: this function runs `tmux`. The caller asks again on every
    /// connection, so a window that moved to another session names the right
    /// socket as soon as the daemon blinks.
    pub fn read_session(&self) -> Result<TmuxSessionId, FatalError> {
        let answer = build_session_id_command(&self.pane_id)
            .output()
            .map_err(FatalError::TmuxDidNotRun)?;
        if !answer.status.success() {
            return Err(FatalError::TmuxRefusedQuestion(trimmed_output(
                &answer.stderr,
            )));
        }
        let said = trimmed_output(&answer.stdout);
        TmuxSessionId::new(&said).ok_or(FatalError::AnswerIsNotASessionId(said))
    }
}

/// A pane-local sidebar flag, removed on ordinary scope exit.
/// This is not an ownership record. A crash can leave a surviving pane marked.
pub struct SidebarRegistration {
    location: TmuxLocation,
}

impl SidebarRegistration {
    /// Sets this pane's flag before topology queries or terminal setup begin.
    /// A host that rejects pane-local options reports an error, not a fallback.
    pub fn register(location: &TmuxLocation) -> Result<Self, FatalError> {
        let answer = build_sidebar_flag_command(location, false)
            .output()
            .map_err(FatalError::TmuxDidNotRun)?;
        if !answer.status.success() {
            return Err(FatalError::TmuxRefusedQuestion(trimmed_output(
                &answer.stderr,
            )));
        }
        Ok(Self {
            location: location.clone(),
        })
    }
}

impl Drop for SidebarRegistration {
    fn drop(&mut self) {
        // A removed pane's stable ID fails without changing any other pane.
        // Cleanup must not replace the result of the sidebar run.
        let _ = build_sidebar_flag_command(&self.location, true).output();
    }
}

/// Builds a pane-local flag change on the server captured at startup.
fn build_sidebar_flag_command(location: &TmuxLocation, remove: bool) -> Command {
    let mut command = Command::new(TMUX_PROGRAM);
    command.args([
        "-S",
        location.server_socket(),
        "set-option",
        if remove { "-pu" } else { "-p" },
        "-t",
        &location.pane_id,
        "@agent-wrangler-sidebar",
    ]);
    if !remove {
        command.arg("1");
    }
    command
}

/// The command that asks tmux which session holds one pane.
///
/// The words are built here and run by the caller. A test can therefore read the
/// program and its arguments on any system, with tmux installed or not. These
/// words are a contract with another program, and a mistake in them compiles and
/// passes every test that does not read them.
fn build_session_id_command(pane: &str) -> Command {
    let mut command = Command::new(TMUX_PROGRAM);
    command.args(["display-message", "-p", "-t", pane, "#{session_id}"]);
    command
}

/// What a program wrote on one of its streams, as one line of text.
fn trimmed_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable_lookup(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(known, _)| known == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn sidebar_registration_targets_only_its_pane_on_its_server() {
        for server in ["/tmp/other tmux/socket", r"\\.\pipe\psmux-default"] {
            let location = TmuxLocation::from_variables(variable_lookup(&[
                ("TMUX", &format!("{server},3242,99")),
                ("TMUX_PANE", "%12"),
            ]))
            .unwrap();
            for (remove, flag, value) in [(false, "-p", vec!["1"]), (true, "-pu", vec![])] {
                let command = build_sidebar_flag_command(&location, remove);
                assert_eq!(command.get_program(), "tmux");
                let args: Vec<_> = command
                    .get_args()
                    .map(|arg| arg.to_str().unwrap())
                    .collect();
                let mut expected = vec![
                    "-S",
                    server,
                    "set-option",
                    flag,
                    "-t",
                    "%12",
                    "@agent-wrangler-sidebar",
                ];
                expected.extend(value);
                assert_eq!(args, expected);
            }
        }
    }

    #[test]
    #[ignore = "requires a real tmux server; run explicitly with --ignored"]
    fn sidebar_registration_cleans_only_its_pane_and_tolerates_removed_panes() {
        struct TestServer(String);
        impl TestServer {
            fn output(&self, args: &[&str]) -> String {
                let output = Command::new(TMUX_PROGRAM)
                    .args(["-L", &self.0])
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    trimmed_output(&output.stderr)
                );
                trimmed_output(&output.stdout)
            }
        }
        impl Drop for TestServer {
            fn drop(&mut self) {
                let _ = Command::new(TMUX_PROGRAM)
                    .args(["-L", &self.0, "kill-server"])
                    .output();
            }
        }
        let server = TestServer(format!("wrangler-flag-test-{}", std::process::id()));
        server.output(&["-f", "", "new-session", "-d", "-s", "flag-test"]);
        let socket = server.output(&["display-message", "-p", "#{socket_path}"]);
        let own = server.output(&["display-message", "-p", "#{pane_id}"]);
        let peer = server.output(&["split-window", "-d", "-P", "-F", "#{pane_id}"]);
        let location = TmuxLocation::from_variables(variable_lookup(&[
            ("TMUX", &format!("{socket},0,99")),
            ("TMUX_PANE", &own),
        ]))
        .unwrap();
        server.output(&[
            "set-option",
            "-p",
            "-t",
            &peer,
            "@agent-wrangler-sidebar",
            "1",
        ]);
        let registration = SidebarRegistration::register(&location).unwrap();
        assert_eq!(
            server.output(&["show-options", "-pv", "-t", &own, "@agent-wrangler-sidebar"]),
            "1"
        );
        drop(registration);
        assert_eq!(
            server.output(&[
                "show-options",
                "-pqv",
                "-t",
                &own,
                "@agent-wrangler-sidebar"
            ]),
            ""
        );
        assert_eq!(
            server.output(&[
                "show-options",
                "-pv",
                "-t",
                &peer,
                "@agent-wrangler-sidebar"
            ]),
            "1"
        );
        let registration = SidebarRegistration::register(&location).unwrap();
        server.output(&["kill-pane", "-t", &own]);
        drop(registration);
        assert_eq!(
            server.output(&[
                "show-options",
                "-pv",
                "-t",
                &peer,
                "@agent-wrangler-sidebar"
            ]),
            "1"
        );
        assert!(SidebarRegistration::register(&location).is_err());
    }

    #[test]
    fn a_session_id_is_a_dollar_sign_and_digits() {
        assert_eq!(
            TmuxSessionId::new("$3").map(|s| s.digits().to_string()),
            Some("3".to_string())
        );
        assert_eq!(
            TmuxSessionId::new("$1024").map(|s| s.digits().to_string()),
            Some("1024".to_string())
        );
    }

    #[test]
    fn anything_else_is_not_a_session_id() {
        // The socket name rests on this refusal. Every character that gets past
        // here goes into a name that the namespace must accept.
        assert_eq!(TmuxSessionId::new("3"), None, "no dollar sign");
        assert_eq!(TmuxSessionId::new("$"), None, "no digits");
        assert_eq!(TmuxSessionId::new("$3a"), None, "a letter");
        assert_eq!(TmuxSessionId::new("$3/x"), None, "a separator");
        assert_eq!(TmuxSessionId::new("$ 3"), None, "a space");
        assert_eq!(TmuxSessionId::new(""), None, "nothing at all");
        assert_eq!(TmuxSessionId::new("$-1"), None, "a sign");
    }

    #[test]
    fn the_question_to_tmux_names_the_pane_and_asks_for_the_session() {
        // These words are the contract with tmux. A mistake in them compiles,
        // and it fails only against a real tmux.
        let command = build_session_id_command("%12");
        assert_eq!(command.get_program().to_string_lossy(), "tmux");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            ["display-message", "-p", "-t", "%12", "#{session_id}"]
        );
    }

    #[test]
    fn the_session_comes_from_the_pane_and_never_from_the_variable() {
        // The third field of `$TMUX` names the session that this process started
        // in, and it goes stale when a window moves. The question must therefore
        // carry the pane and nothing from `$TMUX`.
        let command = build_session_id_command("%12");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"%12".to_string()));
        assert!(args.iter().any(|arg| arg == "#{session_id}"));
    }

    #[test]
    fn the_server_is_the_first_field_of_the_variable() {
        let location = TmuxLocation::from_variables(variable_lookup(&[
            ("TMUX", "/tmp/tmux-1000/default,3242,0"),
            ("TMUX_PANE", "%12"),
        ]))
        .expect("a location");
        assert_eq!(location.server_socket(), "/tmp/tmux-1000/default");
    }

    #[test]
    fn a_server_socket_that_is_a_pipe_survives_as_it_stands() {
        // On Windows the server socket is a named pipe and not a path. Nothing
        // here reads it for meaning, so it travels word for word.
        let location = TmuxLocation::from_variables(variable_lookup(&[
            ("TMUX", r"\\.\pipe\psmux-default,3242,0"),
            ("TMUX_PANE", "%1"),
        ]))
        .expect("a location");
        assert_eq!(location.server_socket(), r"\\.\pipe\psmux-default");
    }

    #[test]
    fn a_process_outside_tmux_is_refused() {
        assert!(matches!(
            TmuxLocation::from_variables(variable_lookup(&[])),
            Err(FatalError::NotInsideTmux)
        ));
        assert!(matches!(
            TmuxLocation::from_variables(variable_lookup(&[("TMUX", ""), ("TMUX_PANE", "%1")])),
            Err(FatalError::NotInsideTmux)
        ));
        // A variable with no server in its first field names no server at all.
        assert!(matches!(
            TmuxLocation::from_variables(variable_lookup(&[
                ("TMUX", ",3242,0"),
                ("TMUX_PANE", "%1")
            ])),
            Err(FatalError::NotInsideTmux)
        ));
    }

    #[test]
    fn a_process_with_no_pane_is_refused() {
        assert!(matches!(
            TmuxLocation::from_variables(variable_lookup(&[(
                "TMUX",
                "/tmp/tmux-1000/default,3242,0"
            )])),
            Err(FatalError::NoPaneId)
        ));
    }
}
