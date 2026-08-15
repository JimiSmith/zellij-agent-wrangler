//! Reconcile multiplexer reports into the tree's session vocabulary.

use agent_wrangler_ui::model::{PaneId, TabPosition};
use agent_wrangler_ui::tree::{Pane, Tab};

use crate::model::{Focus, FocusTarget, SessionLayout, TabId, TabReport};

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

pub fn tab_of_pane(layout: &SessionLayout, pane: &PaneId) -> Option<TabPosition> {
    layout.tabs.iter().find_map(|tab| {
        tab.content_panes
            .iter()
            .any(|candidate| &candidate.id == pane)
            .then_some(tab.position)
    })
}

pub fn first_pane(layout: &SessionLayout, tab: TabPosition) -> Option<PaneId> {
    layout
        .tabs
        .iter()
        .find(|candidate| candidate.position == tab)?
        .content_panes
        .first()
        .map(|pane| pane.id.clone())
}

pub fn position_of(tabs: &[TabReport], id: &TabId) -> Option<TabPosition> {
    tabs.iter()
        .find(|tab| &tab.id == id)
        .map(|tab| tab.position)
}

/// Whether the tab report and the session layout describe the same moment.
///
/// The two arrive as separate events, so one can already describe a topology
/// the other has not seen. Position is the only key a pane report offers, and a
/// position names a different tab either side of a tab opening or closing:
/// joining a stable id from one report with a pane from the other then produces
/// a pair that never existed, and every validation of it agrees, since both
/// halves of the pair are checked against the same stale layout.
///
/// Every tab is listed in both reports, so a position in one and not the other
/// says the two cannot be joined, and where the user is waits for a pair that
/// can be. Rows do not wait: each is drawn from what its own report holds, so a
/// row under the wrong tab is corrected by the report that follows, where an
/// effect taken meanwhile would not be.
///
/// A reordering that leaves the same positions occupied is not visible here.
/// Nothing in a position-keyed pane report tells one arrangement of the same
/// tabs from another.
pub fn coherent(tabs: &[TabReport], layout: &SessionLayout) -> bool {
    tabs.len() == layout.tabs.len()
        && tabs.iter().all(|tab| {
            layout
                .tabs
                .iter()
                .any(|listed| listed.position == tab.position)
        })
}

pub fn left_behind_by(
    tabs: &[TabReport],
    layout: &SessionLayout,
    now: &Focus,
    held: Option<PaneId>,
) -> Option<PaneId> {
    let Some(current) = position_of(tabs, &now.tab) else {
        return held;
    };
    if !layout
        .tabs
        .iter()
        .any(|tab| tab.position == current && tab.sidebar_pane.is_some())
    {
        return held;
    }
    match &now.target {
        FocusTarget::Content(pane) => Some(pane.clone()),
        FocusTarget::Sidebar | FocusTarget::Other => held,
    }
}

pub fn stand_down_to(
    layout: &SessionLayout,
    left_behind: Option<&PaneId>,
    leaving_from: TabPosition,
    going_to: TabPosition,
) -> Option<PaneId> {
    let source = layout
        .tabs
        .iter()
        .find(|tab| tab.position == leaving_from && tab.sidebar_pane.is_some())?;
    if source.position == going_to {
        return None;
    }
    left_behind
        .filter(|remembered| {
            source
                .content_panes
                .iter()
                .any(|pane| &pane.id == *remembered)
        })
        .cloned()
        .or_else(|| first_pane(layout, source.position))
}

