use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use agent_wrangler_core::agent::AGENTS_MESSAGE;
use agent_wrangler_sidebar::{Application, Effect, Input, Options, PaneId, Permission, UserAction};
use agent_wrangler_ui::{ansi, Rect};
use zellij_agent_wrangler::adapter;
use zellij_tile::prelude::*;

/// Run the client from a directory that cannot disappear.
///
/// Zellij otherwise uses the directory the sidebar pane was opened in. A
/// removed worktree can take that directory away while the session remains,
/// causing every later spawn to fail before reaching even an absolute client
/// path. None of these calls reads a working directory, so the root is safe.
const RUN_FROM: &str = "/";
const CALL: &str = "call";

#[derive(Default)]
struct Plugin {
    application: Application,
    plugin_id: u32,
}

register_plugin!(Plugin);

fn cells(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

impl Plugin {
    fn reduce(&mut self, input: Input, settle: bool) -> bool {
        let mut repaint = false;
        let mut effects = VecDeque::new();
        effects.extend(self.application.reduce(input).effects);
        self.drain(&mut repaint, &mut effects);
        if settle {
            effects.extend(self.application.reduce(Input::EventSettled).effects);
            self.drain(&mut repaint, &mut effects);
        }
        repaint
    }

    fn drain(&mut self, repaint: &mut bool, effects: &mut VecDeque<Effect>) {
        while let Some(effect) = effects.pop_front() {
            if effect == Effect::Repaint {
                *repaint = true;
                continue;
            }
            if let Some(observation) = self.execute(effect) {
                effects.extend(self.application.reduce(observation).effects);
            }
        }
    }

    fn execute(&self, effect: Effect) -> Option<Input> {
        match effect {
            Effect::Repaint => None,
            Effect::RefreshFocus => {
                // Zellij calls the tuple's first value a tab index, but the
                // server actually returns the stable Tab.id stored in
                // active_tab_ids, not the tab's visual position.
                Some(Input::FocusObserved(get_focused_pane_info().ok().map(
                    |(tab_id, pane)| adapter::focus(tab_id, pane, self.plugin_id),
                )))
            }
            Effect::RefreshPaneTitle(pane) => {
                let title = adapter::numeric_pane(&pane)
                    .and_then(|id| get_pane_info(zellij_tile::prelude::PaneId::Terminal(id)))
                    .map(|pane| pane.title);
                Some(Input::PaneTitleObserved { pane, title })
            }
            Effect::Run(command) => {
                let mut words = vec![command.program];
                words.extend(command.args);
                let words: Vec<&str> = words.iter().map(String::as_str).collect();
                run_command_with_env_variables_and_cwd(
                    &words,
                    BTreeMap::new(),
                    PathBuf::from(RUN_FROM),
                    BTreeMap::from([(CALL.to_string(), command.call)]),
                );
                None
            }
            Effect::Broadcast(message) => {
                let (name, payload) = adapter::encode_message(message);
                let mut message = MessageToPlugin::new(name);
                if let Some(payload) = payload {
                    message = message.with_payload(payload);
                }
                pipe_message_to_plugin(message);
                None
            }
            Effect::FocusPane(pane) => {
                if let Some(id) = adapter::numeric_pane(&pane) {
                    focus_pane_with_id(zellij_tile::prelude::PaneId::Terminal(id), false, false);
                }
                None
            }
            Effect::SwitchTab(tab) => {
                if let Some(position) = self.application.tab_position(&tab) {
                    switch_tab_to(position.one_based() as u32);
                }
                None
            }
            Effect::StopSessionDiscovery => {
                unsubscribe(&[EventType::SessionUpdate]);
                None
            }
            Effect::CloseSidebar => {
                close_self();
                None
            }
        }
    }

    fn update_input(&self, event: Event) -> Option<Input> {
        match event {
            Event::Visible(visible) => Some(Input::VisibilityChanged(visible)),
            Event::TabUpdate(tabs) => Some(Input::TabsReported(adapter::tabs(tabs))),
            Event::PaneUpdate(panes) => Some(Input::LayoutReported(adapter::layout(
                panes,
                self.plugin_id,
            ))),
            Event::CommandChanged(zellij_tile::prelude::PaneId::Terminal(id), ..)
            | Event::CwdChanged(zellij_tile::prelude::PaneId::Terminal(id), ..) => {
                Some(Input::PaneChanged(PaneId::new(id.to_string())))
            }
            Event::SessionUpdate(sessions, _) => sessions
                .into_iter()
                .find(|session| session.is_current_session)
                .map(|session| Input::SessionNamed(session.name)),
            Event::RunCommandResult(exit, _, stderr, context) => Some(Input::CommandFinished {
                exit,
                stderr,
                call: context.get(CALL).cloned().unwrap_or_default(),
            }),
            Event::PermissionRequestResult(status) => {
                Some(Input::PermissionReported(match status {
                    PermissionStatus::Granted => Permission::Granted,
                    PermissionStatus::Denied => Permission::Denied,
                }))
            }
            Event::Key(key) => match key.bare_key {
                BareKey::Down | BareKey::Char('j') => Some(Input::User(UserAction::Next)),
                BareKey::Up | BareKey::Char('k') => Some(Input::User(UserAction::Previous)),
                BareKey::Enter => Some(Input::User(UserAction::Activate)),
                BareKey::Char('q') => Some(Input::User(UserAction::Quit)),
                _ => None,
            },
            Event::Mouse(Mouse::LeftClick(line, _)) => usize::try_from(line)
                .ok()
                .map(|line| Input::User(UserAction::Click(line))),
            _ => None,
        }
    }
}

impl ZellijPlugin for Plugin {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.application = Application::new(Options::read(&configuration), "zellij");
        self.plugin_id = get_plugin_ids().plugin_id;
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::MessageAndLaunchOtherPlugins,
            PermissionType::RunCommands,
        ]);
        subscribe(&[
            EventType::Key,
            EventType::Mouse,
            EventType::Visible,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::CommandChanged,
            EventType::CwdChanged,
            EventType::PermissionRequestResult,
            EventType::SessionUpdate,
            EventType::RunCommandResult,
        ]);
        // `Visible` reports later changes, but Zellij does not necessarily
        // emit an initial `Visible(true)` for a newly opened plugin pane. Seed
        // that fact now, but do not execute its focus-refresh effect from
        // `load`: Zellij host state queries are only safe after loading. The
        // initial tab or pane report will perform the pending refresh.
        self.application.reduce(Input::VisibilityChanged(true));
    }

    fn update(&mut self, event: Event) -> bool {
        match self.update_input(event) {
            Some(input) => self.reduce(input, true),
            None => self.reduce(Input::EventSettled, false),
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.source == PipeSource::Plugin(self.plugin_id) {
            return false;
        }
        let input = if message.name == AGENTS_MESSAGE {
            adapter::agents(
                message.payload.as_deref().unwrap_or_default(),
                // Agent reports cannot belong here until the session is named;
                // an empty name matches no Zellij session.
                self.application.session_name().unwrap_or_default(),
            )
            .map(Input::Agents)
        } else {
            adapter::decode_message(&message.name, message.payload.as_deref()).map(Input::Message)
        };
        input
            .map(|input| self.reduce(input, false))
            .unwrap_or(false)
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let area = Rect::new(0, 0, cells(cols), cells(rows));
        let rendered = self.application.render(area);
        print!(
            "{}",
            ansi::pane(&rendered.frame, rendered.selection.as_ref())
        );
    }
}
