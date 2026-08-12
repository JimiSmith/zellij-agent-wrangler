//! How the sidebar reads an agent record: which of the session's records are
//! this sidebar's to draw at all, and which pane the row belongs under.
//!
//! A record says where it was raised as the variables of that process read, and
//! nothing about what those variables mean. This is where they are read for
//! meaning, because only something drawing a zellij session knows that
//! `ZELLIJ_PANE_ID` is a pane it can go to and that `ZELLIJ_SESSION_NAME` is the
//! session it is drawing.

use agent_wrangler_core::agent::{Agent, Record};
use agent_wrangler_core::registry::Registry;

use agent_wrangler_ui::tree::Tab;

/// What a session is called is composed from the facts a record carries rather
/// than decided here, and is re-exported so that a row and an announcement of
/// the same session are spelled the same way.
pub use agent_wrangler_core::label::label;

/// The variable naming the zellij session an agent was raised in.
const SESSION_VAR: &str = "ZELLIJ_SESSION_NAME";

/// The variable naming the pane an agent was raised in.
const PANE_VAR: &str = "ZELLIJ_PANE_ID";

/// Which zellij pane an agent reported itself from, or `None` for one that
/// reported itself from somewhere this sidebar cannot point at.
///
/// The value is carried verbatim from the environment of whatever raised the
/// record, so a pane id is a pane id only once it reads as one: an agent in
/// another multiplexer, or in none, says nothing here that could be mistaken for
/// a pane of this session.
pub fn pane(agent: &Agent) -> Option<u32> {
    agent.origin.get(PANE_VAR)?.parse().ok()
}

