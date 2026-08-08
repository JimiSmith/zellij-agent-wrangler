//! Where an agent reported itself from, as its own environment described it.
//!
//! The variables are captured verbatim and never read for meaning here. What
//! `ZELLIJ_PANE_ID` or `TMUX_PANE` points at is known only to the thing drawing
//! that multiplexer, so this carries the strings and lets it decide.
//!
//! Only the variables in [`LOCATION_VARS`] are kept. An agent's environment
//! holds credentials and a great deal else that is nobody's business; taking a
//! named few is what keeps this a location rather than a copy of the process.

use std::collections::BTreeMap;

/// The variables that say which multiplexer a process is in, and where.
///
/// The order is part of the wire format, because a captured set is written as
/// its values in this order and nothing else. A new variable may be appended; a
/// reordering silently moves every value to the wrong name.
pub const LOCATION_VARS: &[&str] = &[
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    "TMUX",
    "TMUX_PANE",
];

/// The separator between one captured value and the next. A control character,
/// so no path, session name or pane id can contain one.
const UNIT: char = '\u{1f}';

/// What one process's environment said about where it is.
///
/// The run is always as long as the table, so one built any of the three ways
/// compares equal to the same one built another way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// One entry per [`LOCATION_VARS`] name, in that order. Empty means the
    /// variable was not set, which is how a process outside every multiplexer
    /// this knows describes itself.
    values: Vec<String>,
}

/// Replace anything that would split the run, or the record it sits in, with a
/// space. A location value cannot legitimately hold one of these.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Default for Origin {
    fn default() -> Self {
        Origin::from(|_| None)
    }
}

impl Origin {
    /// What the current process's environment says about where it is.
    ///
    /// Side effect: reads the environment. A hook runs as a descendant of the
    /// pane it belongs to, so these are that pane's variables.
    pub fn capture() -> Self {
        Origin::from(|name| std::env::var(name).ok())
    }

    /// The same, from anything that can answer for a variable.
    pub fn from(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        Origin {
            values: LOCATION_VARS
                .iter()
                .map(|name| lookup(name).map(|value| clean(&value)).unwrap_or_default())
                .collect(),
        }
    }

    /// What was captured for one variable, or `None` for one that was not set.
    /// A name that is not captured at all answers the same way.
    pub fn get(&self, name: &str) -> Option<&str> {
        let index = LOCATION_VARS.iter().position(|known| *known == name)?;
        match self.values.get(index).map(String::as_str) {
            Some("") | None => None,
            Some(value) => Some(value),
        }
    }

    /// Whether nothing was captured, which says the process was in no
    /// multiplexer this knows about.
    pub fn is_empty(&self) -> bool {
        self.values.iter().all(String::is_empty)
    }

    /// Every captured variable by name, for saying what was seen rather than
    /// for deciding anything.
    pub fn named(&self) -> BTreeMap<&str, &str> {
        LOCATION_VARS
            .iter()
            .filter_map(|name| self.get(name).map(|value| (*name, value)))
            .collect()
    }

    /// The values in [`LOCATION_VARS`] order, separated by [`UNIT`].
    pub fn encode(&self) -> String {
        self.values.join(&UNIT.to_string())
    }

    /// Read back what `encode` wrote.
    ///
    /// A run shorter than the table is padded and a longer one is cut, so a
    /// record written where the table had a different length is read for the
    /// names both ends agree on rather than refused.
    pub fn decode(text: &str) -> Self {
        let mut values: Vec<String> = match text.is_empty() {
            true => Vec::new(),
            false => text.split(UNIT).map(str::to_string).collect(),
        };
        values.resize(LOCATION_VARS.len(), String::new());
        Origin { values }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(pairs: &[(&str, &str)]) -> Origin {
        Origin::from(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn a_captured_variable_comes_back_by_name() {
        let origin = origin(&[("ZELLIJ_SESSION_NAME", "wrangler-proto")]);
        assert_eq!(origin.get("ZELLIJ_SESSION_NAME"), Some("wrangler-proto"));
    }

    #[test]
    fn a_variable_that_was_not_set_is_not_there() {
        assert_eq!(origin(&[("ZELLIJ", "0")]).get("ZELLIJ_PANE_ID"), None);
        // An empty value is a variable that says nothing, not a value of "".
        assert_eq!(origin(&[("ZELLIJ", "")]).get("ZELLIJ"), None);
    }

    #[test]
    fn a_name_that_is_not_captured_is_not_there_either() {
        assert_eq!(origin(&[("PATH", "/usr/bin")]).get("PATH"), None);
        assert!(origin(&[("PATH", "/usr/bin")]).is_empty());
    }

    #[test]
    fn an_origin_survives_the_round_trip() {
        let origin = origin(&[
            ("ZELLIJ", "0"),
            ("ZELLIJ_SESSION_NAME", "wrangler-proto"),
            ("ZELLIJ_PANE_ID", "7"),
        ]);
        assert_eq!(Origin::decode(&origin.encode()), origin);
    }

    #[test]
    fn a_tmux_socket_field_survives_the_characters_it_carries() {
        // `$TMUX` is a socket path, a pid and a session id, comma separated.
        let origin = origin(&[
            ("TMUX", "/tmp/tmux-1000/default,3242,0"),
            ("TMUX_PANE", "%12"),
        ]);
        let read = Origin::decode(&origin.encode());
        assert_eq!(read.get("TMUX"), Some("/tmp/tmux-1000/default,3242,0"));
        assert_eq!(read.get("TMUX_PANE"), Some("%12"));
    }

    #[test]
    fn nothing_captured_encodes_and_reads_back_as_nothing() {
        let empty = Origin::default();
        assert!(empty.is_empty());
        assert!(Origin::decode(&empty.encode()).is_empty());
        assert!(Origin::decode("").is_empty());
    }

    #[test]
    fn a_value_cannot_split_the_run_it_sits_in() {
        let origin = origin(&[("ZELLIJ_SESSION_NAME", "one\u{1f}two\tthree\nfour")]);
        assert_eq!(
            origin.get("ZELLIJ_SESSION_NAME"),
            Some("one two three four")
        );
        assert_eq!(Origin::decode(&origin.encode()), origin);
    }

    #[test]
    fn a_run_from_a_shorter_table_reads_for_the_names_both_ends_know() {
        // A record written before a variable was appended to the table.
        let short = "0\u{1f}wrangler-proto\u{1f}7";
        let read = Origin::decode(short);
        assert_eq!(read.get("ZELLIJ_PANE_ID"), Some("7"));
        assert_eq!(read.get("TMUX"), None);
        assert_eq!(read.encode(), Origin::decode(&read.encode()).encode());
    }

    #[test]
    fn every_captured_variable_is_listed_by_name() {
        let origin = origin(&[("ZELLIJ", "0"), ("ZELLIJ_PANE_ID", "7")]);
        assert_eq!(
            origin.named(),
            BTreeMap::from([("ZELLIJ", "0"), ("ZELLIJ_PANE_ID", "7")])
        );
    }
}
