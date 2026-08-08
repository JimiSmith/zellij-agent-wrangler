//! What the layout asks the sidebar for.
//!
//! Zellij hands a plugin its configuration as strings, so every value is read
//! back into a type here and nothing downstream sees a string again. A value
//! that is not one of the words an option accepts leaves that option at its
//! default: an option written wrong should cost the user the setting, not the
//! sidebar.

use std::collections::BTreeMap;

use agent_wrangler_core::command::words;

/// What an agent's row is called.
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
    fn read(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "name" => Some(Label::Name),
            "dir" => Some(Label::Dir),
            _ => None,
        }
    }
}

/// The words a desktop notification is raised by. The title and the body are
/// appended to them, which is the shape `notify-send` and its like take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notifier(Vec<String>);

/// What a desktop notification is raised by when the option only says to raise
/// one.
const NOTIFY: &str = "notify-send";

impl Notifier {
    /// The notifier an option value asks for: nothing for the words meaning
    /// off, the default for those meaning on, and anything else read as the
    /// command line to run.
    ///
    /// A command is quoted the way a shell would quote it, so a notifier living
    /// at a path with a space in it can be named.
    fn read(value: &str) -> Option<Self> {
        match truth(value) {
            Some(false) => None,
            Some(true) => Some(Notifier(vec![NOTIFY.to_string()])),
            None => match words(value).as_slice() {
                [] => None,
                command => Some(Notifier(command.to_vec())),
            },
        }
    }

    /// The whole command line for one notification: what to run, then the title
    /// and the body.
    pub fn command(&self, title: &str, body: &str) -> Vec<String> {
        let mut command = self.0.clone();
        command.push(title.to_string());
        command.push(body.to_string());
        command
    }
}

/// Whether a value is one of the words for yes or for no, or neither.
fn truth(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Some(true),
        "false" | "off" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// Everything the layout can say about how the sidebar behaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub label: Label,
    /// Whether the tree is followed by a block per agent, the same sessions
    /// gathered by which agent they are rather than by where they are.
    pub sections: bool,
    /// Whether an agent's row says whose turn it is.
    pub turn_state: bool,
    /// Whether the calls for the user are listed at the foot of the pane.
    pub notifications: bool,
    /// What to raise a desktop notification with, if anything.
    pub desktop: Option<Notifier>,
    /// The hook client to install the agent hooks with on load, if any. Named
    /// rather than found, because a plugin has no way to look for it.
    pub install_hooks: Option<String>,
}

/// The hook client the install option names when it only says to install.
const CLIENT: &str = "agent-wrangler";

impl Default for Options {
    fn default() -> Self {
        Options {
            label: Label::default(),
            sections: false,
            turn_state: true,
            notifications: true,
            desktop: None,
            install_hooks: None,
        }
    }
}

impl Options {
    /// Read the configuration zellij passes a plugin, leaving every option it
    /// does not mention at its default.
    pub fn read(configuration: &BTreeMap<String, String>) -> Self {
        let default = Options::default();
        let flag = |key: &str, default: bool| {
            configuration
                .get(key)
                .and_then(|value| truth(value))
                .unwrap_or(default)
        };
        Options {
            label: configuration
                .get("label")
                .and_then(|value| Label::read(value))
                .unwrap_or(default.label),
            sections: flag("sections", default.sections),
            turn_state: flag("turn_state", default.turn_state),
            notifications: flag("notifications", default.notifications),
            desktop: configuration
                .get("desktop_notification")
                .and_then(|value| Notifier::read(value)),
            install_hooks: configuration.get("install_hooks").and_then(|value| {
                match truth(value) {
                    Some(false) => None,
                    Some(true) => Some(CLIENT.to_string()),
                    None => Some(value.trim().to_string()).filter(|path| !path.is_empty()),
                }
            }),
        }
    }

    /// The client the sidebar reaches everything outside its own pane through.
    ///
    /// The sidebar always needs one, whatever the layout says: what it draws
    /// arrives only once it has asked to be sent it, and only the client can
    /// ask. The install option is where a layout names a client living
    /// somewhere the path would not find, so a layout that has named one there
    /// has named it for this too.
    pub fn client(&self) -> &str {
        self.install_hooks.as_deref().unwrap_or(CLIENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(pairs: &[(&str, &str)]) -> Options {
        Options::read(
            &pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        )
    }

    #[test]
    fn an_empty_configuration_is_every_default() {
        assert_eq!(read(&[]), Options::default());
    }

    #[test]
    fn a_flag_answers_to_every_word_for_yes_and_for_no() {
        for yes in ["true", "on", "yes", "1", " ON "] {
            assert!(read(&[("sections", yes)]).sections, "{yes}");
        }
        for no in ["false", "off", "no", "0"] {
            assert!(!read(&[("turn_state", no)]).turn_state, "{no}");
        }
    }

    #[test]
    fn a_value_an_option_does_not_know_leaves_it_at_its_default() {
        // Losing the setting is the cost of a typo; losing the sidebar is not.
        assert_eq!(read(&[("sections", "sometimes")]), Options::default());
        assert_eq!(read(&[("label", "surname")]), Options::default());
    }

    #[test]
    fn the_label_mode_is_named() {
        assert_eq!(read(&[("label", "dir")]).label, Label::Dir);
        assert_eq!(read(&[("label", "NAME")]).label, Label::Name);
    }

    #[test]
    fn a_desktop_notification_is_off_until_it_is_asked_for() {
        assert_eq!(read(&[]).desktop, None);
        assert_eq!(read(&[("desktop_notification", "off")]).desktop, None);
        assert_eq!(
            read(&[("desktop_notification", "on")])
                .desktop
                .unwrap()
                .command("claude", "vim · api"),
            ["notify-send", "claude", "vim · api"]
        );
    }

    #[test]
    fn a_notifier_of_its_own_takes_the_title_and_the_body_after_its_arguments() {
        let options = read(&[("desktop_notification", "'/opt/my notifier' --urgency low")]);
        assert_eq!(
            options.desktop.unwrap().command("claude", "vim · api"),
            [
                "/opt/my notifier",
                "--urgency",
                "low",
                "claude",
                "vim · api"
            ]
        );
    }

    #[test]
    fn installing_the_hooks_names_the_client_when_the_option_does_not() {
        assert_eq!(read(&[]).install_hooks, None);
        assert_eq!(read(&[("install_hooks", "off")]).install_hooks, None);
        assert_eq!(
            read(&[("install_hooks", "on")]).install_hooks.as_deref(),
            Some("agent-wrangler")
        );
        assert_eq!(
            read(&[("install_hooks", "/opt/bin/agent-wrangler")])
                .install_hooks
                .as_deref(),
            Some("/opt/bin/agent-wrangler")
        );
    }

    #[test]
    fn the_client_is_the_one_the_install_option_names_or_the_default() {
        // A sidebar told not to install the hooks still has a client to reach,
        // since that is what it is sent anything through at all.
        assert_eq!(read(&[]).client(), "agent-wrangler");
        assert_eq!(read(&[("install_hooks", "off")]).client(), "agent-wrangler");
        assert_eq!(
            read(&[("install_hooks", "/opt/bin/agent-wrangler")]).client(),
            "/opt/bin/agent-wrangler"
        );
    }
}
