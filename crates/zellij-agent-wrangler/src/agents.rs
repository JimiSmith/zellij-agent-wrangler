//! How the sidebar reads an agent record: what a row calls the session, the
//! color it says it in, and which pane the row belongs under.
//!
//! A record carries what a session is known by rather than the text of its row,
//! so the spelling is chosen here. That is what lets the label option change
//! every row at once with no agent reporting itself again.

use agent_wrangler_core::agent::Agent;
use agent_wrangler_core::registry::Registry;

use crate::model::NamedColor;
use crate::options::Label;
use crate::tree::Tab;

/// What a sidebar with no records of its own asks, and what any sidebar that
/// has some answers with.
pub const SYNC_REQUEST_MESSAGE: &str = "wrangler:agents?";
pub const SYNC_MESSAGE: &str = "wrangler:agents";

/// What this session's row says, spelled the way the sidebar was asked to spell
/// it.
///
/// A teammate leads with its own name, so it is never mistaken for a session of
/// its own. An untitled session falls back to where it is working, and one that
/// cannot say even that falls back to what it is.
pub fn label(agent: &Agent, mode: Label) -> String {
    let dir = match agent.meta.dir.is_empty() {
        true => agent.agent.as_str(),
        false => agent.meta.dir.as_str(),
    };
    let title = agent.meta.title.as_str();
    match (mode, agent.meta.name.as_str()) {
        (Label::Name, "") if !title.is_empty() => title.to_string(),
        (_, "") => dir.to_string(),
        (Label::Name, name) if title.is_empty() => format!("@{name}"),
        (Label::Name, name) => format!("@{name} - {title}"),
        (Label::Dir, name) => format!("@{name} - {dir}"),
    }
}

/// The color this session's icon is drawn in, or `None` for one the agent gives
/// no color to.
pub fn color(agent: &Agent) -> Option<NamedColor> {
    NamedColor::agent(&agent.meta.color)
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
                .filter(|agent| agent.pane == Some(pane.id))
                .cloned()
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::agent::{Meta, SessionId};

    use crate::tree::Pane;

    fn session(text: &str) -> SessionId {
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

    fn agent(id: &str, pane: Option<u32>) -> Agent {
        Agent::new(session(id), "claude", meta("wrangler", "", ""), pane)
    }

    fn colored(color: &str) -> Agent {
        Agent::new(
            session("one"),
            "claude",
            Meta {
                color: color.to_string(),
                ..meta("wrangler", "", "")
            },
            Some(1),
        )
    }

    fn labelled(dir: &str, name: &str, title: &str) -> Agent {
        Agent::new(session("one"), "claude", meta(dir, name, title), Some(1))
    }

    #[test]
    fn a_session_is_drawn_in_the_color_the_agent_gives_it() {
        // The two an agent names that a terminal does not are drawn in the
        // bright form of their neighbour, so all eight stay apart.
        for (name, want) in [
            ("red", NamedColor::Red),
            ("green", NamedColor::Green),
            ("yellow", NamedColor::Yellow),
            ("blue", NamedColor::Blue),
            ("purple", NamedColor::Magenta),
            ("cyan", NamedColor::Cyan),
            ("orange", NamedColor::BrightYellow),
            ("pink", NamedColor::BrightMagenta),
        ] {
            assert_eq!(color(&colored(name)), Some(want), "{name}");
        }
    }

    #[test]
    fn a_session_with_no_color_of_its_own_is_drawn_in_none() {
        assert_eq!(color(&colored("")), None);
        // A name this sidebar does not know is not a color to guess at.
        assert_eq!(color(&colored("chartreuse")), None);
    }

    #[test]
    fn a_titled_session_is_called_by_its_title_and_an_untitled_one_by_its_directory() {
        let titled = labelled("wrangler", "", "the zellij port");
        assert_eq!(label(&titled, Label::Name), "the zellij port");
        assert_eq!(label(&titled, Label::Dir), "wrangler");
        let untitled = labelled("wrangler", "", "");
        assert_eq!(label(&untitled, Label::Name), "wrangler");
        assert_eq!(label(&untitled, Label::Dir), "wrangler");
    }

    #[test]
    fn a_session_that_can_say_neither_is_called_by_what_it_is() {
        let anonymous = labelled("", "", "");
        assert_eq!(label(&anonymous, Label::Name), "claude");
        assert_eq!(label(&anonymous, Label::Dir), "claude");
    }

    #[test]
    fn a_teammate_leads_with_its_own_name_whatever_it_is_called_by() {
        let teammate = labelled("wrangler", "scout", "reading the source");
        assert_eq!(label(&teammate, Label::Name), "@scout - reading the source");
        assert_eq!(label(&teammate, Label::Dir), "@scout - wrangler");
        // A teammate with nothing to say for itself is still told apart from a
        // session of its own.
        assert_eq!(
            label(&labelled("wrangler", "scout", ""), Label::Name),
            "@scout"
        );
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
        registry.start(agent("one", Some(2)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert!(tabs[0].panes[0].agents.is_empty());
        assert_eq!(tabs[0].panes[1].agents.len(), 1);
    }

    #[test]
    fn an_agent_whose_pane_is_gone_lands_nowhere() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(99)));
        registry.start(agent("two", None));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert!(tabs
            .iter()
            .all(|tab| tab.panes.iter().all(|pane| pane.agents.is_empty())));
    }

    #[test]
    fn two_agents_in_one_pane_both_land_on_it() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(1)));
        registry.start(agent("two", Some(1)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert_eq!(tabs[0].panes[0].agents.len(), 2);
    }

    #[test]
    fn placing_again_replaces_what_was_placed_before() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(1)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        registry.end(&session("one"));
        place(&mut tabs, &registry);
        assert!(tabs[0].panes[0].agents.is_empty());
    }
}
