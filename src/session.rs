//! Reading the shape of the session out of what zellij reports.
//!
//! This is the only place the sidebar's own vocabulary meets zellij's: what
//! arrives is every pane of every tab, the sidebar's own pane and the UI bars
//! among them, in no particular order.

use zellij_tile::prelude::{PaneId, PaneInfo, PaneManifest, TabInfo};

use crate::tree::{Pane, Tab};

/// Whether the sidebar lists this pane.
///
/// Plugin panes are dropped, which covers the sidebar itself along with the tab
/// bar and the status bar. Unselectable panes are UI furniture by definition,
/// and a suppressed pane is one the user cannot currently see.
fn is_listed(pane: &PaneInfo) -> bool {
    !pane.is_plugin && pane.is_selectable && !pane.is_suppressed
}

/// The panes of one tab in the order they are drawn: tiled panes in reading
/// order, then the floating ones that sit above them.
///
/// Ids are unique among the panes kept here: zellij numbers terminal panes and
/// plugin panes in separate sequences, and every plugin pane has been dropped.
fn panes_of(panes: &[PaneInfo]) -> Vec<Pane> {
    let mut listed: Vec<&PaneInfo> = panes.iter().filter(|pane| is_listed(pane)).collect();
    listed.sort_by_key(|pane| (pane.is_floating, pane.pane_y, pane.pane_x));
    listed
        .into_iter()
        .map(|pane| Pane::new(pane.id, &pane.title, pane.is_focused))
        .collect()
}

/// Which tab a plugin pane is in, found by the plugin's own id. `None` until the
/// manifest has caught up with the pane's existence.
pub fn tab_of_plugin(manifest: &PaneManifest, plugin_id: u32) -> Option<usize> {
    manifest.panes.iter().find_map(|(tab, panes)| {
        panes
            .iter()
            .any(|pane| pane.is_plugin && pane.id == plugin_id)
            .then_some(*tab)
    })
}

/// The first pane a tab lists, in the order the sidebar draws them. `None` for a
/// tab holding nothing but the panes the sidebar leaves out.
pub fn first_pane(manifest: &PaneManifest, tab: usize) -> Option<u32> {
    let panes = manifest.panes.get(&tab)?;
    panes_of(panes).first().map(|pane| pane.id)
}

/// Which tab a terminal pane is in, found by its id. `None` until the manifest
/// has caught up with the pane's existence.
pub fn tab_of_pane(manifest: &PaneManifest, pane_id: u32) -> Option<usize> {
    manifest.panes.iter().find_map(|(tab, panes)| {
        panes
            .iter()
            .any(|pane| !pane.is_plugin && pane.id == pane_id)
            .then_some(*tab)
    })
}

/// Where the user is: the tab they are in, and the pane holding the focus in it.
///
/// Something is always focused, and it is as often a plugin pane as a terminal
/// one - the sidebar itself, most of all. Carrying zellij's own id keeps those
/// two apart, where an id alone could not: zellij numbers plugin panes and
/// terminal panes in separate sequences, so the same number means two panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Focus {
    pub tab: usize,
    pub pane: PaneId,
}

impl Focus {
    /// The focused pane, when it is one the sidebar lists. A plugin pane is not.
    pub fn listed(self) -> Option<u32> {
        match self.pane {
            PaneId::Terminal(id) => Some(id),
            PaneId::Plugin(_) => None,
        }
    }

    /// Whether the focus is on the plugin pane with this id, which is how a
    /// sidebar tells whether a keystroke would reach it.
    pub fn is_plugin(self, plugin_id: u32) -> bool {
        self.pane == PaneId::Plugin(plugin_id)
    }
}

