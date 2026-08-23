//! Reconciliation of multiplexer reports into the session vocabulary of the
//! tree.

use std::collections::BTreeSet;

use agent_wrangler_ui::model::{PaneId, TabPosition};
use agent_wrangler_ui::tree::{Pane, Tab};

use crate::model::{
    Focus, FocusTarget, PaneReport, PaneVisibility, SessionLayout, TabId, TabReport,
};

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

/// The tab that holds a pane, parked panes included.
///
/// A parked pane is a place that the sidebar can send the user to. The host
/// brings the pane back on screen as it takes the focus.
pub fn tab_position_of_pane(layout: &SessionLayout, pane: &PaneId) -> Option<TabPosition> {
    layout.tabs.iter().find_map(|tab| {
        tab.content_panes
            .iter()
            .any(|candidate| &candidate.id == pane)
            .then_some(tab.position)
    })
}

/// The pane that a tab sends the user to when no row names one.
///
/// A parked pane is never that pane. It answers for an agent that the user
/// asked for by name, and not for a tab that the user asked to enter. Focus on
/// a parked pane also takes the pane in front of it off the screen.
pub fn first_pane_on_screen(layout: &SessionLayout, tab: TabPosition) -> Option<PaneId> {
    layout
        .tabs
        .iter()
        .find(|candidate| candidate.position == tab)?
        .content_panes
        .iter()
        .find(|pane| pane.visibility == PaneVisibility::OnScreen)
        .map(|pane| pane.id.clone())
}

/// Whether a pane is due a row of its own.
///
/// A pane on screen always is. A parked pane is due one only while it hosts an
/// agent: the agent is what the sidebar exists to show, and it keeps running
/// while the host holds its pane off screen.
fn drawn(pane: &PaneReport, agent_panes: &BTreeSet<PaneId>) -> bool {
    pane.visibility == PaneVisibility::OnScreen || agent_panes.contains(&pane.id)
}

pub fn position_of(tabs: &[TabReport], id: &TabId) -> Option<TabPosition> {
    tabs.iter()
        .find(|tab| &tab.id == id)
        .map(|tab| tab.position)
}

/// Tells whether the tab report and the session layout describe the same
/// moment.
///
/// The two reports arrive as separate events. One report can describe a
/// topology that the other did not see. Position is the only key that a pane
/// report offers. A position names a different tab on each side of a tab that
/// opens or closes. A stable id from one report and a pane from the other then
/// make a pair that never existed. Every check of that pair agrees, because
/// both halves of the pair go against the same stale layout.
///
/// Both reports list every tab. A position in one report and not in the other
/// shows that the two cannot join. The position of the user waits for a pair
/// that can join. Rows do not wait. Each row comes from its own report. The
/// next report corrects a row under the wrong tab. The next report cannot
/// correct an effect that ran in the meantime.
///
/// A reorder that leaves the same positions occupied is not visible here. A
/// position-keyed pane report cannot tell one arrangement of the same tabs
/// from another.
fn reports_match_layout(tabs: &[TabReport], layout: &SessionLayout) -> bool {
    tabs.len() == layout.tabs.len()
        && tabs.iter().all(|tab| {
            layout
                .tabs
                .iter()
                .any(|listed| listed.position == tab.position)
        })
}

