//! Reconcile multiplexer reports into the tree's session vocabulary.

use agent_wrangler_ui::model::{PaneId, TabPosition};
use agent_wrangler_ui::tree::{Pane, Tab};

use crate::model::{Focus, FocusTarget, PaneSnapshot, TabId, TabReport};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconciledFocus {
    Confirmed(Focus),
    Pending,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciledSession {
    pub tabs: Vec<Tab>,
    pub focus: ReconciledFocus,
}

pub fn tab_of_pane(snapshot: &PaneSnapshot, pane: &PaneId) -> Option<TabPosition> {
    snapshot.tabs.iter().find_map(|tab| {
        tab.panes
            .iter()
            .any(|candidate| &candidate.id == pane)
            .then_some(tab.position)
    })
}

pub fn first_pane(snapshot: &PaneSnapshot, tab: TabPosition) -> Option<PaneId> {
    snapshot
        .tabs
        .iter()
        .find(|candidate| candidate.position == tab)?
        .panes
        .first()
        .map(|pane| pane.id.clone())
}

pub fn position_of(tabs: &[TabReport], id: &TabId) -> Option<TabPosition> {
    tabs.iter()
        .find(|tab| &tab.id == id)
        .map(|tab| tab.position)
}

pub fn left_behind_by(
    tabs: &[TabReport],
    panes: &PaneSnapshot,
    now: &Focus,
    held: Option<PaneId>,
) -> Option<PaneId> {
    let Some(mine) = panes.sidebar_tab else {
        return held;
    };
    if position_of(tabs, &now.tab) != Some(mine) {
        return held;
    }
    match &now.target {
        FocusTarget::Content(pane) => Some(pane.clone()),
        FocusTarget::Sidebar | FocusTarget::Other => held,
    }
}

pub fn stand_down_to(
    panes: &PaneSnapshot,
    left_behind: Option<&PaneId>,
    going_to: TabPosition,
) -> Option<PaneId> {
    let mine = panes.sidebar_tab?;
    if mine == going_to {
        return None;
    }
    left_behind.cloned().or_else(|| first_pane(panes, mine))
}

pub fn reconcile(
    reports: &[TabReport],
    panes: &PaneSnapshot,
    visible: bool,
    observed_focus: Option<&Focus>,
    focus_refresh_pending: bool,
) -> ReconciledSession {
    let focus = reconcile_focus(
        reports,
        panes,
        visible,
        observed_focus,
        focus_refresh_pending,
    );
    let confirmed = match &focus {
        ReconciledFocus::Confirmed(focus) => Some(focus),
        ReconciledFocus::Pending | ReconciledFocus::Unknown => None,
    };
    let here = confirmed.and_then(|focus| position_of(reports, &focus.tab));
    let on = confirmed.and_then(|focus| match &focus.target {
        FocusTarget::Content(pane) => Some(pane),
        FocusTarget::Sidebar | FocusTarget::Other => None,
    });
    let mut reports: Vec<&TabReport> = reports.iter().collect();
    reports.sort_by_key(|tab| tab.position.zero_based());
    let tabs = reports
        .into_iter()
        .map(|report| {
            let active = here == Some(report.position);
            let listed = panes
                .tabs
                .iter()
                .find(|tab| tab.position == report.position)
                .map(|tab| {
                    tab.panes
                        .iter()
                        .map(|pane| {
                            Pane::new(pane.id.clone(), &pane.title, active && on == Some(&pane.id))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Tab {
                id: report.id.clone(),
                position: report.position,
                name: report.name.clone(),
                active,
                panes: listed,
            }
        })
        .collect();
    ReconciledSession { tabs, focus }
}

fn reconcile_focus(
    reports: &[TabReport],
    panes: &PaneSnapshot,
    visible: bool,
    observed: Option<&Focus>,
    pending: bool,
) -> ReconciledFocus {
    if !visible || observed.is_none() {
        return ReconciledFocus::Unknown;
    }
    if pending {
        return ReconciledFocus::Pending;
    }
    let observed = observed.expect("checked above");
    let Some(focused_tab) = position_of(reports, &observed.tab) else {
        return ReconciledFocus::Pending;
    };
    if panes.sidebar_tab != Some(focused_tab) {
        return ReconciledFocus::Pending;
    }
    if let FocusTarget::Content(pane) = &observed.target {
        if tab_of_pane(panes, pane) != Some(focused_tab) {
            return ReconciledFocus::Pending;
        }
    }
    ReconciledFocus::Confirmed(observed.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PaneReport, TabPanes};

    fn tab(id: &str, position: usize) -> TabReport {
        TabReport {
            id: TabId::new(id),
            position: TabPosition::at(position),
            name: id.to_string(),
            active: false,
        }
    }

    fn panes() -> PaneSnapshot {
        PaneSnapshot {
            tabs: vec![
                TabPanes {
                    position: TabPosition::at(0),
                    panes: vec![PaneReport {
                        id: PaneId::new("%1"),
                        title: "one".to_string(),
                        focused: false,
                    }],
                },
                TabPanes {
                    position: TabPosition::at(1),
                    panes: vec![PaneReport {
                        id: PaneId::new("%2"),
                        title: "two".to_string(),
                        focused: false,
                    }],
                },
            ],
            sidebar_tab: Some(TabPosition::at(0)),
        }
    }

    fn observed(reports: &[TabReport], panes: &PaneSnapshot, focus: &Focus) -> ReconciledSession {
        reconcile(reports, panes, true, Some(focus), false)
    }

    #[test]
    fn focus_uses_a_stable_tab_id_after_positions_change() {
        let tabs = vec![tab("0", 0), tab("2", 1)];
        let mut panes = panes();
        panes.sidebar_tab = Some(TabPosition::at(1));
        let focus = Focus {
            tab: TabId::new("2"),
            target: FocusTarget::Content(PaneId::new("%2")),
        };
        let snapshot = observed(&tabs, &panes, &focus);
        assert_eq!(snapshot.focus, ReconciledFocus::Confirmed(focus));
        assert!(!snapshot.tabs[0].active);
        assert!(snapshot.tabs[1].active);
        assert!(snapshot.tabs[1].panes[0].focused);
    }

    #[test]
    fn hidden_missing_and_refreshing_focus_are_conservative() {
        let reports = vec![tab("mine", 0), tab("other", 1)];
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Sidebar,
        };
        assert_eq!(
            reconcile(&reports, &panes(), false, Some(&focus), false).focus,
            ReconciledFocus::Unknown
        );
        assert_eq!(
            reconcile(&reports, &panes(), true, None, false).focus,
            ReconciledFocus::Unknown
        );
        let pending = reconcile(&reports, &panes(), true, Some(&focus), true);
        assert_eq!(pending.focus, ReconciledFocus::Pending);
        assert!(pending.tabs.iter().all(|tab| !tab.active));
    }

    #[test]
    fn other_plugin_focus_is_confirmed_from_the_tab_and_sidebar_relationship() {
        let reports = vec![tab("mine", 0)];
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Other,
        };
        assert_eq!(
            observed(&reports, &panes(), &focus).focus,
            ReconciledFocus::Confirmed(focus)
        );
    }

    #[test]
    fn contradictory_topology_keeps_focus_pending() {
        let reports = vec![tab("mine", 0), tab("other", 1)];
        for focus in [
            Focus {
                tab: TabId::new("mine"),
                target: FocusTarget::Content(PaneId::new("%2")),
            },
            Focus {
                tab: TabId::new("other"),
                target: FocusTarget::Other,
            },
        ] {
            assert_eq!(
                observed(&reports, &panes(), &focus).focus,
                ReconciledFocus::Pending
            );
        }
        let mut missing_sidebar = panes();
        missing_sidebar.sidebar_tab = None;
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Sidebar,
        };
        assert_eq!(
            observed(&reports, &missing_sidebar, &focus).focus,
            ReconciledFocus::Pending
        );
    }

    #[test]
    fn pane_ids_are_opaque_when_finding_and_leaving_tabs() {
        let panes = panes();
        let id = PaneId::new("%2");
        assert_eq!(tab_of_pane(&panes, &id), Some(TabPosition::at(1)));
        assert_eq!(first_pane(&panes, TabPosition::at(1)), Some(id));
        assert_eq!(
            stand_down_to(&panes, Some(&PaneId::new("%held")), TabPosition::at(1)),
            Some(PaneId::new("%held"))
        );
    }

    #[test]
    fn only_content_focus_in_the_sidebars_tab_is_left_behind() {
        let tabs = vec![tab("mine", 0), tab("other", 1)];
        let held = Some(PaneId::new("%held"));
        let elsewhere = Focus {
            tab: TabId::new("other"),
            target: FocusTarget::Content(PaneId::new("%2")),
        };
        assert_eq!(
            left_behind_by(&tabs, &panes(), &elsewhere, held.clone()),
            held
        );
        let here = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Content(PaneId::new("%1")),
        };
        assert_eq!(
            left_behind_by(&tabs, &panes(), &here, None),
            Some(PaneId::new("%1"))
        );
    }

    #[test]
    fn tabs_are_sorted_by_position_independently_of_report_order() {
        let resolved = reconcile(
            &[tab("third", 2), tab("first", 0), tab("second", 1)],
            &PaneSnapshot::default(),
            false,
            None,
            false,
        )
        .tabs;
        let names: Vec<&str> = resolved.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, ["first", "second", "third"]);
        let ids: Vec<&str> = resolved.iter().map(|tab| tab.id.as_str()).collect();
        assert_eq!(ids, ["first", "second", "third"]);
    }

    #[test]
    fn reported_focus_flags_are_never_a_presentation_fallback() {
        let mut reports = vec![tab("mine", 0), tab("other", 1)];
        reports[1].active = true;
        let mut panes = panes();
        panes.tabs[1].panes[0].focused = true;
        let resolved = reconcile(&reports, &panes, true, None, false);
        assert_eq!(resolved.focus, ReconciledFocus::Unknown);
        assert!(resolved.tabs.iter().all(|tab| !tab.active));
        assert!(resolved
            .tabs
            .iter()
            .flat_map(|tab| &tab.panes)
            .all(|pane| !pane.focused));

        let unknown = Focus {
            tab: TabId::new("missing"),
            target: FocusTarget::Content(PaneId::new("%1")),
        };
        let unresolved = observed(&reports, &panes, &unknown);
        assert_eq!(unresolved.focus, ReconciledFocus::Pending);
        assert!(unresolved.tabs.iter().all(|tab| !tab.active));
    }

    #[test]
    fn sidebar_focus_places_the_tab_but_no_content_pane() {
        let reports = vec![tab("mine", 0), tab("other", 1)];
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Sidebar,
        };
        let resolved = observed(&reports, &panes(), &focus);
        assert_eq!(resolved.focus, ReconciledFocus::Confirmed(focus));
        assert!(resolved.tabs[0].active);
        assert!(resolved.tabs[0].panes.iter().all(|pane| !pane.focused));
        assert!(!resolved.tabs[1].active);
    }

    #[test]
    fn leaving_for_the_same_tab_stands_down_nowhere() {
        let panes = panes();
        assert_eq!(
            stand_down_to(&panes, Some(&PaneId::new("%1")), TabPosition::at(0)),
            None
        );
    }

    #[test]
    fn leaving_without_a_remembered_pane_uses_the_first_content_pane() {
        let panes = panes();
        assert_eq!(
            stand_down_to(&panes, None, TabPosition::at(1)),
            Some(PaneId::new("%1"))
        );
    }

    #[test]
    fn focusing_the_sidebar_does_not_replace_the_remembered_content_pane() {
        let reports = vec![tab("mine", 0)];
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Sidebar,
        };
        let held = Some(PaneId::new("%held"));
        assert_eq!(
            left_behind_by(&reports, &panes(), &focus, held.clone()),
            held
        );
    }
}
