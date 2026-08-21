use std::collections::{BTreeMap, BTreeSet};

use agent_wrangler_core::agent::{Agent, SessionId, Turn};
use agent_wrangler_core::label::label;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_ui::frame::{compose, Frame, Note};
use agent_wrangler_ui::model::{NamedColor, Notification, PaneId, RowKey, TabPosition};
use agent_wrangler_ui::{selection, tree, Rect};

use crate::calls::Answered;
use crate::client::Client;
use crate::model::{
    AgentSnapshot, Broadcast, Command, Decision, Effect, Focus, FocusTarget, Input,
    InteractionItem, Permission, RenderedView, SessionLayout, TabId, TabReport, Told, UserAction,
    ViewAction,
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
    layout: SessionLayout,
    registry: Registry,
    agent_panes: BTreeMap<SessionId, PaneId>,
    answered: Answered,
    client: Client,
    session_name: Option<String>,
    registered: bool,
    selected: Option<RowKey>,
    permission: Option<Permission>,
    rendered: Option<RenderedView>,
    visible: bool,
    observed_focus: Option<Focus>,
    left_behind: BTreeMap<TabId, PaneId>,
    tabs_with_company: BTreeSet<TabId>,
    installed: bool,
    mismatched: bool,
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
            Input::VisibilityChanged(visible) => self.change_visibility(visible),
            Input::TabsReported(tabs) => self.report_tabs(tabs),
            Input::LayoutReported(layout) => self.report_layout(layout),
            Input::PaneChanged(pane) => Decision::effect(Effect::RefreshPaneTitle(pane)),
            Input::PaneTitleObserved { pane, title } => self.observe_pane_title(pane, title),
            Input::FocusObserved(focus) => self.observe_focus(focus),
            Input::SessionNamed(name) => self.name_session(name),
            Input::PermissionReported(permission) => self.report_permission(permission),
            Input::CommandFinished { exit, stderr, call } => {
                self.finish_command(exit, &stderr, &call)
            }
            Input::User(action) => self.user(action),
            Input::Message(message) => self.message(message),
            Input::Agents(snapshot) => self.adopt(snapshot),
            Input::EventSettled => self.confirmed_effects(),
        }
    }

    pub fn render(&mut self, area: Rect) -> RenderedView {
        let rendered = self.view(area);
        self.rendered = Some(rendered.clone());
        rendered
    }

    pub fn session_name(&self) -> Option<&str> {
        self.session_name.as_deref()
    }

    /// The latest reported position of a stable tab identity.
    ///
    /// A positional host API resolves an id through this lookup. The
    /// resolution occurs immediately before the application runs a tab
    /// effect.
    pub fn tab_position(&self, id: &TabId) -> Option<TabPosition> {
        session::position_of(&self.tabs, id)
    }

    fn report_tabs(&mut self, tabs: Vec<TabReport>) -> Decision {
        let changed = self.tabs != tabs;
        self.left_behind
            .retain(|id, _| tabs.iter().any(|tab| &tab.id == id));
        self.tabs_with_company
            .retain(|id| tabs.iter().any(|tab| &tab.id == id));
        self.tabs = tabs;
        let mut decision = self.observe_reported_focus();
        decision.request_repaint(changed);
        decision
    }

    fn report_layout(&mut self, layout: SessionLayout) -> Decision {
        let changed = self.layout != layout;
        self.layout = layout;
        let mut decision = self.observe_reported_focus();
        decision.request_repaint(changed);
        decision
    }

    fn observe_pane_title(&mut self, pane: PaneId, title: Option<String>) -> Decision {
        let mut changed = false;
        if let Some(title) = title {
            for tab in &mut self.layout.tabs {
                for candidate in &mut tab.content_panes {
                    if candidate.id == pane && candidate.title != title {
                        candidate.title = title.clone();
                        changed = true;
                    }
                }
            }
        }
        let mut decision = self.confirmed_effects();
        decision.request_repaint(changed);
        decision
    }

    fn change_visibility(&mut self, visible: bool) -> Decision {
        let changed = self.visible != visible;
        self.visible = visible;
        // A sidebar that becomes visible has no fresh report to work from. It
        // holds reports from the last time that the user looked at it. New
        // reports are much slower than an answer to a question. The sidebar
        // therefore asks, and it decides nothing from the reports in hand. The
        // answer arrives in time for the frame that this decision paints.
        let mut decision = if visible {
            Decision::effect(Effect::RefreshFocus)
        } else {
            Decision::default()
        };
        decision.request_repaint(changed);
        decision
    }

    fn observe_reported_focus(&mut self) -> Decision {
        // A tab report carries the stable identity of the active tab. A layout
        // report carries the focused pane of that tab. This code joins the
        // latest snapshot of each event by position. It does not ask the host
        // for a second focus snapshot. The result is the best reading of the
        // two reports in hand. It is not a settled fact to act on.
        let fresh = self.tabs.iter().find(|tab| tab.active).and_then(|tab| {
            let layout = self
                .layout
                .tabs
                .iter()
                .find(|layout| layout.position == tab.position)?;
            let target = layout
                .content_panes
                .iter()
                .find(|pane| pane.focused)
                .map(|pane| FocusTarget::Content(pane.id.clone()))
                .or_else(|| {
                    layout
                        .sidebar_pane
                        .as_ref()
                        .filter(|pane| pane.focused)
                        .map(|_| FocusTarget::Sidebar)
                })
                .or_else(|| layout.other_focused.then_some(FocusTarget::Other))?;
            Some(Focus {
                tab: tab.id.clone(),
                target,
            })
        });
        self.observe_focus(fresh)
    }

    fn name_session(&mut self, name: String) -> Decision {
        self.session_name = Some(name);
        let mut decision = Decision::effect(Effect::StopSessionDiscovery);
        self.register(&mut decision);
        decision
    }

    fn report_permission(&mut self, permission: Permission) -> Decision {
        let changed = self.permission != Some(permission);
        self.permission = Some(permission);
        let mut decision = Decision::default();
        decision.request_repaint(changed);
        self.register(&mut decision);
        decision
    }

    fn observe_focus(&mut self, fresh: Option<Focus>) -> Decision {
        let changed = self.observed_focus != fresh;
        self.observed_focus = fresh;
        let mut decision = self.confirmed_effects();
        decision.request_repaint(changed);
        decision
    }

    fn finish_command(&mut self, exit: Option<i32>, stderr: &[u8], call: &str) -> Decision {
        let changed = match exit {
            Some(0) => self.client.reached(),
            _ => self.client.failed(call, said(stderr)),
        };
        let mut decision = Decision::default();
        decision.request_repaint(changed);
        decision
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

    fn answer(&mut self, focus: &Focus, decision: &mut Decision) -> bool {
        let FocusTarget::Content(pane) = &focus.target else {
            return false;
        };
        let calling: Vec<Agent> = self
            .registry
            .iter()
            .filter(|agent| self.agent_panes.get(&agent.session) == Some(pane))
            .filter(|agent| agent.turn == Turn::Attention)
            .cloned()
            .collect();
        let mut changed = false;
        for agent in &calling {
            self.answered.answer(agent);
            changed |= self.registry.seen(&agent.session);
        }
        if self.allowed() {
            for agent in &calling {
                decision
                    .effects
                    .push(Effect::Tell(Told::Seen(agent.session.clone())));
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

    /// The session, as the reports and the focus in hand describe it.
    fn reconciled_session(&self) -> session::ReconciledSession {
        let agent_panes: BTreeSet<PaneId> = self.agent_panes.values().cloned().collect();
        let mut resolved = session::reconcile(
            &self.tabs,
            &self.layout,
            self.visible,
            self.observed_focus.as_ref(),
            &agent_panes,
        );
        for tab in &mut resolved.tabs {
            for pane in &mut tab.panes {
                pane.agents = self
                    .registry
                    .iter()
                    .filter(|agent| self.agent_panes.get(&agent.session) == Some(&pane.id))
                    .cloned()
                    .collect();
            }
        }
        resolved
    }

    fn confirmed_effects(&mut self) -> Decision {
        let resolved = self.reconciled_session();
        let session::ReconciledFocus::Confirmed(focus) = resolved.focus else {
            return Decision::default();
        };
        let Some(focused_tab) = session::position_of(&self.tabs, &focus.tab) else {
            return Decision::default();
        };
        let remembered = self.left_behind.get(&focus.tab).cloned();
        if let Some(pane) = session::left_behind_by(&self.tabs, &self.layout, &focus, remembered) {
            self.left_behind.insert(focus.tab.clone(), pane);
        }
        let mut decision = Decision::default();
        let answered = self.answer(&focus, &mut decision);
        self.leave_if_alone(&focus.tab, focused_tab, &resolved.tabs, &mut decision);
        self.install_hooks(&mut decision);
        decision.request_repaint(answered);
        decision
    }

    fn leave_if_alone(
        &mut self,
        focused_id: &TabId,
        focused_tab: TabPosition,
        resolved: &[tree::Tab],
        decision: &mut Decision,
    ) {
        if !self
            .layout
            .tabs
            .iter()
            .any(|tab| tab.position == focused_tab && tab.sidebar_pane.is_some())
        {
            return;
        }
        let Some(tab) = resolved.iter().find(|tab| tab.position == focused_tab) else {
            return;
        };
        if !tab.panes.is_empty() {
            self.tabs_with_company.insert(focused_id.clone());
        } else if self.tabs_with_company.contains(focused_id) {
            decision.effects.push(Effect::CloseSidebar);
        }
    }

    fn install_hooks(&mut self, decision: &mut Decision) {
        if self.options.install_hooks.is_none() || self.installed || !self.allowed() {
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
                let mut decision = Decision::default();
                decision.request_repaint(changed);
                decision
            }
            AgentSnapshot::Compatible { registry, panes } => {
                let mut changed = self.registry != registry || self.agent_panes != panes;
                self.registry = registry;
                self.agent_panes = panes;
                changed |= self.suppress();
                let mut decision = self.confirmed_effects();
                self.answered.prune(&self.registry);
                decision.request_repaint(changed);
                decision
            }
        }
    }

    fn user(&mut self, action: UserAction) -> Decision {
        let mut decision = Decision::default();
        match action {
            UserAction::Next => self.step(1, &mut decision),
            UserAction::Previous => self.step(-1, &mut decision),
            UserAction::Activate => self.activate(&mut decision),
            UserAction::Quit => decision.effects.push(Effect::Broadcast(Broadcast::Off)),
            UserAction::Click(line) => {
                let item = self
                    .rendered
                    .as_ref()
                    .and_then(|view| view.item_at(line))
                    .cloned();
                if let Some(item) = item {
                    self.select(Some(item.key), &mut decision);
                    self.activate_action(&item.action, &mut decision);
                    decision.request_repaint(true);
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
                let changed = self.selected.as_ref() != Some(&key);
                self.selected = Some(key);
                let mut decision = Decision::default();
                decision.request_repaint(changed);
                decision
            }
        }
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
        let Some(view) = self.rendered.as_ref() else {
            return;
        };
        let items = view.selectable_items();
        let selection = view.selection.as_ref();
        let next = match items.iter().position(|item| Some(&item.key) == selection) {
            Some(at) => (at as isize + step).clamp(0, items.len() as isize - 1) as usize,
            None => 0,
        };
        let key = items.get(next).map(|item| item.key.clone());
        if key.is_some() {
            self.select(key, decision);
            decision.request_repaint(true);
        }
    }

    fn activate(&mut self, decision: &mut Decision) {
        let action = self
            .rendered
            .as_ref()
            .and_then(RenderedView::selected_item)
            .map(|item| item.action.clone());
        if let Some(action) = action {
            self.activate_action(&action, decision);
        }
    }

    fn activate_action(&mut self, action: &ViewAction, decision: &mut Decision) {
        match action {
            ViewAction::ActivatePane(pane) => {
                if session::tab_of_pane(&self.layout, pane).is_some() {
                    self.go_to_pane(pane.clone(), decision);
                }
            }
            ViewAction::ActivateTab(tab) => {
                let Some(position) = self.tab_position(tab) else {
                    return;
                };
                match session::first_pane(&self.layout, position) {
                    Some(pane) => self.go_to_pane(pane, decision),
                    None => {
                        self.stand_down(position, decision);
                        decision.effects.push(Effect::SwitchTab(tab.clone()));
                    }
                }
            }
            ViewAction::ActivateAgent(agent) => {
                if self.registry.get(agent).is_some() {
                    if let Some(pane) = self.agent_panes.get(agent) {
                        if session::tab_of_pane(&self.layout, pane).is_some() {
                            self.go_to_pane(pane.clone(), decision);
                        }
                    }
                }
            }
        }
    }

    fn go_to_pane(&mut self, pane: PaneId, decision: &mut Decision) {
        if let Some(tab) = session::tab_of_pane(&self.layout, &pane) {
            self.stand_down(tab, decision);
        }
        decision.effects.push(Effect::FocusPane(pane));
    }

    fn stand_down(&self, going_to: TabPosition, decision: &mut Decision) {
        let Some(focus) = self.observed_focus.as_ref() else {
            return;
        };
        let Some(leaving_from) = session::position_of(&self.tabs, &focus.tab) else {
            return;
        };
        if let Some(pane) = session::stand_down_to(
            &self.layout,
            self.left_behind.get(&focus.tab),
            leaving_from,
            going_to,
        ) {
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
            .and_then(|pane| session::tab_of_pane(&self.layout, pane))
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

    fn view(&self, area: Rect) -> RenderedView {
        let session = self.reconciled_session();
        let frame = self.frame(area, &session.tabs);
        let interactions: Vec<Option<InteractionItem>> = frame
            .lines()
            .iter()
            .map(|row| row.key.as_ref().map(interaction))
            .collect();
        let visible = selection::keys(frame.lines());
        let focused = matches!(
            session.focus,
            session::ReconciledFocus::Confirmed(Focus {
                target: FocusTarget::Sidebar,
                ..
            })
        );
        let selection = focused
            .then(|| selection::selected(&visible, self.selected.as_ref()))
            .flatten();
        RenderedView {
            frame,
            interactions,
            selection,
        }
    }

    fn frame(&self, area: Rect, session: &[tree::Tab]) -> Frame {
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
        let rows = tree::build_tree(session, &self.options.view);
        let notices = self.notifications();
        compose(&notes, &rows, &notices, area, &self.options.view)
    }
}

fn interaction(key: &RowKey) -> InteractionItem {
    let action = match key {
        RowKey::Tab(tab) => ViewAction::ActivateTab(tab.clone()),
        RowKey::Pane(pane) => ViewAction::ActivatePane(pane.clone()),
        RowKey::Agent(agent) | RowKey::Section(agent) | RowKey::Notification(agent) => {
            ViewAction::ActivateAgent(agent.clone())
        }
    };
    InteractionItem {
        key: key.clone(),
        action,
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
    use crate::model::{PaneReport, PaneVisibility, SidebarPaneReport, TabId, TabLayout};
    use agent_wrangler_core::agent::Meta;
    use agent_wrangler_core::origin::Origin;
    use agent_wrangler_ui::ansi;

    fn tab(id: &str, position: usize) -> TabReport {
        TabReport {
            id: TabId::new(id),
            position: TabPosition::at(position),
            name: format!("tab {id}"),
            active: position == 0,
        }
    }

    fn layout(sidebar: usize, tabs: &[(usize, &[&str])]) -> SessionLayout {
        SessionLayout {
            tabs: tabs
                .iter()
                .map(|(position, panes)| TabLayout {
                    position: TabPosition::at(*position),
                    other_focused: false,
                    content_panes: panes
                        .iter()
                        .map(|id| PaneReport {
                            id: PaneId::new(*id),
                            title: format!("pane {id}"),
                            focused: false,
                            visibility: PaneVisibility::OnScreen,
                        })
                        .collect(),
                    sidebar_pane: (*position == sidebar)
                        .then_some(SidebarPaneReport { focused: false }),
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
        let mut app = Application::new(Options::default(), "tmux");
        app.reduce(Input::VisibilityChanged(true));
        app
    }

    fn agent(id: &str, turn: Turn) -> Agent {
        let mut agent = Agent::new(
            SessionId::new(id).unwrap(),
            "claude",
            Meta::default(),
            Origin::default(),
        );
        agent.turn = turn;
        agent.raised = 1;
        agent
    }

    fn agents(entries: &[(&str, &str, Turn)]) -> AgentSnapshot {
        let mut registry = Registry::default();
        let mut panes = BTreeMap::new();
        for (id, pane, turn) in entries {
            let agent = agent(id, *turn);
            panes.insert(agent.session.clone(), PaneId::new(*pane));
            registry.report(agent);
        }
        AgentSnapshot::Compatible { registry, panes }
    }

    fn repaints(decision: &Decision) -> bool {
        decision.effects.contains(&Effect::Repaint)
    }

    #[test]
    fn reports_store_facts_and_derive_focus_without_an_effect() {
        let mut app = app();
        let decision = app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        assert_eq!(decision, Decision::repaint());
        assert_eq!(app.tabs[0].id, TabId::new("10"));

        let mut panes = layout(0, &[(0, &["%7"])]);
        panes.tabs[0].sidebar_pane.as_mut().unwrap().focused = true;
        let decision = app.reduce(Input::LayoutReported(panes.clone()));
        assert_eq!(decision, Decision::repaint());
        assert_eq!(app.layout, panes);
        assert_eq!(
            app.observed_focus,
            Some(Focus {
                tab: TabId::new("10"),
                target: FocusTarget::Sidebar,
            })
        );
    }

    #[test]
    fn tab_reports_prune_state_for_closed_stable_ids() {
        let mut app = app();
        for id in ["closed", "live"] {
            let id = TabId::new(id);
            app.left_behind.insert(id.clone(), PaneId::new(id.as_str()));
            app.tabs_with_company.insert(id);
        }

        app.reduce(Input::TabsReported(vec![tab("live", 0)]));

        assert_eq!(app.left_behind.len(), 1);
        assert!(app.left_behind.contains_key(&TabId::new("live")));
        assert!(!app.left_behind.contains_key(&TabId::new("closed")));
        assert_eq!(app.tabs_with_company.len(), 1);
        assert!(app.tabs_with_company.contains(&TabId::new("live")));
        assert!(!app.tabs_with_company.contains(&TabId::new("closed")));
    }

    #[test]
    fn being_shown_asks_where_the_user_is_rather_than_reading_held_reports() {
        let mut app = Application::new(Options::default(), "zellij");
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        let mut reported = layout(0, &[(0, &["7"])]);
        reported.tabs[0].sidebar_pane.as_mut().unwrap().focused = true;
        app.reduce(Input::LayoutReported(reported));
        app.selected = Some(RowKey::Pane(PaneId::new("7")));
        assert_eq!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Unknown
        );
        assert_eq!(app.render(Rect::new(0, 0, 30, 5)).selection, None);

        // The reports in hand say that this sidebar has the focus. The sidebar
        // becomes visible, so it asks and does not act on them. The host sent
        // those reports to a sidebar that nobody looked at. The answer is
        // quicker and better than the reports that will replace them.
        let shown = app.reduce(Input::VisibilityChanged(true));
        assert_eq!(shown.effects, vec![Effect::RefreshFocus, Effect::Repaint]);

        // The answer replaces the reports in hand, wherever it puts the user.
        let answered = app.reduce(focus("10", FocusTarget::Content(PaneId::new("7"))));
        assert!(answered.effects.contains(&Effect::Repaint));
        assert_eq!(
            app.observed_focus,
            Some(Focus {
                tab: TabId::new("10"),
                target: FocusTarget::Content(PaneId::new("7")),
            })
        );
        // The sidebar is not where the user is, so it draws no selection.
        assert_eq!(app.render(Rect::new(0, 0, 30, 5)).selection, None);

        app.reduce(focus("10", FocusTarget::Sidebar));
        assert_eq!(
            app.render(Rect::new(0, 0, 30, 5)).selection,
            Some(RowKey::Pane(PaneId::new("7")))
        );

        // A sidebar that goes away asks nothing. A hidden sidebar cannot own
        // the focus.
        assert_eq!(
            app.reduce(Input::VisibilityChanged(false)),
            Decision::repaint()
        );
        assert_eq!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Unknown
        );
    }

    #[test]
    fn a_focus_the_host_cannot_answer_leaves_the_sidebar_placing_nobody() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("7"))));

        app.reduce(Input::VisibilityChanged(false));
        assert_eq!(
            app.reduce(Input::VisibilityChanged(true)).effects,
            vec![Effect::RefreshFocus, Effect::Repaint]
        );
        app.reduce(Input::FocusObserved(None));

        assert_eq!(app.observed_focus, None);
        assert_eq!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Unknown
        );
    }

    #[test]
    fn a_failed_focus_query_clears_the_cached_observation() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("7"))));
        assert!(app.observed_focus.is_some());

        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::FocusObserved(None));
        assert_eq!(app.observed_focus, None);
        assert_eq!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Unknown
        );
    }

    #[test]
    fn focus_is_derived_again_for_topology_reports_but_not_title_updates() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));
        assert!(matches!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Confirmed(_)
        ));

        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        assert_eq!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Unknown
        );
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.reduce(Input::PaneTitleObserved {
            pane: PaneId::new("7"),
            title: Some("new".to_string()),
        });
        assert!(matches!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Confirmed(_)
        ));
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
        app.reduce(Input::VisibilityChanged(true));
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
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
    fn hook_installation_waits_for_confirmed_visible_focus() {
        let options = Options {
            install_hooks: Some("agent-wrangler".to_string()),
            ..Options::default()
        };
        let mut app = Application::new(options, "zellij");
        app.reduce(Input::VisibilityChanged(true));
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
        app.reduce(Input::PermissionReported(Permission::Granted));

        assert_eq!(app.reduce(Input::EventSettled), Decision::default());
        assert!(!app.installed);

        let confirmed = app.reduce(focus("10", FocusTarget::Sidebar));
        assert!(matches!(
            confirmed.effects.as_slice(),
            [
                Effect::Run(_),
                Effect::Broadcast(Broadcast::HooksInstalled),
                Effect::Repaint
            ]
        ));
        assert!(app.installed);
    }

    #[test]
    fn activation_emits_stand_down_before_the_destination() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        app.reduce(Input::LayoutReported(layout(
            0,
            &[(0, &["1"]), (1, &["2"])],
        )));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("1"))));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.selected = Some(RowKey::Pane(PaneId::new("2")));
        app.render(Rect::new(0, 0, 30, 10));

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
    fn stand_down_restores_the_pane_remembered_for_the_focused_sidebar() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        let mut reported = layout(0, &[(0, &["1"]), (1, &["2"])]);
        reported.tabs[1].sidebar_pane = Some(SidebarPaneReport { focused: false });
        app.reduce(Input::LayoutReported(reported));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("1"))));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.reduce(focus("20", FocusTarget::Content(PaneId::new("2"))));
        app.reduce(focus("20", FocusTarget::Sidebar));

        let mut decision = Decision::default();
        app.stand_down(TabPosition::at(0), &mut decision);
        assert_eq!(decision.effects, vec![Effect::FocusPane(PaneId::new("2"))]);
    }

    #[test]
    fn a_selected_tab_survives_reordering_and_switches_by_stable_id() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["1"]), (1, &[])])));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("1"))));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.selected = Some(RowKey::Tab(TabId::new("20")));
        let rendered = app.render(Rect::new(0, 0, 30, 10));
        assert_eq!(rendered.selection, Some(RowKey::Tab(TabId::new("20"))));

        app.reduce(Input::TabsReported(vec![tab("20", 0), tab("10", 1)]));
        app.reduce(Input::LayoutReported(layout(1, &[(0, &[]), (1, &["1"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));

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
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
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
            Effect::Tell(Told::Seen(session)) if session.as_str() == "call"
        )));
        assert_eq!(app.registry.get(&session).unwrap().turn, Turn::Idle);
    }

    #[test]
    fn a_call_waits_for_pending_focus_to_be_confirmed() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
        app.reduce(Input::PermissionReported(Permission::Granted));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("7"))));
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        let session = SessionId::new("call").unwrap();

        let pending = app.reduce(Input::Agents(agents(&[("call", "7", Turn::Attention)])));
        assert!(!pending
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Tell(Told::Seen(_)))));
        assert_eq!(app.registry.get(&session).unwrap().turn, Turn::Attention);

        let confirmed = app.reduce(focus("10", FocusTarget::Content(PaneId::new("7"))));
        assert!(confirmed.effects.iter().any(|effect| matches!(
            effect,
            Effect::Tell(Told::Seen(session)) if session.as_str() == "call"
        )));
        assert_eq!(app.registry.get(&session).unwrap().turn, Turn::Idle);
        assert!(!app
            .reduce(focus("10", FocusTarget::Content(PaneId::new("7"))))
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Tell(Told::Seen(_)))));
    }

    #[test]
    fn a_hidden_sidebar_does_not_answer_from_stale_focus() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
        app.reduce(Input::PermissionReported(Permission::Granted));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("7"))));
        app.reduce(Input::VisibilityChanged(false));
        let session = SessionId::new("call").unwrap();

        let decision = app.reduce(Input::Agents(agents(&[("call", "7", Turn::Attention)])));
        assert!(!decision
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Tell(Told::Seen(_)))));
        assert_eq!(app.registry.get(&session).unwrap().turn, Turn::Attention);
        assert!(app.answered.settled(&app.registry).is_empty());
    }

    #[test]
    fn a_sidebar_closes_only_after_its_tab_has_had_company() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &[])])));
        let decision = app.reduce(focus("10", FocusTarget::Sidebar));
        assert!(decision.effects.contains(&Effect::CloseSidebar));
    }

    #[test]
    fn company_is_tracked_in_the_focused_tab_when_every_tab_has_a_sidebar() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        let mut reported = layout(0, &[(0, &[]), (1, &["7"])]);
        reported.tabs[1].sidebar_pane = Some(SidebarPaneReport { focused: false });
        app.reduce(Input::LayoutReported(reported.clone()));
        app.reduce(focus("20", FocusTarget::Sidebar));
        assert!(app.tabs_with_company.contains(&TabId::new("20")));
        assert!(!app.tabs_with_company.contains(&TabId::new("10")));

        reported.tabs[1].content_panes.clear();
        app.reduce(Input::LayoutReported(reported));
        let decision = app.reduce(focus("20", FocusTarget::Sidebar));
        assert!(decision.effects.contains(&Effect::CloseSidebar));
    }

    #[test]
    fn an_empty_tab_does_not_close_while_topology_is_pending() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));
        assert!(app.tabs_with_company.contains(&TabId::new("10")));

        let report = app.reduce(Input::LayoutReported(layout(0, &[(0, &[])])));
        assert!(!report.effects.contains(&Effect::CloseSidebar));
        assert!(!app
            .reduce(Input::EventSettled)
            .effects
            .contains(&Effect::CloseSidebar));

        let confirmed = app.reduce(focus("10", FocusTarget::Sidebar));
        assert!(confirmed.effects.contains(&Effect::CloseSidebar));
    }

    #[test]
    fn a_tab_report_that_has_outrun_the_pane_report_decides_nothing() {
        let mut app = app();
        let mut both = vec![tab("10", 0), tab("20", 1)];
        both[0].active = false;
        both[1].active = true;
        app.reduce(Input::TabsReported(both));
        // Every tab holds a sidebar, and each tab reports the focused pane
        // inside itself. Only the tab report tells which tab holds the user.
        let mut reported = layout(0, &[(0, &[]), (1, &["7"])]);
        reported.tabs[0].sidebar_pane = Some(SidebarPaneReport { focused: true });
        reported.tabs[1].sidebar_pane = Some(SidebarPaneReport { focused: true });
        app.reduce(Input::LayoutReported(reported));
        let sidebar_of_20 = Focus {
            tab: TabId::new("20"),
            target: FocusTarget::Sidebar,
        };
        assert_eq!(app.observed_focus, Some(sidebar_of_20.clone()));
        assert!(app.tabs_with_company.contains(&TabId::new("20")));

        // The first tab closes. Until the pane report catches up, position 0
        // names the open tab in one report and the closed tab in the other.
        // The entry for the closed tab has no panes.
        let closed = app.reduce(Input::TabsReported(vec![tab("20", 0)]));
        assert!(!closed.effects.contains(&Effect::CloseSidebar));
        assert_eq!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Pending
        );
        // The sidebar still draws the frame. A report that changes the tabs
        // asks for a repaint, whatever it does to the focus. Only the gutter
        // waits.
        assert!(closed.effects.contains(&Effect::Repaint));
        assert_eq!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Pending
        );

        let mut caught_up = layout(0, &[(0, &["7"])]);
        caught_up.tabs[0].sidebar_pane = Some(SidebarPaneReport { focused: true });
        let settled = app.reduce(Input::LayoutReported(caught_up));
        assert!(!settled.effects.contains(&Effect::CloseSidebar));
        assert_eq!(
            app.reconciled_session().focus,
            session::ReconciledFocus::Confirmed(sidebar_of_20)
        );
    }

    #[test]
    fn company_is_recorded_only_from_confirmed_topology() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));

        app.reduce(Input::EventSettled);
        assert!(!app.tabs_with_company.contains(&TabId::new("10")));

        app.reduce(focus("10", FocusTarget::Sidebar));
        assert!(app.tabs_with_company.contains(&TabId::new("10")));
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
    fn equal_state_and_area_produce_equal_views_and_ansi() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["1"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));

        let first = app.render(Rect::new(0, 0, 30, 8));
        let second = app.render(Rect::new(0, 0, 30, 8));
        assert_eq!(first, second);
        assert_eq!(
            ansi::pane(&first.frame, first.selection.as_ref()),
            ansi::pane(&second.frame, second.selection.as_ref())
        );
    }

    #[test]
    fn only_items_that_fit_in_the_frame_are_interactive() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        app.reduce(Input::LayoutReported(layout(
            0,
            &[(0, &["1"]), (1, &["2"])],
        )));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.reduce(Input::Agents(agents(&[("call", "1", Turn::Attention)])));

        let rendered = app.render(Rect::new(0, 0, 30, 2));
        let keys: Vec<&RowKey> = rendered
            .selectable_items()
            .into_iter()
            .map(|item| &item.key)
            .collect();
        assert_eq!(
            keys,
            vec![
                &RowKey::Tab(TabId::new("10")),
                &RowKey::Agent(SessionId::new("call").unwrap()),
            ]
        );
        assert!(!rendered.interactions.iter().flatten().any(|item| matches!(
            item.key,
            RowKey::Tab(ref tab) if tab == &TabId::new("20")
        )));
        assert!(!rendered
            .interactions
            .iter()
            .flatten()
            .any(|item| matches!(item.key, RowKey::Notification(_))));
    }

    #[test]
    fn navigation_deduplicates_wrapped_notifications_in_screen_order() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["1", "2"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.reduce(Input::Agents(agents(&[
            ("first", "1", Turn::Attention),
            ("second", "2", Turn::Attention),
        ])));
        let first = SessionId::new("first").unwrap();
        let second = SessionId::new("second").unwrap();
        app.selected = Some(RowKey::Notification(first.clone()));

        let rendered = app.render(Rect::new(0, 0, 30, 28));
        let notifications: Vec<&InteractionItem> = rendered
            .selectable_items()
            .into_iter()
            .filter(|item| matches!(item.key, RowKey::Notification(_)))
            .collect();
        assert_eq!(notifications.len(), 2);
        assert_eq!(
            notifications
                .iter()
                .map(|item| &item.key)
                .collect::<Vec<_>>(),
            vec![
                &RowKey::Notification(first),
                &RowKey::Notification(second.clone()),
            ]
        );

        let decision = app.reduce(Input::User(UserAction::Next));
        assert_eq!(
            decision.effects,
            vec![
                Effect::Broadcast(Broadcast::Selection(RowKey::Notification(second))),
                Effect::Repaint,
            ]
        );
    }

    #[test]
    fn resizing_replaces_the_line_interaction_map() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        app.reduce(Input::LayoutReported(layout(
            0,
            &[(0, &["1"]), (1, &["2"])],
        )));
        app.reduce(focus("10", FocusTarget::Sidebar));

        let short = app.render(Rect::new(0, 0, 30, 2));
        assert_eq!(short.interactions.len(), 2);
        assert!(short
            .selectable_items()
            .iter()
            .all(|item| item.key != RowKey::Tab(TabId::new("20"))));

        let tall = app.render(Rect::new(0, 0, 30, 8));
        assert_eq!(tall.interactions.len(), 8);
        assert!(tall
            .selectable_items()
            .iter()
            .any(|item| item.key == RowKey::Tab(TabId::new("20"))));
    }

    #[test]
    fn command_results_change_only_the_client_state_and_repaint_decision() {
        let mut app = app();
        assert!(repaints(&app.reduce(Input::CommandFinished {
            exit: Some(0),
            stderr: Vec::new(),
            call: "register".to_string(),
        })));
        assert!(!repaints(&app.reduce(Input::CommandFinished {
            exit: Some(0),
            stderr: Vec::new(),
            call: "register".to_string(),
        })));
        assert!(repaints(&app.reduce(Input::CommandFinished {
            exit: None,
            stderr: b"busy\nmore detail".to_vec(),
            call: "install-hooks".to_string(),
        })));
        assert_eq!(app.client.why(), None, "one transient failure is tolerated");
    }

    #[test]
    fn incoming_messages_update_state_without_broadcasting_back() {
        let mut app = app();
        let selected = RowKey::Pane(PaneId::new("%7"));
        let decision = app.reduce(Input::Message(Broadcast::Selection(selected.clone())));
        assert_eq!(decision.effects, vec![Effect::Repaint]);
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
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["%7"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.render(Rect::new(0, 0, 30, 5));

        let decision = app.reduce(Input::User(UserAction::Click(1)));
        assert_eq!(
            decision.effects,
            vec![
                Effect::Broadcast(Broadcast::Selection(RowKey::Pane(PaneId::new("%7")))),
                Effect::FocusPane(PaneId::new("%7")),
                Effect::Repaint,
            ]
        );
    }

    #[test]
    fn clicks_keep_the_last_rendered_meaning_until_repaint() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("old-tab", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["old-pane"])])));
        app.reduce(focus("old-tab", FocusTarget::Sidebar));
        app.render(Rect::new(0, 0, 30, 5));

        app.reduce(Input::TabsReported(vec![tab("new-tab", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["new-pane"])])));
        app.reduce(focus("new-tab", FocusTarget::Sidebar));
        let decision = app.reduce(Input::User(UserAction::Click(1)));

        assert_eq!(
            decision.effects,
            vec![
                Effect::Broadcast(Broadcast::Selection(RowKey::Pane(PaneId::new("old-pane",)))),
                Effect::Repaint,
            ]
        );
    }

    #[test]
    fn interaction_before_the_first_render_is_a_no_op() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["1"])])));
        app.reduce(focus("10", FocusTarget::Sidebar));

        for action in [
            UserAction::Next,
            UserAction::Previous,
            UserAction::Activate,
            UserAction::Click(0),
        ] {
            assert_eq!(app.reduce(Input::User(action)), Decision::default());
        }
    }

    #[test]
    fn peer_selection_changes_interaction_only_after_rendering() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        app.reduce(Input::LayoutReported(layout(
            0,
            &[(0, &["1"]), (1, &["2"])],
        )));
        app.reduce(focus("10", FocusTarget::Content(PaneId::new("1"))));
        app.reduce(focus("10", FocusTarget::Sidebar));
        app.selected = Some(RowKey::Tab(TabId::new("10")));
        app.render(Rect::new(0, 0, 30, 8));

        app.reduce(Input::Message(Broadcast::Selection(RowKey::Pane(
            PaneId::new("2"),
        ))));
        assert_eq!(
            app.reduce(Input::User(UserAction::Activate)).effects,
            vec![Effect::FocusPane(PaneId::new("1")),]
        );

        let mut arrived = layout(0, &[(0, &["1"]), (1, &["2"])]);
        arrived.tabs[0].content_panes[0].focused = true;
        app.reduce(Input::LayoutReported(arrived));
        let mut returned = layout(0, &[(0, &["1"]), (1, &["2"])]);
        returned.tabs[0].sidebar_pane.as_mut().unwrap().focused = true;
        app.reduce(Input::LayoutReported(returned));
        let rendered = app.render(Rect::new(0, 0, 30, 8));
        assert_eq!(rendered.selection, Some(RowKey::Pane(PaneId::new("2"))));
        assert_eq!(
            app.reduce(Input::User(UserAction::Activate)).effects,
            vec![
                Effect::FocusPane(PaneId::new("1")),
                Effect::FocusPane(PaneId::new("2")),
            ]
        );
    }

    #[test]
    fn stale_tab_pane_and_agent_actions_are_safe_no_ops() {
        let mut tab_app = app();
        tab_app.reduce(Input::TabsReported(vec![tab("old", 0)]));
        tab_app.reduce(Input::LayoutReported(layout(0, &[(0, &[])])));
        tab_app.reduce(focus("old", FocusTarget::Sidebar));
        tab_app.render(Rect::new(0, 0, 30, 4));
        tab_app.reduce(Input::TabsReported(vec![tab("replacement", 0)]));
        assert!(tab_app
            .reduce(Input::User(UserAction::Activate))
            .effects
            .is_empty());

        let mut pane_app = app();
        pane_app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        pane_app.reduce(Input::LayoutReported(layout(0, &[(0, &["old"])])));
        pane_app.reduce(focus("10", FocusTarget::Sidebar));
        pane_app.selected = Some(RowKey::Pane(PaneId::new("old")));
        pane_app.render(Rect::new(0, 0, 30, 4));
        pane_app.reduce(Input::LayoutReported(layout(0, &[(0, &["replacement"])])));
        assert!(pane_app
            .reduce(Input::User(UserAction::Activate))
            .effects
            .is_empty());

        let mut agent_app = app();
        agent_app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        agent_app.reduce(Input::LayoutReported(layout(0, &[(0, &["1"])])));
        agent_app.reduce(focus("10", FocusTarget::Sidebar));
        let session = SessionId::new("agent").unwrap();
        agent_app.reduce(Input::Agents(agents(&[("agent", "1", Turn::Idle)])));
        agent_app.selected = Some(RowKey::Agent(session));
        agent_app.render(Rect::new(0, 0, 30, 4));
        agent_app.reduce(Input::Agents(AgentSnapshot::Compatible {
            registry: Registry::default(),
            panes: BTreeMap::new(),
        }));
        assert!(agent_app
            .reduce(Input::User(UserAction::Activate))
            .effects
            .is_empty());
    }

    #[test]
    fn moved_panes_and_agents_resolve_by_stable_id() {
        let mut pane_app = app();
        pane_app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        pane_app.reduce(Input::LayoutReported(layout(
            0,
            &[(0, &["held", "moving"]), (1, &[])],
        )));
        pane_app.reduce(focus("10", FocusTarget::Content(PaneId::new("held"))));
        pane_app.reduce(focus("10", FocusTarget::Sidebar));
        pane_app.selected = Some(RowKey::Pane(PaneId::new("moving")));
        pane_app.render(Rect::new(0, 0, 30, 8));
        let mut moved = layout(0, &[(0, &["held"]), (1, &["moving"])]);
        moved.tabs[0].sidebar_pane.as_mut().unwrap().focused = true;
        pane_app.reduce(Input::LayoutReported(moved));
        assert_eq!(
            pane_app.reduce(Input::User(UserAction::Activate)).effects,
            vec![
                Effect::FocusPane(PaneId::new("held")),
                Effect::FocusPane(PaneId::new("moving")),
            ]
        );

        let mut agent_app = app();
        agent_app.reduce(Input::TabsReported(vec![tab("10", 0), tab("20", 1)]));
        agent_app.reduce(Input::LayoutReported(layout(
            0,
            &[(0, &["held", "old-host"]), (1, &["new-host"])],
        )));
        agent_app.reduce(focus("10", FocusTarget::Content(PaneId::new("held"))));
        agent_app.reduce(focus("10", FocusTarget::Sidebar));
        let session = SessionId::new("agent").unwrap();
        agent_app.reduce(Input::Agents(agents(&[("agent", "old-host", Turn::Idle)])));
        agent_app.selected = Some(RowKey::Agent(session.clone()));
        agent_app.render(Rect::new(0, 0, 30, 8));
        agent_app.reduce(Input::Agents(agents(&[("agent", "new-host", Turn::Idle)])));
        assert_eq!(
            agent_app.reduce(Input::User(UserAction::Activate)).effects,
            vec![
                Effect::FocusPane(PaneId::new("held")),
                Effect::FocusPane(PaneId::new("new-host")),
            ]
        );
    }

    #[test]
    fn a_title_observation_updates_portable_state_without_querying_focus() {
        let mut app = app();
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["%7"])])));
        let decision = app.reduce(Input::PaneTitleObserved {
            pane: PaneId::new("%7"),
            title: Some("editor".to_string()),
        });
        assert_eq!(decision, Decision::repaint());
        assert_eq!(app.layout.tabs[0].content_panes[0].title, "editor");

        let decision = app.reduce(Input::PaneTitleObserved {
            pane: PaneId::new("%7"),
            title: None,
        });
        assert_eq!(decision, Decision::default());
    }

    #[test]
    fn an_agent_in_a_parked_pane_keeps_its_row_and_the_way_back_to_it() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        let mut reported = layout(0, &[(0, &["%1", "%parked"])]);
        reported.tabs[0].content_panes[1].visibility = PaneVisibility::Parked;
        app.reduce(Input::LayoutReported(reported));
        app.reduce(focus("10", FocusTarget::Sidebar));

        let keys = |app: &mut Application| -> Vec<Option<RowKey>> {
            app.render(Rect::new(0, 0, 30, 10))
                .interactions
                .iter()
                .map(|item| item.as_ref().map(|item| item.key.clone()))
                .collect()
        };
        assert!(!keys(&mut app).contains(&Some(RowKey::Pane(PaneId::new("%parked")))));

        app.reduce(Input::Agents(agents(&[("hidden", "%parked", Turn::Idle)])));
        let session = SessionId::new("hidden").unwrap();
        assert!(keys(&mut app).contains(&Some(RowKey::Agent(session.clone()))));

        app.reduce(Input::Message(Broadcast::Selection(RowKey::Agent(session))));
        app.render(Rect::new(0, 0, 30, 10));
        let decision = app.reduce(Input::User(UserAction::Activate));
        assert!(decision
            .effects
            .contains(&Effect::FocusPane(PaneId::new("%parked"))));
    }

    #[test]
    fn agents_replace_their_pane_row_and_all_agents_in_one_pane_are_placed() {
        let mut app = app();
        app.reduce(Input::TabsReported(vec![tab("10", 0)]));
        app.reduce(Input::LayoutReported(layout(0, &[(0, &["7"])])));
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

        let rendered = app.render(Rect::new(0, 0, 30, 10));
        let keys: Vec<Option<RowKey>> = rendered
            .interactions
            .iter()
            .map(|item| item.as_ref().map(|item| item.key.clone()))
            .collect();
        assert!(keys.contains(&Some(RowKey::Agent(first))));
        assert!(keys.contains(&Some(RowKey::Agent(second))));
        assert!(!keys.contains(&Some(RowKey::Pane(PaneId::new("7")))));
    }
}
