//! What a session is called, spelled out of the facts a record carries.
//!
//! A record holds what a session is known by rather than the text naming it, so
//! the spelling happens here and not where the facts are gathered. That is what
//! lets the same session be named the same way by everything that mentions it,
//! whether it is drawing a row or announcing a call.

use crate::agent::Agent;

/// What a session is called by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Label {
    /// The title the session gave itself, falling back to its directory until
    /// it has one.
    #[default]
    Name,
    /// The working directory, whatever the session calls itself.
    Dir,
}

impl Label {
    /// The mode a written value asks for, or `None` for a word that names none.
    pub fn read(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "name" => Some(Label::Name),
            "dir" => Some(Label::Dir),
            _ => None,
        }
    }
}

/// What this session is called, spelled the way `mode` asks for.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::{at_pane, meta, session};

    fn labelled(dir: &str, name: &str, title: &str) -> Agent {
        Agent::new(session("one"), "claude", meta(dir, name, title), at_pane(1))
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

    #[test]
    fn a_mode_is_named_and_anything_else_names_none() {
        assert_eq!(Label::read("dir"), Some(Label::Dir));
        assert_eq!(Label::read(" NAME "), Some(Label::Name));
        assert_eq!(Label::read("surname"), None);
    }
}
