//! Keep authoritative agent records while hiding records on known sidebar panes.

use std::collections::{BTreeMap, BTreeSet};

use agent_wrangler_core::registry::Registry;
use agent_wrangler_sidebar::AgentSnapshot;

use crate::topology::ReportedPane;

pub(crate) struct SidebarAgents {
    own_pane: String,
    excluded: BTreeSet<String>,
    snapshot: Option<AgentSnapshot>,
}

impl SidebarAgents {
    pub fn new(own_pane: &str) -> Self {
        Self {
            own_pane: own_pane.to_string(),
            excluded: BTreeSet::from([own_pane.to_string()]),
            snapshot: None,
        }
    }

    pub fn receive(&mut self, snapshot: AgentSnapshot) -> AgentSnapshot {
        self.snapshot = Some(snapshot);
        self.project().expect("the received snapshot is present")
    }

    /// A pane can return to content without another publication from the daemon.
    pub fn update_panes(&mut self, panes: &[ReportedPane]) -> Option<AgentSnapshot> {
        let excluded = panes
            .iter()
            .filter(|pane| pane.is_sidebar)
            .map(|pane| pane.id.clone())
            .chain(std::iter::once(self.own_pane.clone()))
            .collect();
        if self.excluded == excluded {
            return None;
        }
        self.excluded = excluded;
        self.project()
    }

    fn project(&self) -> Option<AgentSnapshot> {
        let snapshot = self.snapshot.as_ref()?;
        let AgentSnapshot::Compatible { registry, panes } = snapshot else {
            return Some(AgentSnapshot::Incompatible);
        };
        let mut visible_registry = Registry::default();
        let mut visible_panes = BTreeMap::new();
        for agent in registry.iter() {
            let pane = panes.get(&agent.session);
            if pane.is_some_and(|pane| self.excluded.contains(pane.as_str())) {
                continue;
            }
            visible_registry.report(agent.clone());
            if let Some(pane) = pane {
                visible_panes.insert(agent.session.clone(), pane.clone());
            }
        }
        Some(AgentSnapshot::Compatible {
            registry: visible_registry,
            panes: visible_panes,
        })
    }
}
