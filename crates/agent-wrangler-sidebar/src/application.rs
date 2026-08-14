use std::collections::BTreeMap;

use agent_wrangler_core::agent::{Agent, SessionId, Turn};
use agent_wrangler_core::label::label;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_ui::frame::{compose, Frame, Note};
use agent_wrangler_ui::model::{NamedColor, Notification, PaneId, Row, RowKey, TabPosition};
use agent_wrangler_ui::{selection, tree, Rect};

use crate::calls::Answered;
use crate::client::Client;
use crate::model::{
    AgentSnapshot, Broadcast, Command, Decision, Effect, Focus, FocusTarget, Input, PaneSnapshot,
    Permission, RenderedView, TabId, TabReport, UserAction,
};
use crate::options::Options;
use crate::session;

const UNREACHABLE: &str = "the sidebar could not run its client";
const REFUSED: &str = "the sidebar cannot read the session";
const MISMATCH: &str = "the hook client is a different version; install both again";

#[derive(Default)]
pub struct Application {
    options: Options,
    multiplexer: String,
    tabs: Vec<TabReport>,
    panes: PaneSnapshot,
    rows: Vec<Row>,
    notices: Vec<Notification>,
    registry: Registry,
    agent_panes: BTreeMap<SessionId, PaneId>,
    answered: Answered,
    client: Client,
    session_name: Option<String>,
    registered: bool,
    selected: Option<RowKey>,
    permission: Option<Permission>,
    painted: Vec<Option<RowKey>>,
    focus: Option<Focus>,
    left_behind: Option<PaneId>,
    had_company: bool,
    installed: bool,
    mismatched: bool,
    focused: bool,
}

impl Application {
    pub fn new(options: Options, multiplexer: impl Into<String>) -> Self {
        let client = Client::new(options.client());
        Application {
            options,
            multiplexer: multiplexer.into(),
            client,
            ..Application::default()
        }
    }

    pub fn reduce(&mut self, input: Input) -> Decision {
        match input {
            Input::TabsReported(tabs) => {
                self.tabs = tabs;
                Decision::effect(Effect::RefreshFocus)
            }
            Input::PanesReported(panes) => {
                self.panes = panes;
                Decision::effect(Effect::RefreshFocus)
            }
            Input::PaneChanged(pane) => Decision::effect(Effect::RefreshPaneTitle(pane)),
            Input::PaneTitleObserved { pane, title } => match title {
                Some(title) => {
                    for tab in &mut self.panes.tabs {
                        for candidate in &mut tab.panes {
                            if candidate.id == pane {
                                candidate.title = title.clone();
                            }
                        }
                    }
                    Decision::effect(Effect::RefreshFocus)
                }
                None => Decision::default(),
            },
            Input::FocusObserved(focus) => self.observe_focus(focus),
            Input::SessionNamed(name) => {
                self.session_name = Some(name);
                let mut decision = Decision::effect(Effect::StopSessionDiscovery);
                self.register(&mut decision);
                decision
            }
            Input::PermissionReported(permission) => {
                self.permission = Some(permission);
                let mut decision = Decision::repaint();
                self.register(&mut decision);
                decision
            }
            Input::CommandFinished { exit, stderr, call } => Decision {
                repaint: self.ran(exit, &stderr, &call),
                effects: Vec::new(),
            },
            Input::User(action) => self.user(action),
            Input::Message(message) => self.message(message),
            Input::Agents(snapshot) => self.adopt(snapshot),
            Input::EventSettled => {
                let mut decision = Decision::default();
                self.install_hooks(&mut decision);
                decision
            }
        }
    }

