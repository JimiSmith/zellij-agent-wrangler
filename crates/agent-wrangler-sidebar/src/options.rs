use std::collections::BTreeMap;

use agent_wrangler_core::command::split_command_line;
use agent_wrangler_core::notify::Notifier;
use agent_wrangler_ui::options::{DrawingOptions, Label};

const NOTIFY: &str = "notify-send";
const CLIENT: &str = "agent-wrangler";

fn truth(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Some(true),
        "false" | "off" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn notifier(value: &str) -> Option<Notifier> {
    match truth(value) {
        Some(false) => None,
        Some(true) => Notifier::new(vec![NOTIFY.to_string()]),
        None => Notifier::new(split_command_line(value)),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub view: DrawingOptions,
    pub desktop: Option<Notifier>,
    pub install_hooks: Option<String>,
}

impl Options {
    pub fn read(configuration: &BTreeMap<String, String>) -> Self {
        let default = Options::default();
        let flag = |key: &str, default: bool| {
            configuration
                .get(key)
                .and_then(|value| truth(value))
                .unwrap_or(default)
        };
        Options {
            view: DrawingOptions {
                label: configuration
                    .get("label")
                    .and_then(|value| Label::read(value))
                    .unwrap_or(default.view.label),
                sections: flag("sections", default.view.sections),
                turn_state: flag("turn_state", default.view.turn_state),
                notifications: flag("notifications", default.view.notifications),
            },
            desktop: configuration
                .get("desktop_notification")
                .and_then(|value| notifier(value)),
            install_hooks: configuration.get("install_hooks").and_then(|value| {
                match truth(value) {
                    Some(false) => None,
                    Some(true) => Some(CLIENT.to_string()),
                    None => Some(value.trim().to_string()).filter(|path| !path.is_empty()),
                }
            }),
        }
    }

    pub fn client(&self) -> &str {
        self.install_hooks.as_deref().unwrap_or(CLIENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(values: &[(&str, &str)]) -> Options {
        Options::read(
            &values
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        )
    }

    #[test]
    fn defaults_and_boolean_spellings_are_preserved() {
        assert_eq!(read(&[]), Options::default());
        assert!(read(&[("sections", "yes")]).view.sections);
        assert!(!read(&[("turn_state", "off")]).view.turn_state);
    }

    #[test]
    fn install_hooks_also_names_the_client() {
        let options = read(&[("install_hooks", "/opt/agent-wrangler")]);
        assert_eq!(options.client(), "/opt/agent-wrangler");
    }

    #[test]
    fn notification_commands_keep_quoted_words_whole() {
        let options = read(&[("desktop_notification", "'/opt/my notifier' --urgency low")]);
        assert_eq!(
            options.desktop.unwrap().program_and_arguments(),
            ["/opt/my notifier", "--urgency", "low"]
        );
    }
}
