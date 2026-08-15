use std::collections::BTreeMap;

use agent_wrangler_core::agent::{self, Agent, Record};
use agent_wrangler_core::registry::Registry;
use agent_wrangler_sidebar::{
    AgentSnapshot, Broadcast, Focus, FocusTarget, PaneId, PaneReport, SessionLayout,
    SidebarPaneReport, TabId, TabLayout, TabReport,
};
use agent_wrangler_ui::model::{RowKey, TabPosition};
use zellij_tile::prelude::{PaneId as ZellijPaneId, PaneInfo, PaneManifest, TabInfo};

pub const SELECTION_MESSAGE: &str = "wrangler:selection";
pub const OFF_MESSAGE: &str = "wrangler:off";
pub const INSTALLED_MESSAGE: &str = "wrangler:hooks-installed";

const SESSION_VAR: &str = "ZELLIJ_SESSION_NAME";
const PANE_VAR: &str = "ZELLIJ_PANE_ID";

pub fn tabs(reported: Vec<TabInfo>) -> Vec<TabReport> {
    reported
        .into_iter()
        .map(|tab| TabReport {
            id: TabId::new(tab.tab_id.to_string()),
            position: TabPosition::at(tab.position),
            name: tab.name,
            active: tab.active,
        })
        .collect()
}

pub fn layout(manifest: PaneManifest, plugin_id: u32) -> SessionLayout {
    let mut tabs: Vec<TabLayout> = manifest
        .panes
        .into_iter()
        .map(|(position, panes)| TabLayout {
            position: TabPosition::at(position),
            other_focused: panes
                .iter()
                .any(|pane| pane.is_plugin && pane.id != plugin_id && pane.is_focused),
            sidebar_pane: panes
                .iter()
                .find(|pane| pane.is_plugin && pane.id == plugin_id)
                .map(|pane| SidebarPaneReport {
                    focused: pane.is_focused,
                }),
            content_panes: listed(panes),
        })
        .collect();
    tabs.sort_by_key(|tab| tab.position.zero_based());
    SessionLayout { tabs }
}

fn listed(panes: Vec<PaneInfo>) -> Vec<PaneReport> {
    let mut panes: Vec<PaneInfo> = panes
        .into_iter()
        .filter(|pane| !pane.is_plugin && pane.is_selectable && !pane.is_suppressed)
        .collect();
    panes.sort_by_key(|pane| (pane.is_floating, pane.pane_y, pane.pane_x));
    panes
        .into_iter()
        .map(|pane| PaneReport {
            id: PaneId::new(pane.id.to_string()),
            title: pane.title,
            focused: pane.is_focused,
        })
        .collect()
}

pub fn numeric_pane(id: &PaneId) -> Option<u32> {
    id.as_str().parse().ok()
}

/// The focus a host focus query answers with.
///
/// `tab_id` is a stable tab identity and not a position, whatever the host API
/// calls the number: the two agree until a tab before the focused one closes,
/// and this takes it as the identity, which is what it is. `pane` says which
/// kind of pane holds the focus, so this sidebar can tell the user's own pane
/// from itself and from another plugin.
pub fn focus(tab_id: usize, pane: ZellijPaneId, plugin_id: u32) -> Focus {
    let target = match pane {
        ZellijPaneId::Terminal(id) => FocusTarget::Content(PaneId::new(id.to_string())),
        ZellijPaneId::Plugin(id) if id == plugin_id => FocusTarget::Sidebar,
        ZellijPaneId::Plugin(_) => FocusTarget::Other,
    };
    Focus {
        tab: TabId::new(tab_id.to_string()),
        target,
    }
}

/// Whether a frame is owed, between the change that made one stale and the
/// draw that replaces it.
///
/// A host sends one change as several events, and drawing each of them draws
/// the halves of a change as well as the whole of it. Holding the debt here
/// lets one draw settle a burst of them, so what reaches the screen is the
/// state the burst arrived at rather than every state it passed through.
#[derive(Default)]
pub struct RenderSchedule {
    owed: bool,
}

impl RenderSchedule {
    /// Record that the frame on screen is out of date.
    ///
    /// True when the draw has to be arranged, which is only for the first of a
    /// burst: the rest are already covered by the draw it asks for, and that
    /// is what makes them one frame instead of several.
    pub fn invalidate(&mut self) -> bool {
        !std::mem::replace(&mut self.owed, true)
    }

    /// Settle the debt, if there is one, at the moment the draw can happen.
    pub fn due(&mut self) -> bool {
        std::mem::take(&mut self.owed)
    }
}