    pub fn render(&mut self, area: Rect) -> RenderedView {
        let frame = self.frame(area);
        self.painted = frame.keys();
        let selection = self.focused.then(|| self.selection()).flatten();
        RenderedView { frame, selection }
    }

    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
    }

    /// The latest reported position of a stable tab identity.
    ///
    /// Positional host APIs resolve an id through this lookup immediately
    /// before executing a tab effect.
    pub fn tab_position(&self, id: &TabId) -> Option<TabPosition> {
        session::position_of(&self.tabs, id)
    }

    fn observe_focus(&mut self, fresh: Option<Focus>) -> Decision {
        if let Some(fresh) = fresh {
            self.left_behind =
                session::left_behind_by(&self.tabs, &self.panes, &fresh, self.left_behind.clone());
            self.focus = Some(fresh);
        }
        let mut decision = Decision::default();
        let answered = self.answer(&mut decision);
        decision.repaint = answered | self.resolve(&mut decision);
        decision
    }

    fn ran(&mut self, exit: Option<i32>, stderr: &[u8], call: &str) -> bool {
        match exit {
            Some(0) => self.client.reached(),
            _ => self.client.failed(call, said(stderr)),
        }
    }

    fn allowed(&self) -> bool {
        self.permission == Some(Permission::Granted)
    }

    fn run(&self, call: &str, args: &[&str], decision: &mut Decision) {
        let Some(program) = self.client.path() else {
            return;
        };
        decision.effects.push(Effect::Run(Command {
            call: call.to_string(),
            program: program.to_string(),
            args: args.iter().map(|word| (*word).to_string()).collect(),
        }));
    }

    fn register(&mut self, decision: &mut Decision) {
        let Some(name) = self.session_name.clone() else {
            return;
        };
        if self.registered || !self.allowed() {
            return;
        }
        self.registered = true;
        let mut owned = vec!["register".to_string(), self.multiplexer.clone(), name];
        let notifier = self
            .options
            .desktop
            .as_ref()
            .map(|notifier| notifier.words())
            .unwrap_or_default();
        if !notifier.is_empty() {
            owned.push("--notify".to_string());
            owned.extend(notifier.iter().cloned());
        }
        let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
        self.run("register", &borrowed, decision);
    }

    fn is_where_the_user_is(&self) -> bool {
        match (self.panes.sidebar_tab, self.focus.as_ref()) {
            (Some(mine), Some(focus)) => session::position_of(&self.tabs, &focus.tab) == Some(mine),
            _ => false,
        }
    }

    fn answer(&mut self, decision: &mut Decision) -> bool {
        let Some(pane) = self.focus.as_ref().and_then(|focus| match &focus.target {
            FocusTarget::Content(pane) => Some(pane.clone()),
            FocusTarget::Sidebar | FocusTarget::Other => None,
        }) else {
            return false;
        };
        let calling: Vec<Agent> = self
            .registry
            .iter()
            .filter(|agent| self.agent_panes.get(&agent.session) == Some(&pane))
            .filter(|agent| agent.turn == Turn::Attention)
            .cloned()
            .collect();
        let mut changed = false;
        for agent in &calling {
            self.answered.answer(agent);
            changed |= self.registry.seen(&agent.session);
        }
        if self.allowed() && self.is_where_the_user_is() {
            for agent in &calling {
                self.run("seen", &["seen", agent.session.as_str()], decision);
            }
        }
        changed
    }

    fn suppress(&mut self) -> bool {
        let mut changed = false;
        for settled in self.answered.settled(&self.registry) {
            changed |= self.registry.seen(&settled);
        }
        changed
    }

    fn resolve(&mut self, decision: &mut Decision) -> bool {
        let mut resolved = session::session(&self.tabs, &self.panes, self.focus.as_ref());
        self.leave_if_alone(&resolved, decision);
        for tab in &mut resolved {
            for pane in &mut tab.panes {
                pane.agents = self
                    .registry
                    .iter()
                    .filter(|agent| self.agent_panes.get(&agent.session) == Some(&pane.id))
                    .cloned()
                    .collect();
            }
        }
        let rows = tree::build_tree(&resolved, &self.options.view);
        let notices = self.notifications();
        let focused = self
            .focus
            .as_ref()
            .map(|focus| focus.target == FocusTarget::Sidebar)
            == Some(true);
        let changed = rows != self.rows || notices != self.notices || focused != self.focused;
        self.rows = rows;
        self.notices = notices;
        self.focused = focused;
        changed
    }

    fn leave_if_alone(&mut self, resolved: &[tree::Tab], decision: &mut Decision) {
        let Some(mine) = self.panes.sidebar_tab else {
            return;
        };
        let Some(tab) = resolved.iter().find(|tab| tab.position == mine) else {
            return;
        };
        if !tab.panes.is_empty() {
            self.had_company = true;
        } else if self.had_company {
            decision.effects.push(Effect::CloseSidebar);
        }
    }

    fn install_hooks(&mut self, decision: &mut Decision) {
        if self.options.install_hooks.is_none()
            || self.installed
            || !self.allowed()
            || !self.is_where_the_user_is()
        {
            return;
        }
        self.installed = true;
        self.run("install-hooks", &["install-hooks"], decision);
        decision
            .effects
            .push(Effect::Broadcast(Broadcast::HooksInstalled));
    }

    fn adopt(&mut self, snapshot: AgentSnapshot) -> Decision {
        match snapshot {
            AgentSnapshot::Incompatible => {
                let changed = !std::mem::replace(&mut self.mismatched, true);
                Decision {
                    repaint: changed,
                    effects: Vec::new(),
                }
            }
            AgentSnapshot::Compatible { registry, panes } => {
                let mut changed = self.registry != registry || self.agent_panes != panes;
                self.registry = registry;
                self.agent_panes = panes;
                changed |= self.suppress();
                let mut decision = Decision::default();
                changed |= self.answer(&mut decision);
                self.answered.prune(&self.registry);
                decision.repaint = changed && self.resolve(&mut decision);
                decision
            }
        }
    }

    fn user(&mut self, action: UserAction) -> Decision {
        let mut decision = Decision::default();
        match action {
            UserAction::Next => {
                self.step(1, &mut decision);
                decision.repaint = true;
            }
            UserAction::Previous => {
                self.step(-1, &mut decision);
                decision.repaint = true;
            }
            UserAction::Activate => self.activate(&mut decision),
            UserAction::Quit => decision.effects.push(Effect::Broadcast(Broadcast::Off)),
            UserAction::Click(line) => {
                if let Some(Some(key)) = self.painted.get(line).cloned() {
                    self.select(Some(key), &mut decision);
                    self.activate(&mut decision);
                    decision.repaint = true;
                }
            }
        }
        decision
    }

    fn message(&mut self, message: Broadcast) -> Decision {
        match message {
            Broadcast::Off => Decision::effect(Effect::CloseSidebar),
            Broadcast::HooksInstalled => {
                self.installed = true;
                Decision::default()
            }
            Broadcast::Selection(key) => {
                self.selected = Some(key);
                Decision::repaint()
            }
        }
    }

    fn selection(&self) -> Option<RowKey> {
        let keys = selection::keys(&self.rows, &self.registry, &self.options.view);
        selection::selected(&keys, self.selected.as_ref())
    }

    fn select(&mut self, key: Option<RowKey>, decision: &mut Decision) {
        if let Some(key) = &key {
            decision
                .effects
                .push(Effect::Broadcast(Broadcast::Selection(key.clone())));
        }
        self.selected = key;
    }

    fn step(&mut self, step: isize, decision: &mut Decision) {
        let mut keys = Vec::new();
        for key in self.painted.iter().flatten() {
            if keys.last() != Some(key) {
                keys.push(key.clone());
            }
        }
        let selection = self.selection();
        let Some(at) = keys.iter().position(|key| Some(key) == selection.as_ref()) else {
            self.select(keys.first().cloned(), decision);
            return;
        };
        let next = (at as isize + step).clamp(0, keys.len() as isize - 1) as usize;
        self.select(keys.get(next).cloned(), decision);
    }

    fn activate(&self, decision: &mut Decision) {
        match self.selection() {
            Some(RowKey::Pane(pane)) => self.go_to_pane(pane, decision),
            Some(RowKey::Tab(tab)) => {
                let Some(position) = self.tab_position(&tab) else {
                    return;
                };
                match session::first_pane(&self.panes, position) {
                    Some(pane) => self.go_to_pane(pane, decision),
                    None => {
                        self.stand_down(position, decision);
                        decision.effects.push(Effect::SwitchTab(tab));
                    }
                }
            }
            Some(RowKey::Agent(agent))
            | Some(RowKey::Section(agent))
            | Some(RowKey::Notification(agent)) => {
                if let Some(pane) = self.agent_panes.get(&agent) {
                    self.go_to_pane(pane.clone(), decision);
                }
            }
            None => {}
        }
    }

    fn go_to_pane(&self, pane: PaneId, decision: &mut Decision) {
        if let Some(tab) = session::tab_of_pane(&self.panes, &pane) {
            self.stand_down(tab, decision);
        }
        decision.effects.push(Effect::FocusPane(pane));
    }

    fn stand_down(&self, going_to: TabPosition, decision: &mut Decision) {
        if let Some(pane) = session::stand_down_to(&self.panes, self.left_behind.as_ref(), going_to)
        {
            decision.effects.push(Effect::FocusPane(pane));
        }
    }

    fn notifications(&self) -> Vec<Notification> {
        self.registry
            .calling()
            .into_iter()
            .map(|agent| Notification {
                session: agent.session.clone(),
                agent: agent.agent.clone(),
                color: NamedColor::of(agent),
                message: self.where_it_is(agent),
            })
            .collect()
    }

    fn where_it_is(&self, agent: &Agent) -> String {
        let tab = self
            .agent_panes
            .get(&agent.session)
            .and_then(|pane| session::tab_of_pane(&self.panes, pane))
            .and_then(|position| self.tabs.iter().find(|tab| tab.position == position))
            .map(|tab| tab.name.clone())
            .unwrap_or_default();
        let agent_label = label(agent, self.options.view.label);
        match (tab.is_empty(), agent_label.is_empty()) {
            (true, _) => agent_label,
            (false, true) => tab,
            (false, false) => format!("{tab} · {agent_label}"),
        }
    }

    fn frame(&self, area: Rect) -> Frame {
        if self.permission == Some(Permission::Denied) {
            return compose(
                &[Note {
                    heading: "no permission",
                    text: REFUSED,
                }],
                &[],
                &[],
                area,
                &self.options.view,
            );
        }
        let unreachable = self.client.why().map(|why| format!("{UNREACHABLE}. {why}"));
        let mut notes = Vec::new();
        if self.mismatched {
            notes.push(Note {
                heading: "out of step",
                text: MISMATCH,
            });
        }
        if let Some(text) = &unreachable {
            notes.push(Note {
                heading: "no client",
                text,
            });
        }
        compose(&notes, &self.rows, &self.notices, area, &self.options.view)
    }
}

