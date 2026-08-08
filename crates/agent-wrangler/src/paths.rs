//! Where this keeps its things, on each of the systems it runs on.
//!
//! Every path is derived rather than configured, and every lookup falls through
//! to something that exists: a machine that answers none of these gets the
//! working directory rather than an error, because none of the callers have
//! anywhere to report one to.

use std::path::PathBuf;

/// The user's home, by whichever name this system gives it.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Where the daemon keeps the records it has been told about.
///
/// `XDG_STATE_HOME` where it is set, `%LOCALAPPDATA%` on Windows, and
/// `~/.local/state` otherwise.
pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| home().map(|home| home.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("agent-wrangler")
}

/// The name of the socket the daemon listens on.
///
/// One daemon per user, and the name carries the user because the namespace it
/// is claimed in is shared by everyone on the machine: two users each running
/// their own daemon must not be one user failing to start theirs.
pub fn socket_name() -> String {
    format!("agent-wrangler-{}.sock", user())
}

/// Who this is running as, as far as the environment says. A machine that says
/// nothing gets one shared name, which is the same answer a single-user machine
/// would have given anyway.
fn user() -> String {
    let name = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    match cleaned.is_empty() {
        true => "default".to_string(),
        false => cleaned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_is_named_for_a_user_and_nothing_else() {
        let name = socket_name();
        assert!(name.starts_with("agent-wrangler-"), "{name}");
        assert!(name.ends_with(".sock"), "{name}");
        // Whatever the user is called, the name is one the namespace accepts.
        assert!(name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
    }

    #[test]
    fn the_state_directory_is_this_projects_own() {
        assert!(state_dir().ends_with("agent-wrangler"));
    }
}
