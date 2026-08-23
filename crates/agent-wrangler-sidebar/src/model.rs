use std::collections::BTreeMap;

use agent_wrangler_core::agent::SessionId;
pub use agent_wrangler_core::client_message::ClientMessage;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_ui::frame::Frame;
pub use agent_wrangler_ui::model::{PaneId, TabId};
use agent_wrangler_ui::model::{RowKey, TabPosition};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabReport {
    pub id: TabId,
    /// Where this tab sits in the order of the tabs. Contiguous and zero based.
    pub position: TabPosition,
    /// The number that this tab's row draws, and that the user types to reach
    /// the tab.
    ///
    /// This is not the position. A host is free to number its tabs from any
    /// value, and to leave a gap where a tab closed. A host that numbers its
    /// tabs by their order reports the position plus one here.
    pub displayed_index: String,
    pub name: String,
    pub active: bool,
}

/// Whether the host draws a pane, or holds it off screen.
///
/// A host parks a pane that it keeps but does not draw. Zellij parks every
/// member of a stacked list except the one on screen, and it parks the pane
/// that a scrollback editor stands in for. A parked pane keeps its process, its
/// id and its title, and the host brings it back the moment that something
/// focuses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneVisibility {
    OnScreen,
    Parked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneReport {
    pub id: PaneId,
    pub title: String,
    pub focused: bool,
    pub visibility: PaneVisibility,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarPaneReport {
    /// Whether the host event reports this particular sidebar as focused.
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLayout {
    pub position: TabPosition,
    pub content_panes: Vec<PaneReport>,
    pub sidebar_pane: Option<SidebarPaneReport>,
    /// Whether the host event reports a different plugin pane as focused.
    pub other_focused: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionLayout {
    pub tabs: Vec<TabLayout>,
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

/// How a client asks the daemon to reach it.
///
/// Both fields are opaque here. `kind` is the word that the client program
/// accepts after `register`, and `id` is the value that follows that word.
/// Nothing in this crate reads either field, so this crate names no host.
///
/// This is not the session name. A client is free to be reached under a name
/// that it derived, and the session keeps the name that the user gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserAction {
    Next,
    Previous,
    Activate,
    Quit,
    Click(usize),
}

/// A host-neutral operation that one item in a rendered sidebar offers.
///
/// The target is always a stable identity. The application validates the
/// target against its latest authoritative state. The validation occurs
/// immediately before the application produces host effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewAction {
    ActivateTab(TabId),
    ActivatePane(PaneId),
    ActivateAgent(SessionId),
}

/// The result of a user interaction with one visible frame line.
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
    LayoutReported(SessionLayout),
    PaneChanged(PaneId),
    PaneTitleObserved {
        pane: PaneId,
        title: Option<String>,
    },
    FocusObserved(Option<Focus>),
    SessionNamed(String),
    RegistrationReported(Registration),
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

/// One program for the client to run, with the word that names the call.
///
/// The client hands `call` back with the result, so the reducer knows which
/// call finished. The client turns `program` and `args` into a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramToRun {
    pub call: String,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Repaint,
    /// Asks the host where the user is. [`Input::FocusObserved`] holds the
    /// answer.
    ///
    /// A report gives the position of the user at the moment of the report. If
    /// no report will arrive, this question is worth its round trip. In other
    /// conditions, it is not. A newly visible sidebar holds the last position
    /// that it heard. The reports that replace that position are much slower
    /// than the answer.
    RefreshFocus,
    RefreshPaneTitle(PaneId),
    Run(ProgramToRun),
    /// Says one thing to the daemon, on the transport that the daemon already
    /// holds open to reach this client.
    ///
    /// This effect costs no process, so a sidebar can speak as often as it
    /// needs to. [`Effect::Run`] keeps the two things that no such
    /// transport can carry: the registration that opens it, and the hooks that
    /// are files on disk.
    Tell(ClientMessage),
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
    /// Notification titles and their wrapped body lines share an item. This
    /// method returns each shared item once, although each of its lines is
    /// clickable.
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
