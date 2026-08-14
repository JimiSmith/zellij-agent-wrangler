use std::collections::BTreeMap;

use agent_wrangler_core::agent::SessionId;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_ui::frame::Frame;
pub use agent_wrangler_ui::model::PaneId;
use agent_wrangler_ui::model::{RowKey, TabPosition};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TabId(String);

impl TabId {
    pub fn new(id: impl Into<String>) -> Self {
        TabId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
    RefreshFocus,
    RefreshPaneTitle(PaneId),
    Run(Command),
    Broadcast(Broadcast),
    FocusPane(PaneId),
    SwitchTab(TabPosition),
    StopSessionDiscovery,
    CloseSidebar,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub repaint: bool,
    pub effects: Vec<Effect>,
}

impl Decision {
    pub fn repaint() -> Self {
        Decision {
            repaint: true,
            effects: Vec::new(),
        }
    }

    pub fn effect(effect: Effect) -> Self {
        Decision {
            repaint: false,
            effects: vec![effect],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedView {
    pub frame: Frame,
    pub selection: Option<RowKey>,
}