pub fn decode_message(name: &str, payload: Option<&str>) -> Option<Broadcast> {
    match name {
        OFF_MESSAGE => Some(Broadcast::Off),
        INSTALLED_MESSAGE => Some(Broadcast::HooksInstalled),
        SELECTION_MESSAGE => payload.and_then(RowKey::decode).map(Broadcast::Selection),
        _ => None,
    }
}

pub fn encode_message(message: Broadcast) -> (&'static str, Option<String>) {
    match message {
        Broadcast::Off => (OFF_MESSAGE, None),
        Broadcast::HooksInstalled => (INSTALLED_MESSAGE, None),
        Broadcast::Selection(key) => (SELECTION_MESSAGE, Some(key.encode())),
    }
}

pub fn agents(payload: &str, session: &str) -> Option<AgentSnapshot> {
    let (format, records) = agent::read_state(payload)?;
    if format != agent::FORMAT {
        return Some(AgentSnapshot::Incompatible);
    }
    let mut registry = Registry::default();
    let mut panes = BTreeMap::new();
    for line in records.split('\n') {
        let Record::Known(agent) = Agent::decode(line) else {
            continue;
        };
        if agent.origin.get(SESSION_VAR) != Some(session) {
            continue;
        }
        if let Some(pane) = agent.origin.get(PANE_VAR).filter(|pane| !pane.is_empty()) {
            panes.insert(agent.session.clone(), PaneId::new(pane));
        }
        registry.report(agent);
    }
    Some(AgentSnapshot::Compatible { registry, panes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::agent::{Meta, SessionId};
    use agent_wrangler_core::origin::Origin;
    use std::collections::HashMap;

    fn pane(id: u32, title: &str, y: usize, x: usize) -> PaneInfo {
        PaneInfo {
            id,
            title: title.to_string(),
            pane_y: y,
            pane_x: x,
            is_selectable: true,
            ..PaneInfo::default()
        }
    }

    #[test]
    fn tabs_preserve_ids_separately_from_positions() {
        let reported = tabs(vec![TabInfo {
            tab_id: 9,
            position: 1,
            name: "second".to_string(),
            active: true,
            ..TabInfo::default()
        }]);
        assert_eq!(reported[0].id, TabId::new("9"));
        assert_eq!(reported[0].position, TabPosition::at(1));
    }

    #[test]
    fn panes_are_filtered_ordered_and_locate_this_sidebar() {
        let sidebar = PaneInfo {
            id: 7,
            is_plugin: true,
            is_selectable: true,
            ..PaneInfo::default()
        };
        let hidden = PaneInfo {
            id: 8,
            is_selectable: false,
            ..PaneInfo::default()
        };
        let mut floating = pane(3, "floating", 0, 0);
        floating.is_floating = true;
        let manifest = PaneManifest {
            panes: HashMap::from([(
                2,
                vec![
                    floating,
                    pane(2, "right", 0, 20),
                    pane(1, "left", 0, 0),
                    hidden,
                    sidebar,
                ],
            )]),
        };
        let normalized = layout(manifest, 7);
        assert_eq!(
            normalized.tabs[0].sidebar_pane,
            Some(SidebarPaneReport { focused: false })
        );
        let ids: Vec<&str> = normalized.tabs[0]
            .content_panes
            .iter()
            .map(|pane| pane.id.as_str())
            .collect();
        assert_eq!(ids, ["1", "2", "3"]);
    }

    #[test]
    fn a_terminal_pane_and_sidebar_with_the_same_number_do_not_collide() {
        let sidebar = PaneInfo {
            id: 7,
            is_plugin: true,
            is_selectable: true,
            ..PaneInfo::default()
        };
        let manifest = PaneManifest {
            panes: HashMap::from([(0, vec![pane(7, "terminal", 0, 0), sidebar])]),
        };
        let normalized = layout(manifest, 7);
        assert_eq!(
            normalized.tabs[0].sidebar_pane,
            Some(SidebarPaneReport { focused: false })
        );
        assert_eq!(normalized.tabs[0].content_panes.len(), 1);
        assert_eq!(normalized.tabs[0].content_panes[0].id, PaneId::new("7"));
    }

    #[test]
    fn every_tab_containing_this_sidebar_is_marked() {
        let sidebar = PaneInfo {
            id: 7,
            is_plugin: true,
            is_selectable: true,
            ..PaneInfo::default()
        };
        let manifest = PaneManifest {
            panes: HashMap::from([(0, vec![sidebar.clone()]), (1, vec![sidebar])]),
        };

        let normalized = layout(manifest, 7);
        assert!(normalized
            .tabs
            .iter()
            .all(|tab| tab.sidebar_pane == Some(SidebarPaneReport { focused: false })));
    }

    #[test]
    fn plugin_focus_is_preserved_for_this_sidebar_and_other_plugins() {
        let mine = PaneInfo {
            id: 7,
            is_plugin: true,
            is_focused: true,
            ..PaneInfo::default()
        };
        let other = PaneInfo {
            id: 8,
            is_plugin: true,
            is_focused: true,
            ..PaneInfo::default()
        };
        let normalized = layout(
            PaneManifest {
                panes: HashMap::from([(0, vec![mine]), (1, vec![other])]),
            },
            7,
        );

        assert_eq!(
            normalized.tabs[0].sidebar_pane,
            Some(SidebarPaneReport { focused: true })
        );
        assert!(!normalized.tabs[0].other_focused);
        assert!(normalized.tabs[1].other_focused);
    }

    #[test]
    fn focus_distinguishes_content_this_sidebar_and_other_ui() {
        assert_eq!(focus(9, ZellijPaneId::Terminal(7), 7).tab, TabId::new("9"));
        assert_eq!(
            focus(4, ZellijPaneId::Terminal(7), 7).target,
            FocusTarget::Content(PaneId::new("7"))
        );
        assert_eq!(
            focus(4, ZellijPaneId::Plugin(7), 7).target,
            FocusTarget::Sidebar
        );
        assert_eq!(
            focus(4, ZellijPaneId::Plugin(9), 7).target,
            FocusTarget::Other
        );
    }

    #[test]
    fn opaque_ids_are_validated_only_at_the_zellij_effect_boundary() {
        assert_eq!(numeric_pane(&PaneId::new("42")), Some(42));
        assert_eq!(numeric_pane(&PaneId::new("%42")), None);
    }

    #[test]
    fn a_burst_of_changes_arranges_one_draw_between_them() {
        let mut schedule = RenderSchedule::default();
        assert!(schedule.invalidate());
        assert!(!schedule.invalidate());
        assert!(!schedule.invalidate());
        assert!(schedule.due());
        // The draw that settled the burst leaves nothing owed, so a later one
        // that finds no debt draws nothing and a change after it starts over.
        assert!(!schedule.due());
        assert!(schedule.invalidate());
    }

    #[test]
    fn nothing_is_owed_until_something_changes() {
        assert!(!RenderSchedule::default().due());
    }

    fn reported(id: &str, session: &str, pane: &str) -> Agent {
        Agent::new(
            SessionId::new(id).unwrap(),
            "claude",
            Meta::default(),
            Origin::from(|name| match name {
                SESSION_VAR => Some(session.to_string()),
                PANE_VAR => Some(pane.to_string()),
                _ => None,
            }),
        )
    }

    #[test]
    fn agent_reports_are_scoped_and_placed_at_the_adapter_boundary() {
        let mut registry = Registry::default();
        registry.report(reported("mine", "work", "11"));
        registry.report(reported("theirs", "other", "12"));
        let payload = agent::state(&registry.encode());
        let Some(AgentSnapshot::Compatible { registry, panes }) = agents(&payload, "work") else {
            panic!("compatible state");
        };
        assert!(registry.get(&SessionId::new("mine").unwrap()).is_some());
        assert!(registry.get(&SessionId::new("theirs").unwrap()).is_none());
        assert_eq!(
            panes.get(&SessionId::new("mine").unwrap()),
            Some(&PaneId::new("11"))
        );
    }

    #[test]
    fn shared_messages_keep_the_existing_wire_names() {
        let message = Broadcast::Selection(RowKey::Pane(PaneId::new("%7")));
        let (name, payload) = encode_message(message.clone());
        assert_eq!(name, SELECTION_MESSAGE);
        assert_eq!(decode_message(name, payload.as_deref()), Some(message));
    }

    #[test]
    fn tab_selections_use_an_unambiguous_stable_id_wire_key() {
        let message = Broadcast::Selection(RowKey::Tab(TabId::new("9")));
        let (name, payload) = encode_message(message.clone());
        assert_eq!(payload.as_deref(), Some("tab-id:9"));
        assert_eq!(decode_message(name, payload.as_deref()), Some(message));
        assert_eq!(decode_message(name, Some("tab:0")), None);
    }

    #[test]
    fn malformed_agent_messages_are_ignored_and_other_formats_are_reported() {
        assert_eq!(agents("", "work"), None);
        assert_eq!(
            agents("wrangler 999\n", "work"),
            Some(AgentSnapshot::Incompatible)
        );
    }
}