/// The session as the tree needs it: every tab in position order, each carrying
/// the panes reported against it. A tab the manifest says nothing about yet
/// still gets its row, since the two arrive as separate events.
///
/// `focus` overrides what the reported tabs and panes say about themselves,
/// because those flags go stale: closing the focused pane leaves the surviving
/// pane reported as unfocused until something moves the focus again. Passing
/// `None` falls back to them, which is all there is when the focus cannot be
/// resolved.
pub fn session(tabs: &[TabInfo], manifest: &PaneManifest, focus: Option<Focus>) -> Vec<Tab> {
    let mut tabs: Vec<&TabInfo> = tabs.iter().collect();
    tabs.sort_by_key(|tab| tab.position);
    tabs.into_iter()
        .map(|tab| {
            let active = match focus {
                Some(focus) => focus.tab == tab.position,
                None => tab.active,
            };
            let mut panes = manifest
                .panes
                .get(&tab.position)
                .map(|panes| panes_of(panes))
                .unwrap_or_default();
            if let Some(focus) = focus {
                for pane in &mut panes {
                    pane.focused = active && focus.listed() == Some(pane.id);
                }
            }
            Tab {
                position: tab.position,
                name: tab.name.clone(),
                active,
                panes,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pane(id: u32, title: &str, y: usize, x: usize) -> PaneInfo {
        PaneInfo {
            id,
            title: title.to_string(),
            pane_y: y,
            pane_x: x,
            is_selectable: true,
            ..Default::default()
        }
    }

    fn tab(position: usize, name: &str) -> TabInfo {
        TabInfo {
            position,
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn manifest(panes: Vec<(usize, Vec<PaneInfo>)>) -> PaneManifest {
        PaneManifest {
            panes: panes.into_iter().collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn the_ui_panes_are_not_listed() {
        let sidebar = PaneInfo {
            id: 1,
            is_plugin: true,
            is_selectable: true,
            ..Default::default()
        };
        let status_bar = PaneInfo {
            id: 2,
            is_plugin: true,
            ..Default::default()
        };
        let unselectable = PaneInfo {
            id: 3,
            is_selectable: false,
            ..Default::default()
        };
        let suppressed = PaneInfo {
            id: 4,
            is_selectable: true,
            is_suppressed: true,
            ..Default::default()
        };
        let panes = vec![
            sidebar,
            status_bar,
            unselectable,
            suppressed,
            pane(5, "bash", 0, 0),
        ];
        let session = session(&[tab(0, "one")], &manifest(vec![(0, panes)]), None);
        assert_eq!(session[0].panes.len(), 1);
        assert_eq!(session[0].panes[0].id, 5);
    }

    #[test]
    fn panes_are_ordered_down_the_screen_then_across_it() {
        let panes = vec![
            pane(1, "bottom", 10, 0),
            pane(2, "top right", 0, 40),
            pane(3, "top left", 0, 0),
        ];
        let session = session(&[tab(0, "one")], &manifest(vec![(0, panes)]), None);
        let titles: Vec<&str> = session[0]
            .panes
            .iter()
            .map(|pane| pane.title.as_str())
            .collect();
        assert_eq!(titles, vec!["top left", "top right", "bottom"]);
    }

    #[test]
    fn a_floating_pane_sits_after_the_tiled_ones_it_covers() {
        let mut floating = pane(1, "floating", 0, 0);
        floating.is_floating = true;
        let panes = vec![floating, pane(2, "tiled", 20, 0)];
        let session = session(&[tab(0, "one")], &manifest(vec![(0, panes)]), None);
        let titles: Vec<&str> = session[0]
            .panes
            .iter()
            .map(|pane| pane.title.as_str())
            .collect();
        assert_eq!(titles, vec!["tiled", "floating"]);
    }

    #[test]
    fn tabs_come_out_in_position_order_whatever_order_they_arrive_in() {
        let session = session(
            &[tab(2, "third"), tab(0, "first"), tab(1, "second")],
            &manifest(vec![]),
            None,
        );
        let names: Vec<&str> = session.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, vec!["first", "second", "third"]);
    }

    #[test]
    fn a_plugin_pane_is_found_by_its_own_id() {
        let mine = PaneInfo {
            id: 7,
            is_plugin: true,
            ..Default::default()
        };
        // A terminal pane numbered the same is a different pane: zellij counts
        // the two kinds in separate sequences.
        let namesake = PaneInfo {
            id: 7,
            is_plugin: false,
            ..Default::default()
        };
        let manifest = manifest(vec![(0, vec![namesake]), (1, vec![mine])]);
        assert_eq!(tab_of_plugin(&manifest, 7), Some(1));
        assert_eq!(tab_of_plugin(&manifest, 8), None);
    }

    #[test]
    fn a_resolved_focus_overrides_what_the_panes_say_about_themselves() {
        // Closing the focused pane leaves the survivor reported as unfocused,
        // which is exactly when the resolved focus has to win.
        let mut stale = pane(1, "survivor", 0, 0);
        stale.is_focused = false;
        let focus = Focus {
            tab: 0,
            pane: PaneId::Terminal(1),
        };
        let session = session(
            &[tab(0, "one")],
            &manifest(vec![(0, vec![stale])]),
            Some(focus),
        );
        assert!(session[0].active);
        assert!(session[0].panes[0].focused);
    }

    #[test]
    fn a_focused_plugin_pane_leaves_the_tab_active_and_no_pane_focused() {
        let panes = vec![pane(1, "bash", 0, 0)];
        let focus = Focus {
            tab: 0,
            pane: PaneId::Plugin(1),
        };
        let session = session(&[tab(0, "one")], &manifest(vec![(0, panes)]), Some(focus));
        assert!(session[0].active);
        assert!(!session[0].panes[0].focused);
    }

    #[test]
    fn a_pane_of_another_tab_is_never_focused_however_it_reports_itself() {
        let mut claims_focus = pane(1, "elsewhere", 0, 0);
        claims_focus.is_focused = true;
        let focus = Focus {
            tab: 1,
            pane: PaneId::Terminal(9),
        };
        let session = session(
            &[tab(0, "one")],
            &manifest(vec![(0, vec![claims_focus])]),
            Some(focus),
        );
        assert!(!session[0].active);
        assert!(!session[0].panes[0].focused);
    }

    #[test]
    fn the_first_pane_of_a_tab_is_the_first_one_drawn() {
        let ui = PaneInfo {
            id: 1,
            is_plugin: true,
            is_selectable: true,
            ..Default::default()
        };
        let panes = vec![ui, pane(2, "bottom", 10, 0), pane(3, "top", 0, 0)];
        let manifest = manifest(vec![(0, panes), (1, vec![])]);
        assert_eq!(first_pane(&manifest, 0), Some(3));
        // A tab holding nothing the sidebar lists, and a tab it has not been
        // told about, both have no pane to go to.
        assert_eq!(first_pane(&manifest, 1), None);
        assert_eq!(first_pane(&manifest, 9), None);
    }

    #[test]
    fn a_focused_plugin_pane_is_told_from_a_terminal_pane_numbered_the_same() {
        // Zellij counts the two kinds separately, so the number alone says
        // nothing about which pane holds the focus.
        let mine = Focus {
            tab: 0,
            pane: PaneId::Plugin(7),
        };
        let theirs = Focus {
            tab: 0,
            pane: PaneId::Terminal(7),
        };
        assert!(mine.is_plugin(7));
        assert!(!mine.is_plugin(8));
        assert!(!theirs.is_plugin(7));
        assert_eq!(mine.listed(), None);
        assert_eq!(theirs.listed(), Some(7));
    }

    #[test]
    fn a_tab_the_manifest_has_not_reached_yet_still_draws() {
        let session = session(&[tab(0, "one")], &manifest(vec![]), None);
        assert_eq!(session.len(), 1);
        assert!(session[0].panes.is_empty());
    }
}