/// Only the records raised in the zellij session named, as a run the registry
/// can adopt.
///
/// What arrives carries every agent known about rather than only this session's,
/// so a sidebar that drew all of it would list panes of other sessions as its
/// own. A record that does not say which zellij session it was raised in was
/// raised outside this one by definition, and is dropped with the rest.
///
/// Lines are passed through as they came rather than re-encoded, so a record
/// this build cannot read is neither kept nor quietly rewritten.
pub fn ours(text: &str, session: &str) -> String {
    text.split('\n')
        .filter(|line| match Agent::decode(line) {
            Record::Known(agent) => agent.origin.get(SESSION_VAR) == Some(session),
            _ => false,
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Hand each pane the agents that reported themselves from it, in session
/// order.
///
/// A pane holding agents is drawn as them rather than as itself, so this is what
/// decides whether a pane appears in the tree under its own title.
pub fn place(tabs: &mut [Tab], registry: &Registry) {
    for tab in tabs.iter_mut() {
        for pane in tab.panes.iter_mut() {
            pane.agents = registry
                .iter()
                .filter(|agent| self::pane(agent) == Some(pane.id))
                .cloned()
                .collect();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use agent_wrangler_core::agent::{Meta, SessionId};
    use agent_wrangler_core::origin::Origin;

    use agent_wrangler_ui::tree::Pane;

    /// The zellij session every record in these tests was raised in, so that a
    /// record from anywhere else is plainly from anywhere else.
    pub(crate) const HERE: &str = "wrangler-proto";

    pub(crate) fn session(text: &str) -> SessionId {
        SessionId::new(text).unwrap()
    }

    fn meta(dir: &str, name: &str, title: &str) -> Meta {
        Meta {
            dir: dir.to_string(),
            name: name.to_string(),
            color: String::new(),
            title: title.to_string(),
        }
    }

    /// What a hook run in a pane of this zellij session captures.
    pub(crate) fn at_pane(pane: u32) -> Origin {
        raised_in(HERE, &pane.to_string())
    }

    /// The same, for a session and a pane named outright, so that a record from
    /// another session or from a pane that is no pane at all can be built.
    pub(crate) fn raised_in(session: &str, pane: &str) -> Origin {
        Origin::from(|name| match name {
            "ZELLIJ" => Some("0".to_string()),
            SESSION_VAR => Some(session.to_string()),
            PANE_VAR => Some(pane.to_string()),
            _ => None,
        })
    }

    pub(crate) fn agent(id: &str, origin: Origin) -> Agent {
        Agent::new(session(id), "claude", meta("wrangler", "", ""), origin)
    }

    fn tabs() -> Vec<Tab> {
        vec![Tab {
            position: 0,
            name: "one".to_string(),
            active: true,
            panes: vec![Pane::new(1, "bash", false), Pane::new(2, "nvim", true)],
        }]
    }

    #[test]
    fn an_agent_lands_on_the_pane_it_reported_itself_from() {
        let mut registry = Registry::default();
        registry.start(agent("one", at_pane(2)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert!(tabs[0].panes[0].agents.is_empty());
        assert_eq!(tabs[0].panes[1].agents.len(), 1);
    }

    #[test]
    fn an_agent_whose_pane_is_gone_lands_nowhere() {
        let mut registry = Registry::default();
        registry.start(agent("one", at_pane(99)));
        registry.start(agent("two", Origin::default()));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert!(tabs
            .iter()
            .all(|tab| tab.panes.iter().all(|pane| pane.agents.is_empty())));
    }

    #[test]
    fn an_agent_in_no_pane_this_sidebar_could_go_to_is_in_no_pane() {
        // A tmux pane is named the way tmux names one, and an agent in no
        // multiplexer at all names nothing. Neither is a zellij pane id.
        assert_eq!(pane(&agent("one", at_pane(7))), Some(7));
        assert_eq!(pane(&agent("two", raised_in(HERE, "%12"))), None);
        assert_eq!(pane(&agent("three", Origin::default())), None);
    }

    #[test]
    fn two_agents_in_one_pane_both_land_on_it() {
        let mut registry = Registry::default();
        registry.start(agent("one", at_pane(1)));
        registry.start(agent("two", at_pane(1)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert_eq!(tabs[0].panes[0].agents.len(), 2);
    }

    #[test]
    fn placing_again_replaces_what_was_placed_before() {
        let mut registry = Registry::default();
        registry.start(agent("one", at_pane(1)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        registry.end(&session("one"));
        place(&mut tabs, &registry);
        assert!(tabs[0].panes[0].agents.is_empty());
    }

    /// The run of records a sidebar is handed, holding every session named.
    fn run(records: &[Agent]) -> String {
        let mut registry = Registry::default();
        for record in records {
            registry.start(record.clone());
        }
        registry.encode()
    }

    #[test]
    fn only_this_zellij_sessions_agents_are_kept() {
        let mine = agent("mine", at_pane(1));
        let theirs = agent("theirs", raised_in("another-session", "1"));
        let mut registry = Registry::default();
        registry.adopt(&ours(&run(&[mine.clone(), theirs]), HERE));
        assert_eq!(
            registry
                .iter()
                .map(|agent| agent.session.clone())
                .collect::<Vec<_>>(),
            vec![mine.session]
        );
    }

    #[test]
    fn an_agent_that_says_no_zellij_session_is_no_agent_of_this_one() {
        // An agent in another multiplexer, or in none, is somewhere this
        // sidebar cannot draw. Keeping it would put a row on a pane of ours.
        let elsewhere = agent("elsewhere", Origin::default());
        assert_eq!(ours(&run(&[elsewhere]), HERE), "");
        // A sidebar that has not been told its own session name yet is not a
        // sidebar every unplaced record belongs to.
        assert_eq!(ours(&run(&[agent("mine", at_pane(1))]), ""), "");
    }

    #[test]
    fn what_is_kept_is_the_record_as_it_arrived() {
        let mine = agent("mine", at_pane(1));
        let kept = ours(
            &run(&[agent("theirs", raised_in("elsewhere", "1")), mine.clone()]),
            HERE,
        );
        assert_eq!(kept, mine.encode());
        // A line that is no record at all is not a line to pass on.
        assert_eq!(ours("nonsense\n", HERE), "");
    }
}