fn said(stderr: &[u8]) -> &str {
    std::str::from_utf8(stderr)
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PaneReport, TabId, TabPanes};
    use agent_wrangler_core::agent::Meta;
    use agent_wrangler_core::origin::Origin;

    fn tab(id: &str, position: usize) -> TabReport {
        TabReport {
            id: TabId::new(id),
            position: TabPosition::at(position),
            name: format!("tab {id}"),
            active: position == 0,
        }
    }

    fn snapshot(sidebar: usize, tabs: &[(usize, &[&str])]) -> PaneSnapshot {
        PaneSnapshot {
            sidebar_tab: Some(TabPosition::at(sidebar)),
            tabs: tabs
                .iter()
                .map(|(position, panes)| TabPanes {
                    position: TabPosition::at(*position),
                    panes: panes
                        .iter()
                        .map(|id| PaneReport {
                            id: PaneId::new(*id),
                            title: format!("pane {id}"),
                            focused: false,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn focus(tab: &str, target: FocusTarget) -> Input {
        Input::FocusObserved(Some(Focus {
            tab: TabId::new(tab),
            target,
        }))
    }

    fn app() -> Application {
        Application::new(Options::default(), "tmux")
    }

    #[test]
    fn reports_store_facts_and_request_focus_as_an_effect() {
        let mut app = app();
        let decision = app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        assert_eq!(decision, Decision::effect(Effect::RefreshFocus));
        assert_eq!(app.tabs[0].id, TabId::new("10"));

        let panes = snapshot(0, &[(0, &["%7"])]);
        let decision = app.reduce(Input::PanesReported(panes.clone()));
        assert_eq!(decision, Decision::effect(Effect::RefreshFocus));
        assert_eq!(app.panes, panes);
    }

    #[test]
    fn session_and_permission_in_either_order_register_once() {
        for permission_first in [false, true] {
            let mut app = app();
            let mut effects = Vec::new();
            if permission_first {
                effects.extend(
                    app.reduce(Input::PermissionReported(Permission::Granted))
                        .effects,
                );
            }
            effects.extend(app.reduce(Input::SessionNamed("work".to_string())).effects);
            if !permission_first {
                effects.extend(
                    app.reduce(Input::PermissionReported(Permission::Granted))
                        .effects,
                );
            }
            let runs: Vec<Command> = effects
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::Run(command) => Some(command),
                    _ => None,
                })
                .collect();
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].args, ["register", "tmux", "work"]);
            assert!(app
                .reduce(Input::SessionNamed("work".to_string()))
                .effects
                .iter()
                .all(|effect| !matches!(effect, Effect::Run(_))));
        }
    }

    #[test]
    fn hook_installation_is_a_once_only_settled_event_effect() {
        let options = Options {
            install_hooks: Some("agent-wrangler".to_string()),
            ..Options::default()
        };
        let mut app = Application::new(options, "zellij");
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::PanesReported(snapshot(0, &[(0, &["7"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.reduce(Input::PermissionReported(Permission::Granted));

        let first = app.reduce(Input::EventSettled);
        assert!(matches!(
            first.effects.as_slice(),
            [Effect::Run(_), Effect::Broadcast(Broadcast::HooksInstalled)]
        ));
        assert!(app.reduce(Input::EventSettled).effects.is_empty());
    }

    #[test]
    fn activation_emits_stand_down_before_the_destination() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        app.reduce(Input::PanesReported(snapshot(
            0,
            &[(0, &["1"]), (1, &["2"])],
        )));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("1"))));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.render(Rect::new(0, 0, 30, 10));
        app.selected = Some(RowKey::Pane(PaneId::new("2")));

        let decision = app.reduce(Input::User(UserAction::Activate));
        assert_eq!(
            decision.effects,
            vec![
                Effect::FocusPane(PaneId::new("1")),
                Effect::FocusPane(PaneId::new("2")),
            ]
        );
    }

    #[test]
    fn a_selected_tab_survives_reordering_and_switches_by_stable_id() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        app.reduce(Input::PanesReported(snapshot(0, &[(0, &["1"]), (1, &[])])));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("1"))));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.selected = Some(RowKey::Tab(TabId::new("20")));

        app.reduce(Input::TabsReported(vec![tab("20", 0), tab("10", 1)]));
        app.reduce(Input::PanesReported(snapshot(1, &[(0, &[]), (1, &["1"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));

        assert_eq!(app.selection(), Some(RowKey::Tab(TabId::new("20"))));
        assert_eq!(
            app.tab_position(&TabId::new("20")),
            Some(TabPosition::at(0))
        );
        assert_eq!(app.tab_position(&TabId::new("gone")), None);
        assert_eq!(
            app.reduce(Input::User(UserAction::Activate)).effects,
            vec![
                Effect::FocusPane(PaneId::new("1")),
                Effect::SwitchTab(TabId::new("20")),
            ]
        );
    }

    #[test]
    fn an_agent_call_in_the_focused_pane_is_answered_by_an_effect() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::PanesReported(snapshot(0, &[(0, &["7"])])));
        app.reduce(Input::PermissionReported(Permission::Granted));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("7"))));
        let session = SessionId::new("call").unwrap();
        let mut agent = Agent::new(
            session.clone(),
            "claude",
            Meta::default(),
            Origin::default(),
        );
        agent.turn = Turn::Attention;
        agent.raised = 9;
        let mut registry = Registry::default();
        registry.report(agent);
        let decision = app.reduce(Input::Agents(AgentSnapshot::Compatible {
            registry,
            panes: BTreeMap::from([(session.clone(), PaneId::new("7"))]),
        }));

        assert!(decision.effects.iter().any(|effect| matches!(
            effect,
            Effect::Run(Command { call, args, .. }) if call == "seen" && args == &["seen", "call"]
        )));
        assert_eq!(app.registry.get(&session).unwrap().turn, Turn::Idle);
    }

    #[test]
    fn a_sidebar_closes_only_after_its_tab_has_had_company() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::PanesReported(snapshot(0, &[(0, &["7"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.reduce(Input::PanesReported(snapshot(0, &[(0, &[])])));
        let decision = app.reduce(focus("10", FocusTarget::Sidebar));
        assert!(decision.effects.contains(&Effect::CloseSidebar));
    }

    #[test]
    fn rendering_returns_a_frame_and_keeps_ansi_out_of_the_application() {
        let mut app = app();
        app.reduce(Input::PermissionReported(Permission::Denied));
        let rendered = app.render(Rect::new(0, 0, 24, 4));
        assert_eq!(rendered.frame.area(), Rect::new(0, 0, 24, 4));
        assert_eq!(rendered.frame.lines().len(), 4);
        assert_eq!(rendered.selection, None);
    }

    #[test]
    fn command_results_change_only_the_client_state_and_repaint_decision() {
        let mut app = app();
        assert!(
            app.reduce(Input::CommandFinished {
                exit: Some(0),
                stderr: Vec::new(),
                call: "register".to_string(),
            })
            .repaint
        );
        assert!(
            !app.reduce(Input::CommandFinished {
                exit: Some(0),
                stderr: Vec::new(),
                call: "register".to_string(),
            })
            .repaint
        );
        assert!(
            app.reduce(Input::CommandFinished {
                exit: None,
                stderr: b"busy\nmore detail".to_vec(),
                call: "seen".to_string(),
            })
            .repaint
        );
        assert_eq!(app.client.why(), None, "one transient failure is tolerated");
    }

    #[test]
    fn incoming_messages_update_state_without_broadcasting_back() {
        let mut app = app();
        let selected = RowKey::Pane(PaneId::new("%7"));
        let decision = app.reduce(Input::Message(Broadcast::Selection(selected.clone())));
        assert!(decision.repaint);
        assert!(decision.effects.is_empty());
        assert_eq!(app.selected, Some(selected));

        assert_eq!(
            app.reduce(Input::Message(Broadcast::Off)),
            Decision::effect(Effect::CloseSidebar)
        );
        app.reduce(Input::Message(Broadcast::HooksInstalled));
        assert!(app.installed);

        let decision = app.reduce(Input::Agents(AgentSnapshot::Compatible {
            registry: Registry::default(),
            panes: BTreeMap::new(),
        }));
        assert!(decision
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::Broadcast(_))));
    }

    #[test]
    fn a_click_uses_the_last_rendered_line_and_orders_its_effects() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::PanesReported(snapshot(0, &[(0, &["%7"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.render(Rect::new(0, 0, 30, 5));

        let decision = app.reduce(Input::User(UserAction::Click(1)));
        assert_eq!(
            decision.effects,
            vec![
                Effect::Broadcast(Broadcast::Selection(RowKey::Pane(PaneId::new("%7")))),
                Effect::FocusPane(PaneId::new("%7")),
            ]
        );
        assert!(decision.repaint);
    }

    #[test]
    fn a_title_observation_updates_portable_state_then_refreshes_focus() {
        let mut app = app();
        app.reduce(Input::PanesReported(snapshot(0, &[(0, &["%7"])])));
        let decision = app.reduce(Input::PaneTitleObserved {
            pane: PaneId::new("%7"),
            title: Some("editor".to_string()),
        });
        assert_eq!(decision, Decision::effect(Effect::RefreshFocus));
        assert_eq!(app.panes.tabs[0].panes[0].title, "editor");

        let decision = app.reduce(Input::PaneTitleObserved {
            pane: PaneId::new("%7"),
            title: None,
        });
        assert_eq!(decision, Decision::default());
    }

    #[test]
    fn agents_replace_their_pane_row_and_all_agents_in_one_pane_are_placed() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::PanesReported(snapshot(0, &[(0, &["7"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));

        let first = SessionId::new("first").unwrap();
        let second = SessionId::new("second").unwrap();
        let mut registry = Registry::default();
        for session in [&first, &second] {
            registry.report(Agent::new(
                session.clone(),
                "claude",
                Meta::default(),
                Origin::default(),
            ));
        }
        app.reduce(Input::Agents(AgentSnapshot::Compatible {
            registry,
            panes: BTreeMap::from([
                (first.clone(), PaneId::new("7")),
                (second.clone(), PaneId::new("7")),
            ]),
        }));

        let keys: Vec<Option<RowKey>> = app.rows.iter().map(|row| row.key.clone()).collect();
        assert!(keys.contains(&Some(RowKey::Agent(first))));
        assert!(keys.contains(&Some(RowKey::Agent(second))));
        assert!(!keys.contains(&Some(RowKey::Pane(PaneId::new("7")))));
    }
}