/// The content pane to remember for the tab that the user is in, or `held`
/// where this focus says nothing new.
///
/// The sidebar hands focus back to this pane when the user leaves the tab. A
/// tab with no sidebar in it changes nothing, because the sidebar never takes
/// focus there. Focus on the sidebar itself changes nothing either, because
/// the pane the user came from is what must come back.
pub fn remembered_content_pane(
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

/// The pane to focus before the user leaves a tab, or `None` where the sidebar
/// must do nothing.
///
/// The sidebar takes focus when it draws, so it must give focus back before the
/// user goes somewhere else. `remembered` is the pane that the user came from.
/// A pane that went off the screen since then is no longer a place to send the
/// user, so the first pane on screen takes its place.
pub fn pane_to_focus_when_leaving(
    layout: &SessionLayout,
    remembered: Option<&PaneId>,
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
    remembered
        .filter(|held| {
            source
                .content_panes
                .iter()
                .any(|pane| &pane.id == *held && pane.visibility == PaneVisibility::OnScreen)
        })
        .cloned()
        .or_else(|| first_pane_on_screen(layout, source.position))
}

pub fn reconcile(
    reports: &[TabReport],
    layout: &SessionLayout,
    visible: bool,
    observed_focus: Option<&Focus>,
    agent_panes: &BTreeSet<PaneId>,
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
                        .filter(|pane| drawn(pane, agent_panes))
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
    if !reports_match_layout(reports, layout) {
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
        if tab_position_of_pane(layout, pane) != Some(focused_tab) {
            return ReconciledFocus::Pending;
        }
    }
    ReconciledFocus::Confirmed(observed.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SessionLayout, SidebarPaneReport, TabLayout};

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
                    content_panes: vec![on_screen("%1", "one")],
                    sidebar_pane: Some(SidebarPaneReport { focused: false }),
                },
                TabLayout {
                    position: TabPosition::at(1),
                    other_focused: false,
                    content_panes: vec![on_screen("%2", "two")],
                    sidebar_pane: None,
                },
            ],
        }
    }

    fn on_screen(id: &str, title: &str) -> PaneReport {
        PaneReport {
            id: PaneId::new(id),
            title: title.to_string(),
            focused: false,
            visibility: PaneVisibility::OnScreen,
        }
    }

    fn parked(id: &str, title: &str) -> PaneReport {
        PaneReport {
            visibility: PaneVisibility::Parked,
            ..on_screen(id, title)
        }
    }

    fn observed(reports: &[TabReport], layout: &SessionLayout, focus: &Focus) -> ReconciledSession {
        reconcile(reports, layout, true, Some(focus), &BTreeSet::new())
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
            reconcile(&reports, &layout(), false, Some(&focus), &BTreeSet::new()).focus,
            ReconciledFocus::Unknown
        );
        assert_eq!(
            reconcile(&reports, &layout(), true, None, &BTreeSet::new()).focus,
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
        // A tab closed and the pane report did not catch up. A tab opened and
        // the pane report did not catch up. The number of tabs agrees, but the
        // positions do not. Every check of such a pair passes on its own. The
        // report holds the tab, and the layout at the position of that tab
        // holds a sidebar.
        for reports in [
            vec![tab("mine", 0)],
            vec![tab("mine", 0), tab("second", 1), tab("third", 2)],
            vec![tab("mine", 0), tab("second", 2)],
        ] {
            assert!(!reports_match_layout(&reports, &layout()));
            let resolved = observed(&reports, &layout(), &focus);
            assert_eq!(resolved.focus, ReconciledFocus::Pending);
            // The tabs stay in the list, because each row comes from the
            // reports and not from the position of the user. Only the gutter
            // waits, and it waits only while the two reports disagree.
            assert_eq!(resolved.tabs.len(), reports.len());
            assert!(resolved.tabs.iter().all(|tab| !tab.active));
        }
    }

    fn with_a_parked_pane() -> SessionLayout {
        let mut layout = layout();
        layout.tabs[1].content_panes = vec![parked("%2", "two"), on_screen("%3", "three")];
        layout
    }

    #[test]
    fn a_parked_pane_has_a_row_only_while_an_agent_answers_for_it() {
        let reports = vec![tab("0", 0), tab("1", 1)];
        let layout = with_a_parked_pane();

        let alone = reconcile(&reports, &layout, false, None, &BTreeSet::new());
        let ids: Vec<&str> = alone.tabs[1]
            .panes
            .iter()
            .map(|pane| pane.id.as_str())
            .collect();
        assert_eq!(ids, ["%3"]);

        let hosting = BTreeSet::from([PaneId::new("%2")]);
        let hosted = reconcile(&reports, &layout, false, None, &hosting);
        let ids: Vec<&str> = hosted.tabs[1]
            .panes
            .iter()
            .map(|pane| pane.id.as_str())
            .collect();
        assert_eq!(ids, ["%2", "%3"]);
    }

    #[test]
    fn a_parked_pane_is_a_place_to_go_but_never_the_place_a_tab_goes() {
        let layout = with_a_parked_pane();
        let parked = PaneId::new("%2");
        // An agent row names its pane, and the host takes the pane back on
        // screen as it takes the focus.
        assert_eq!(
            tab_position_of_pane(&layout, &parked),
            Some(TabPosition::at(1))
        );
        // A tab row names no pane, so it goes to one the user can already see.
        assert_eq!(
            first_pane_on_screen(&layout, TabPosition::at(1)),
            Some(PaneId::new("%3"))
        );
    }

    #[test]
    fn stand_down_falls_back_when_the_remembered_pane_is_parked() {
        let mut layout = with_a_parked_pane();
        layout.tabs[1].sidebar_pane = Some(SidebarPaneReport { focused: false });
        assert_eq!(
            pane_to_focus_when_leaving(
                &layout,
                Some(&PaneId::new("%2")),
                TabPosition::at(1),
                TabPosition::at(0),
            ),
            Some(PaneId::new("%3"))
        );
    }

    #[test]
    fn pane_ids_are_opaque_when_finding_and_leaving_tabs() {
        let layout = layout();
        let id = PaneId::new("%2");
        assert_eq!(tab_position_of_pane(&layout, &id), Some(TabPosition::at(1)));
        assert_eq!(first_pane_on_screen(&layout, TabPosition::at(1)), Some(id));
        assert_eq!(
            pane_to_focus_when_leaving(
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
                pane_to_focus_when_leaving(
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
            remembered_content_pane(&tabs, &layout(), &elsewhere, held.clone()),
            held
        );
        let here = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Content(PaneId::new("%1")),
        };
        assert_eq!(
            remembered_content_pane(&tabs, &layout(), &here, None),
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
            &BTreeSet::new(),
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
        let resolved = reconcile(&reports, &layout, true, None, &BTreeSet::new());
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
            pane_to_focus_when_leaving(
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
            pane_to_focus_when_leaving(&layout, None, TabPosition::at(0), TabPosition::at(1),),
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
            remembered_content_pane(&reports, &layout, &focus, None),
            Some(PaneId::new("%2"))
        );
        assert_eq!(
            pane_to_focus_when_leaving(&layout, None, TabPosition::at(1), TabPosition::at(0),),
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
            remembered_content_pane(&reports, &layout(), &focus, held.clone()),
            held
        );
    }
}
