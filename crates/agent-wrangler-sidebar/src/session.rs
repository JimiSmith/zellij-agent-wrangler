//! Reconcile multiplexer reports into the tree's session vocabulary.

use agent_wrangler_ui::model::{PaneId, TabPosition};
use agent_wrangler_ui::tree::{Pane, Tab};

use crate::model::{Focus, FocusTarget, PaneSnapshot, TabId, TabReport};

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

pub fn session(reports: &[TabReport], panes: &PaneSnapshot, focus: Option<&Focus>) -> Vec<Tab> {
    let here = focus.and_then(|focus| position_of(reports, &focus.tab));
    let on = focus.and_then(|focus| match &focus.target {
        FocusTarget::Content(pane) => Some(pane),
        FocusTarget::Sidebar | FocusTarget::Other => None,
    });
    let mut reports: Vec<&TabReport> = reports.iter().collect();
    reports.sort_by_key(|tab| tab.position.zero_based());
    reports
        .into_iter()
        .map(|report| {
            let active = here.map_or(report.active, |position| position == report.position);
            let mut listed = panes
                .tabs
                .iter()
                .find(|tab| tab.position == report.position)
                .map(|tab| {
                    tab.panes
                        .iter()
                        .map(|pane| Pane::new(pane.id.clone(), &pane.title, pane.focused))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if here.is_some() {
                for pane in &mut listed {
                    pane.focused = active && on == Some(&pane.id);
                }
            }
            Tab {
                id: report.id.clone(),
                position: report.position,
                name: report.name.clone(),
                active,
                panes: listed,
            }
        })
        .collect()
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

    #[test]
    fn focus_uses_a_stable_tab_id_after_positions_change() {
        let tabs = vec![tab("0", 0), tab("2", 1)];
        let focus = Focus {
            tab: TabId::new("2"),
            target: FocusTarget::Content(PaneId::new("%2")),
        };
        let resolved = session(&tabs, &panes(), Some(&focus));
        assert!(!resolved[0].active);
        assert!(resolved[1].active);
        assert!(resolved[1].panes[0].focused);
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
        let resolved = session(
            &[tab("third", 2), tab("first", 0), tab("second", 1)],
            &PaneSnapshot::default(),
            None,
        );
        let names: Vec<&str> = resolved.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, ["first", "second", "third"]);
        let ids: Vec<&str> = resolved.iter().map(|tab| tab.id.as_str()).collect();
        assert_eq!(ids, ["first", "second", "third"]);
    }

    #[test]
    fn reported_focus_flags_are_the_fallback_without_a_placeable_observation() {
        let mut reports = vec![tab("mine", 0), tab("other", 1)];
        reports[1].active = true;
        let mut panes = panes();
        panes.tabs[1].panes[0].focused = true;
        let resolved = session(&reports, &panes, None);
        assert!(!resolved[0].active);
        assert!(resolved[1].active);
        assert!(resolved[1].panes[0].focused);

        let unknown = Focus {
            tab: TabId::new("missing"),
            target: FocusTarget::Content(PaneId::new("%1")),
        };
        assert_eq!(session(&reports, &panes, Some(&unknown)), resolved);
    }

    #[test]
    fn sidebar_focus_places_the_tab_but_no_content_pane() {
        let reports = vec![tab("mine", 0), tab("other", 1)];
        let focus = Focus {
            tab: TabId::new("mine"),
            target: FocusTarget::Sidebar,
        };
        let resolved = session(&reports, &panes(), Some(&focus));
        assert!(resolved[0].active);
        assert!(resolved[0].panes.iter().all(|pane| !pane.focused));
        assert!(!resolved[1].active);
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
