//! Where this program keeps its files, on each system that it runs on.
//!
//! This module derives every path, and nobody configures one. Every lookup falls
//! through to a place that exists. A machine that answers none of these lookups
//! gets the working directory rather than an error. No caller has anywhere to
//! report an error to.

use std::path::PathBuf;

/// The home directory of the user, by whatever name this system gives it.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Where the daemon keeps the records that it received.
///
/// The directory is `XDG_STATE_HOME` where that variable is set,
/// `%LOCALAPPDATA%` on Windows, and `~/.local/state` otherwise.
pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| home().map(|home| home.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("agent-wrangler")
}

/// The name of the socket that the daemon listens on.
///
/// There is one daemon per user, and the name carries the user. Everybody on the
/// machine shares the namespace that the name is claimed in. Two users must each
/// run a daemon of their own, and neither user must fail to start one.
pub fn socket_name() -> String {
    format!("agent-wrangler-{}.sock", user())
}

/// The user that this program runs as, as far as the environment says. A machine
/// that says nothing gets one shared name, which is the same answer that a
/// machine with a single user gives anyway.
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
        // Whatever the name of the user, the namespace accepts the socket name.
        assert!(name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
    }

    #[test]
    fn the_state_directory_is_this_projects_own() {
        assert!(state_dir().ends_with("agent-wrangler"));
    }
}
