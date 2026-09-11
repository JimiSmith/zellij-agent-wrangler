//! Command-line options for a manually started sidebar.

use std::collections::BTreeMap;

use agent_wrangler_core::label::Label;
use agent_wrangler_sidebar::Options;

pub const USAGE: &str = "Usage: tmux-agent-wrangler [OPTIONS]

Run one interactive sidebar in the current tmux pane.
Create and place the pane yourself; this program does not change the layout.

Options accept --name VALUE or --name=VALUE. Boolean values are required.
  --label VALUE                 name (default) or dir
  --sections BOOL               Group agents by type (default: false)
  --dashboard BOOL              Show the dashboard (default: false)
  --turn_state BOOL             Show turn markers (default: true)
  --notifications BOOL          Show calls for the user (default: true)
  --status_line TEMPLATE        Status template; empty disables it
  --desktop_notification VALUE Boolean or quoted program and arguments
  --install_hooks VALUE        Boolean or helper program name
  --help, -h                    Print this help without contacting tmux

BOOL accepts true/on/yes/1 or false/off/no/0, without case sensitivity.
Desktop notifications and hook installation are off by default.
True uses notify-send for desktop notifications and agent-wrangler for hooks.
Quote templates and commands that contain spaces. No shell runs commands.";

#[derive(Debug, PartialEq, Eq)]
pub enum Arguments {
    Sidebar(Options),
    Help,
}

/// Reads arguments without the executable name. Valid values use the shared
/// configuration parser so templates, notifier commands, and hooks keep the
/// same meaning in each client.
pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Arguments, String> {
    let mut arguments = arguments.into_iter();
    let mut configuration = BTreeMap::new();
    while let Some(argument) = arguments.next() {
        if argument == "--help" || argument == "-h" {
            return Ok(Arguments::Help);
        }
        let option = argument
            .strip_prefix("--")
            .ok_or_else(|| format!("unknown argument: {argument}"))?;
        let (name, value) = match option.split_once('=') {
            Some((name, value)) => (name, value.to_string()),
            None => (
                option,
                arguments
                    .next()
                    .filter(|value| !value.starts_with("--") && value != "-h")
                    .ok_or_else(|| format!("missing value for --{option}"))?,
            ),
        };
        if !matches!(
            name,
            "label"
                | "sections"
                | "dashboard"
                | "turn_state"
                | "notifications"
                | "status_line"
                | "desktop_notification"
                | "install_hooks"
        ) {
            return Err(format!("unknown option: --{name}"));
        }
        if configuration.insert(name.to_string(), value).is_some() {
            return Err(format!("duplicate option: --{name}"));
        }
    }
    for (name, value) in &configuration {
        if name == "label" && Label::read(value).is_none() {
            return Err(format!(
                "invalid value for --label: {value:?}; expected name or dir"
            ));
        }
        if matches!(
            name.as_str(),
            "sections" | "dashboard" | "turn_state" | "notifications"
        ) && !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "on" | "yes" | "1" | "false" | "off" | "no" | "0"
        ) {
            return Err(format!(
                "invalid value for --{name}: {value:?}; expected true/on/yes/1 or false/off/no/0"
            ));
        }
    }
    Ok(Arguments::Sidebar(Options::from_configuration(
        &configuration,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_has_long_and_short_spellings() {
        for option in ["--help", "-h"] {
            assert_eq!(read(&[option]), Ok(Arguments::Help));
        }
    }

    #[test]
    fn malformed_labels_are_rejected() {
        for value in ["", "directory", "true", "names"] {
            assert!(read(&["--label", value]).unwrap_err().contains("label"));
        }
    }

    #[test]
    fn malformed_boolean_values_are_rejected() {
        for name in ["sections", "dashboard", "turn_state", "notifications"] {
            for value in ["", "sometimes", "2", "truth"] {
                assert!(read(&[&format!("--{name}={value}")])
                    .unwrap_err()
                    .contains(name));
            }
        }
    }

    #[test]
    fn every_option_requires_an_explicit_value() {
        for name in [
            "label",
            "sections",
            "dashboard",
            "turn_state",
            "notifications",
            "status_line",
            "desktop_notification",
            "install_hooks",
        ] {
            let option = format!("--{name}");
            assert!(read(&[&option]).unwrap_err().contains("missing value"));
            assert!(read(&[&option, "--dashboard=true"])
                .unwrap_err()
                .contains("missing value"));
        }
    }

    #[test]
    fn duplicate_options_are_rejected_even_across_value_forms() {
        for name in [
            "label",
            "sections",
            "dashboard",
            "turn_state",
            "notifications",
            "status_line",
            "desktop_notification",
            "install_hooks",
        ] {
            let option = format!("--{name}");
            let joined = format!("--{name}=true");
            assert!(read(&[&option, "true", &joined])
                .unwrap_err()
                .contains("duplicate"));
        }
    }

    #[test]
    fn unknown_options_are_rejected() {
        for arguments in [
            vec!["--layout=left"],
            vec!["--turn-state=false"],
            vec!["--unknown", "true"],
            vec!["positional"],
            vec!["--help=yes"],
        ] {
            assert!(read(&arguments).is_err(), "{arguments:?}");
        }
    }

    fn read(arguments: &[&str]) -> Result<Arguments, String> {
        parse(arguments.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn no_arguments_preserve_shared_defaults() {
        assert_eq!(read(&[]), Ok(Arguments::Sidebar(Options::default())));
    }

    #[test]
    fn both_value_forms_use_shared_configuration() {
        let values = [
            ("label", " DIR "),
            ("sections", "yes"),
            ("dashboard", "1"),
            ("turn_state", "off"),
            ("notifications", "no"),
            ("status_line", "{branch}={model}"),
            ("desktop_notification", "'my notifier' --urgency low"),
            ("install_hooks", " custom-client "),
        ];
        let expected = Options::from_configuration(
            &values
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        );
        let separate = values
            .iter()
            .flat_map(|(name, value)| [format!("--{name}"), value.to_string()]);
        let joined = values
            .iter()
            .map(|(name, value)| format!("--{name}={value}"));
        assert_eq!(parse(separate), Ok(Arguments::Sidebar(expected.clone())));
        assert_eq!(parse(joined), Ok(Arguments::Sidebar(expected)));
    }
}
