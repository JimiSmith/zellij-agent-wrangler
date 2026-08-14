use std::collections::BTreeMap;

use agent_wrangler_core::agent::SessionId;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_ui::frame::Frame;
pub use agent_wrangler_ui::model::{PaneId, TabId};
use agent_wrangler_ui::model::{RowKey, TabPosition};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabReport {
    pub id: TabId,
    pub position: TabPosition,
    pub name: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneReport {
    pub id: PaneId,
    pub title: String,
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabPanes {
    pub position: TabPosition,
    pub panes: Vec<PaneReport>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneSnapshot {
    pub tabs: Vec<TabPanes>,
    pub sidebar_tab: Option<TabPosition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Content(PaneId),
    Sidebar,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    pub tab: TabId,
    pub target: FocusTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserAction {
    Next,
    Previous,
    Activate,
    Quit,
    Click(usize),
}

/// A host-neutral operation offered by one item in a rendered sidebar.
///
/// The target is always a stable identity. The application validates it
/// against its latest authoritative state immediately before producing host
/// effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewAction {
    ActivateTab(TabId),
    ActivatePane(PaneId),
    ActivateAgent(SessionId),
}

/// What one visible frame line means when the user interacts with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionItem {
    pub key: RowKey,
    pub action: ViewAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Broadcast {
    Off,
    HooksInstalled,
    Selection(RowKey),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentSnapshot {
    Incompatible,
    Compatible {
        registry: Registry,
        panes: BTreeMap<SessionId, PaneId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    VisibilityChanged(bool),
    TabsReported(Vec<TabReport>),
    PanesReported(PaneSnapshot),
    PaneChanged(PaneId),
    PaneTitleObserved {
        pane: PaneId,
        title: Option<String>,
    },
    FocusObserved(Option<Focus>),
    SessionNamed(String),
    PermissionReported(Permission),
    CommandFinished {
        exit: Option<i32>,
        stderr: Vec<u8>,
        call: String,
    },
    User(UserAction),
    Message(Broadcast),
    Agents(AgentSnapshot),
    EventSettled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub call: String,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Repaint,
    RefreshFocus,
    RefreshPaneTitle(PaneId),
    Run(Command),
    Broadcast(Broadcast),
    FocusPane(PaneId),
    SwitchTab(TabId),
    StopSessionDiscovery,
    CloseSidebar,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub effects: Vec<Effect>,
}

impl Decision {
    pub fn repaint() -> Self {
        Decision::effect(Effect::Repaint)
    }

    pub fn effect(effect: Effect) -> Self {
        Decision {
            effects: vec![effect],
        }
    }

    pub fn request_repaint(&mut self, repaint: bool) {
        if repaint && !self.effects.contains(&Effect::Repaint) {
            self.effects.push(Effect::Repaint);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedView {
    pub frame: Frame,
    /// One entry per frame line. Non-interactive lines contain `None`.
    pub interactions: Vec<Option<InteractionItem>>,
    /// The selection resolved against this view's visible interactions.
    pub selection: Option<RowKey>,
}

impl RenderedView {
    /// The interaction attached to a particular rendered line.
    pub fn item_at(&self, line: usize) -> Option<&InteractionItem> {
        self.interactions.get(line)?.as_ref()
    }

    /// The interaction whose key was visibly selected in this view.
    pub fn selected_item(&self) -> Option<&InteractionItem> {
        let selected = self.selection.as_ref()?;
        self.interactions
            .iter()
            .flatten()
            .find(|item| &item.key == selected)
    }

    /// Each distinct selectable item in visible screen order.
    ///
    /// Notification titles and their wrapped body lines share an item, so they
    /// are returned once even though every one of their lines is clickable.
    pub fn selectable_items(&self) -> Vec<&InteractionItem> {
        let mut items = Vec::new();
        for item in self.interactions.iter().flatten() {
            if !items
                .iter()
                .any(|seen: &&InteractionItem| seen.key == item.key)
            {
                items.push(item);
            }
        }
        items
    }
}
