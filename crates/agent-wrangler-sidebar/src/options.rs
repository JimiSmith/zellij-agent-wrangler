use std::collections::BTreeMap;

use agent_wrangler_core::command::split_command_line;
use agent_wrangler_core::notify::Notifier;
use agent_wrangler_ui::options::{DrawingOptions, Label, StatusTemplate};

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
    pub fn from_configuration(configuration: &BTreeMap<String, String>) -> Self {
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
                dashboard: flag("dashboard", default.view.dashboard),
                turn_state: flag("turn_state", default.view.turn_state),
                notifications: flag("notifications", default.view.notifications),
                status_line: configuration
                    .get("status_line")
                    .and_then(|value| StatusTemplate::new(value)),
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

    /// The program that the sidebar runs to reach the daemon. This is what the
    /// layout named, or the name that the installed client carries.
    pub fn helper_program_path(&self) -> &str {
        self.install_hooks.as_deref().unwrap_or(CLIENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(values: &[(&str, &str)]) -> Options {
        Options::from_configuration(
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
        assert!(read(&[("dashboard", "1")]).view.dashboard);
    }

    #[test]
    fn the_dashboard_is_off_until_the_layout_asks_for_it() {
        assert!(!read(&[]).view.dashboard);
        // A value the option does not recognize costs the setting, not the
        // sidebar.
        assert!(!read(&[("dashboard", "sometimes")]).view.dashboard);
    }

    #[test]
    fn a_status_line_is_read_from_the_template_the_user_wrote() {
        let options = read(&[("status_line", "{branch} · {model}")]);
        assert_eq!(
            options.view.status_line,
            StatusTemplate::new("{branch} · {model}")
        );
    }

    #[test]
    fn no_template_and_an_empty_one_both_ask_for_no_status_line() {
        assert_eq!(read(&[]).view.status_line, None);
        assert_eq!(read(&[("status_line", "")]).view.status_line, None);
        assert_eq!(read(&[("status_line", "   ")]).view.status_line, None);
    }

    #[test]
    fn install_hooks_also_names_the_client() {
        let options = read(&[("install_hooks", "/opt/agent-wrangler")]);
        assert_eq!(options.helper_program_path(), "/opt/agent-wrangler");
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
