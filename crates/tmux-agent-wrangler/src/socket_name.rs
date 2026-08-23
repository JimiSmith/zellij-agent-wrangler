//! The name of the socket that one tmux session reads.
//!
//! One socket serves one session. The name comes from the tmux server and from
//! the session, so every sidebar of that session derives the same name and no
//! election is necessary. Whichever one registers first makes the daemon bind
//! the name.

use crate::tmux_location::TmuxSessionId;

/// The start value of an FNV-1a hash.
const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// The multiplier of an FNV-1a hash.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The FNV-1a hash of `text`, as 64 bits.
///
/// This program writes the hash out. It does not use `DefaultHasher` from the
/// standard library. The standard library does not specify what `DefaultHasher`
/// gives back, and that output can change between Rust releases and between
/// systems.
///
/// Every sidebar of one session must derive the same socket name. Two sidebars
/// that derive two names read two sockets and never agree. The daemon also keeps
/// its clients in a file, so a name that changes under a rebuild leaves the old
/// name registered until the daemon retires it. Both faults look like a feed
/// that went quiet.
///
/// FNV-1a gives the same answer in every release and on every system.
pub fn fnv1a_hash(text: &str) -> u64 {
    let mut hash = FNV_BASIS;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The name of the socket that one tmux session reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketName(String);

impl SocketName {
    /// The name for one server and one session.
    ///
    /// The name carries a hash of the whole server socket string, and the
    /// session after it. The hash tells two servers apart when their sockets
    /// share a basename in different directories. The hash also works for a
    /// server socket that is a named pipe rather than a path, which is what
    /// psmux gives. The session keeps the name useful to a person who lists the
    /// sockets.
    ///
    /// This function answers no error and holds no check. The hash is written as
    /// eight hexadecimal digits, and [`TmuxSessionId`] already refused every session
    /// that is not a run of digits. Every character of the name is therefore an
    /// ASCII letter, a digit, a hyphen or a dot, and the namespace accepts all
    /// of those. If you let `TmuxSessionId` hold another character, this name can hold
    /// one too, and it must then be checked here.
    pub fn new(server: &str, session: &TmuxSessionId) -> SocketName {
        // The low 32 bits are enough to tell apart the few tmux servers that one
        // machine runs, and they keep the name short.
        let short = fnv1a_hash(server) as u32;
        SocketName(format!(
            "agent-wrangler-tmux-{short:08x}-{}.sock",
            session.digits()
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_id(text: &str) -> TmuxSessionId {
        TmuxSessionId::new(text).expect("a session")
    }

    #[test]
    fn the_hash_gives_the_published_answers() {
        // These three are the published FNV-1a test vectors. They prove the
        // arithmetic against a source outside this program, which a vector taken
        // from this program cannot do. The whole 64 bits are pinned, because a
        // build that lost the high half would pass a test on the low half.
        assert_eq!(fnv1a_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_hash("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_hash("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn the_name_carries_the_server_and_the_session() {
        let name = SocketName::new("/tmp/tmux-1000/default", &session_id("$3"));
        assert_eq!(name.as_str(), "agent-wrangler-tmux-592f327a-3.sock");
    }

    #[test]
    fn two_servers_in_one_directory_get_two_names() {
        // The whole reason for hashing the whole string. Two servers whose
        // sockets share a basename are told apart by the directory above them.
        let one = SocketName::new("/tmp/tmux-1000/default", &session_id("$3"));
        let two = SocketName::new("/tmp/tmux-1000/other", &session_id("$3"));
        assert_ne!(one, two);
    }

    #[test]
    fn a_server_socket_that_is_a_pipe_gets_a_name_too() {
        // Psmux reaches its server through a named pipe. A hash of the whole
        // string works for a pipe as readily as for a path.
        let name = SocketName::new(r"\\.\pipe\psmux-default", &session_id("$3"));
        assert_eq!(name.as_str(), "agent-wrangler-tmux-7f534df2-3.sock");
    }

    #[test]
    fn two_sessions_of_one_server_get_two_names() {
        let one = SocketName::new("/tmp/tmux-1000/default", &session_id("$3"));
        let two = SocketName::new("/tmp/tmux-1000/default", &session_id("$4"));
        assert_ne!(one, two);
    }

    #[test]
    fn the_namespace_accepts_every_name_that_this_builds() {
        // The same set that the daemon's own socket name keeps to. A name with
        // any other character is one that the namespace can refuse.
        for server in ["/tmp/tmux-1000/default", r"\\.\pipe\psmux-default", ""] {
            for id in ["$0", "$3", "$1024"] {
                let name = SocketName::new(server, &session_id(id));
                assert!(
                    name.as_str()
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
                    "{name:?}"
                );
            }
        }
    }

    #[test]
    fn the_name_is_short_enough_for_a_unix_socket() {
        // A unix socket path is limited to about 108 bytes, and the abstract
        // namespace does not lift that limit. The name must leave room for the
        // directory that a system puts in front of it.
        let name = SocketName::new("/tmp/tmux-1000/default", &session_id("$1024"));
        assert!(name.as_str().len() <= 40, "{name:?}");
    }
}