pub fn reconcile(
    reports: &[TabReport],
    layout: &SessionLayout,
    visible: bool,
    observed_focus: Option<&Focus>,
) -> ReconciledSession {
    let focus = reconcile_focus(reports, layout, visible, observed_focus);
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
            let listed = layout
                .tabs
                .iter()
                .find(|tab| tab.position == report.position)
                .map(|tab| {
                    tab.content_panes
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
    layout: &SessionLayout,
    visible: bool,
    observed: Option<&Focus>,
) -> ReconciledFocus {
    if !visible || observed.is_none() {
        return ReconciledFocus::Unknown;
    }
    if !coherent(reports, layout) {
        return ReconciledFocus::Pending;
    }
    let observed = observed.expect("checked above");
    let Some(focused_tab) = position_of(reports, &observed.tab) else {
        return ReconciledFocus::Pending;
    };
    if !layout
        .tabs
        .iter()
        .any(|tab| tab.position == focused_tab && tab.sidebar_pane.is_some())
    {
        return ReconciledFocus::Pending;
    }
    if let FocusTarget::Content(pane) = &observed.target {
        if tab_of_pane(layout, pane) != Some(focused_tab) {
            return ReconciledFocus::Pending;
        }
    }
    ReconciledFocus::Confirmed(observed.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PaneReport, SessionLayout, SidebarPaneReport, TabLayout};

    fn tab(id: &str, position: usize) -> TabReport {
        TabReport {
            id: TabId::new(id),
            position: TabPosition::at(position),
            name: id.to_string(),
            active: false,
        }
    }

    fn layout() -> SessionLayout {
        SessionLayout {
            tabs: vec![
                TabLayout {
                    position: TabPosition::at(0),
                    other_focused: false,
                    content_panes: vec![PaneReport {
                        id: PaneId::new("%1"),
                        title: "one".to_string(),
                        focused: false,
                    }],
                    sidebar_pane: Some(SidebarPaneReport { focused: false }),
                },
                TabLayout {
                    position: TabPosition::at(1),
                    other_focused: false,
                    content_panes: vec![PaneReport {
                        id: PaneId::new("%2"),
                        title: "two".to_string(),
                        focused: false,
                    }],
                    sidebar_pane: None,
                },
            ],
        }
    }

    fn observed(reports: &[TabReport], layout: &SessionLayout, focus: &Focus) -> ReconciledSession {
        reconcile(reports, layout, true, Some(focus))
    }

    #[test]
    fn focus_uses_a_stable_tab_id_after_positions_change() {
        let tabs = vec![tab("0", 0), tab("2", 1)];
        let mut layout = layout();
        layout.tabs[0].sidebar_pane = None;
        layout.tabs[1].sidebar_pane = Some(SidebarPaneReport { focused: false });
        let focus = Focus {
            tab: TabId::new("2"),
            target: FocusTarget::Content(PaneId::new("%2")),
        };
        let snapshot = observed(&tabs, &layout, &focus);
        assert_eq!(snapshot.focus, ReconciledFocus::Confirmed(focus));
        assert!(!snapshot.tabs[0].active);
        assert!(snapshot.tabs[1].active);
        assert!(snapshot.tabs[1].panes[0].focused);
    }

    #[test]
    fn a_hidden_or_missing_focus_is_conservative_however_it_is_read() {
        let reports = vec![tab("mine", 0), tab("other", 1)];
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Sidebar,
        };
        assert_eq!(
            reconcile(&reports, &layout(), false, Some(&focus)).focus,
            ReconciledFocus::Unknown
        );
        assert_eq!(
            reconcile(&reports, &layout(), true, None).focus,
            ReconciledFocus::Unknown
        );
    }

    #[test]
    fn other_plugin_focus_is_confirmed_from_the_tab_and_sidebar_relationship() {
        let reports = vec![tab("mine", 0), tab("other", 1)];
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Other,
        };
        assert_eq!(
            observed(&reports, &layout(), &focus).focus,
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
                observed(&reports, &layout(), &focus).focus,
                ReconciledFocus::Pending
            );
        }
        let mut missing_sidebar = layout();
        missing_sidebar.tabs[0].sidebar_pane = None;
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
    fn reports_of_different_topologies_are_not_joined() {
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Sidebar,
        };
        // A tab has closed and the pane report has not caught up; a tab has
        // opened and it has not caught up; as many tabs are reported as are
        // listed but not at the same positions. Every check of such a pair
        // passes on its own: the tab is reported, and the layout at its
        // position holds a sidebar.
        for reports in [
            vec![tab("mine", 0)],
            vec![tab("mine", 0), tab("second", 1), tab("third", 2)],
            vec![tab("mine", 0), tab("second", 2)],
        ] {
            assert!(!coherent(&reports, &layout()));
            let resolved = observed(&reports, &layout(), &focus);
            assert_eq!(resolved.focus, ReconciledFocus::Pending);
            // The tabs are still listed, since a row is drawn from the reports
            // rather than from where the user is. It is only the gutter that
            // waits, and only for as long as the two disagree.
            assert_eq!(resolved.tabs.len(), reports.len());
            assert!(resolved.tabs.iter().all(|tab| !tab.active));
        }
    }

    #[test]
    fn pane_ids_are_opaque_when_finding_and_leaving_tabs() {
        let layout = layout();
        let id = PaneId::new("%2");
        assert_eq!(tab_of_pane(&layout, &id), Some(TabPosition::at(1)));
        assert_eq!(first_pane(&layout, TabPosition::at(1)), Some(id));
        assert_eq!(
            stand_down_to(
                &layout,
                Some(&PaneId::new("%1")),
                TabPosition::at(0),
                TabPosition::at(1),
            ),
            Some(PaneId::new("%1"))
        );
    }

    #[test]
    fn stand_down_falls_back_when_the_remembered_pane_closed_or_moved() {
        let layout = layout();

        for stale in ["%gone", "%2"] {
            assert_eq!(
                stand_down_to(
                    &layout,
                    Some(&PaneId::new(stale)),
                    TabPosition::at(0),
                    TabPosition::at(1),
                ),
                Some(PaneId::new("%1"))
            );
        }
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
            left_behind_by(&tabs, &layout(), &elsewhere, held.clone()),
            held
        );
        let here = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Content(PaneId::new("%1")),
        };
        assert_eq!(
            left_behind_by(&tabs, &layout(), &here, None),
            Some(PaneId::new("%1"))
        );
    }

    #[test]
    fn tabs_are_sorted_by_position_independently_of_report_order() {
        let resolved = reconcile(
            &[tab("third", 2), tab("first", 0), tab("second", 1)],
            &SessionLayout::default(),
            false,
            None,
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
        let mut layout = layout();
        layout.tabs[1].content_panes[0].focused = true;
        let resolved = reconcile(&reports, &layout, true, None);
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
        let unresolved = observed(&reports, &layout, &unknown);
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
        let resolved = observed(&reports, &layout(), &focus);
        assert_eq!(resolved.focus, ReconciledFocus::Confirmed(focus));
        assert!(resolved.tabs[0].active);
        assert!(resolved.tabs[0].panes.iter().all(|pane| !pane.focused));
        assert!(!resolved.tabs[1].active);
    }

    #[test]
    fn leaving_for_the_same_tab_stands_down_nowhere() {
        let layout = layout();
        assert_eq!(
            stand_down_to(
                &layout,
                Some(&PaneId::new("%1")),
                TabPosition::at(0),
                TabPosition::at(0),
            ),
            None
        );
    }

    #[test]
    fn leaving_without_a_remembered_pane_uses_the_first_content_pane() {
        let layout = layout();
        assert_eq!(
            stand_down_to(&layout, None, TabPosition::at(0), TabPosition::at(1),),
            Some(PaneId::new("%1"))
        );
    }

    #[test]
    fn each_sidebar_is_resolved_against_its_own_tab() {
        let reports = vec![tab("first", 0), tab("second", 1)];
        let mut layout = layout();
        layout.tabs[1].sidebar_pane = Some(SidebarPaneReport { focused: false });
        let focus = Focus {
            tab: TabId::new("second"),
            target: FocusTarget::Content(PaneId::new("%2")),
        };

        assert_eq!(
            observed(&reports, &layout, &focus).focus,
            ReconciledFocus::Confirmed(focus.clone())
        );
        assert_eq!(
            left_behind_by(&reports, &layout, &focus, None),
            Some(PaneId::new("%2"))
        );
        assert_eq!(
            stand_down_to(&layout, None, TabPosition::at(1), TabPosition::at(0),),
            Some(PaneId::new("%2"))
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
            left_behind_by(&reports, &layout(), &focus, held.clone()),
            held
        );
    }
}
