//! The state of the sidebar must not grow with the duration of a session.
//!
//! The plugin lives in wasm linear memory, which only grows. The plugin gives
//! nothing back to the host. Data that the application keeps for each event
//! becomes memory that the pane never releases. This test drives `Application`
//! through the burst of inputs of a real session. It compares the live heap
//! early against the live heap much later. A steady state is the only
//! acceptable result. A reading that climbs with the step count shows state
//! that accumulates for each call.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent_wrangler_core::agent::{self, Agent, LabelFacts, Record, SessionId, Turn};
use agent_wrangler_core::origin::Origin;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_sidebar::{
    AgentSnapshot, Application, Focus, FocusTarget, Input, Options, PaneId, PaneReport,
    PaneVisibility, Permission, SessionLayout, SidebarPaneReport, TabId, TabLayout, TabReport,
};
use agent_wrangler_ui::model::TabPosition;
use agent_wrangler_ui::{ansi, Rect};

static LIVE: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, with a count of the bytes that are in use.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let fresh = System.realloc(ptr, layout, new_size);
        if !fresh.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            LIVE.fetch_add(new_size, Ordering::Relaxed);
        }
        fresh
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SESSION: &str = "main";
const TABS: usize = 10;
const PANES_PER_TAB: usize = 3;
const AGENTS: usize = 6;

/// The points of the two readings, and the drift that is permitted between
/// them.
///
/// The first reading comes late enough for every one-time allocation to
/// complete. Only the steps between the two checkpoints separate the readings.
/// A sidebar that stands still costs tens of bytes across this number of
/// steps. State that the sidebar holds for each attention call costs hundreds
/// of kilobytes. That gap makes room for a tolerance that is loose enough to
/// avoid a flaky test and still decisive.
const SETTLE: usize = 400;
const STEPS: usize = 4_000;
const TOLERANCE: usize = 8 * 1024;

fn area() -> Rect {
    Rect::new(0, 0, 32, 50)
}

fn pane_id(tab: usize, pane: usize) -> String {
    (tab * 10 + pane).to_string()
}

fn tabs(step: usize) -> Vec<TabReport> {
    (0..TABS)
        .map(|tab| TabReport {
            id: TabId::new(tab.to_string()),
            position: TabPosition::at(tab),
            displayed_index: (tab + 1).to_string(),
            // Tab names change with renames by the user and with renumbers by
            // zellij.
            name: format!("tab-{tab}-{}", step % 7),
            active: tab == step % TABS,
        })
        .collect()
}

fn layout(step: usize) -> SessionLayout {
    SessionLayout {
        tabs: (0..TABS)
            .map(|tab| TabLayout {
                position: TabPosition::at(tab),
                other_focused: false,
                content_panes: (0..PANES_PER_TAB)
                    .map(|pane| PaneReport {
                        id: PaneId::new(pane_id(tab, pane)),
                        // A shell prompt retitles its pane again and again.
                        // Each title is a new string for the sidebar.
                        title: format!("~/work/repo-{tab}-{pane} $ step {step}"),
                        focused: tab == step % TABS && pane == step % PANES_PER_TAB,
                        visibility: PaneVisibility::OnScreen,
                    })
                    .collect(),
                sidebar_pane: Some(SidebarPaneReport { focused: false }),
            })
            .collect(),
    }
}

/// The wire payload of one state message, built the way that the daemon builds
/// it. The payload holds every session as a run of records.
///
/// Every agent sits in the pane that the user focuses. Each step answers the
/// agents that call. The readings measure the cost of an answered call.
fn payload(step: usize) -> String {
    let mut registry = Registry::default();
    for n in 0..AGENTS {
        // A real session id is a uuid, so the id is that long here too.
        let session = SessionId::new(&format!("ba3d783b-cba5-4059-8053-a13c0000000{n}")).unwrap();
        let origin = Origin::from_lookup(|name| match name {
            "ZELLIJ" => Some("0".to_string()),
            "ZELLIJ_SESSION_NAME" => Some(SESSION.to_string()),
            "ZELLIJ_PANE_ID" => Some(pane_id(step % TABS, step % PANES_PER_TAB)),
            _ => None,
        });
        let mut record = Agent::new(
            session,
            "claude",
            LabelFacts {
                dir: format!("repo-{n}"),
                name: String::new(),
                color: "cyan".to_string(),
                // A session retitles itself as the conversation moves on.
                title: format!("Investigating the failing build, step {step}"),
            },
            origin,
        );
        record.turn = match (step + n) % 3 {
            0 => Turn::Working,
            1 => Turn::Attention,
            _ => Turn::Idle,
        };
        record.raised = 1_700_000_000_000 + step as u64;
        registry.report(record);
    }
    agent::build_state_message(&registry.encode())
}

/// Reads a state message into the registry and the pane placement of a
/// snapshot. It keeps only the records that this session draws.
fn snapshot(payload: &str) -> AgentSnapshot {
    let (_, records) = agent::read_state_message(payload).expect("a state message");
    let mut registry = Registry::default();
    let mut panes = BTreeMap::new();
    for line in records.split('\n') {
        let Record::Known(agent) = Agent::decode(line) else {
            continue;
        };
        if agent.origin.get("ZELLIJ_SESSION_NAME") != Some(SESSION) {
            continue;
        }
        if let Some(pane) = agent
            .origin
            .get("ZELLIJ_PANE_ID")
            .filter(|pane| !pane.is_empty())
        {
            panes.insert(agent.session.clone(), PaneId::new(pane));
        }
        registry.report(agent);
    }
    AgentSnapshot::Compatible { registry, panes }
}

/// One burst of host events and one frame, exactly as the plugin receives them
/// and draws them.
fn step(app: &mut Application, step: usize) {
    app.reduce(Input::TabsReported(tabs(step)));
    app.reduce(Input::LayoutReported(layout(step)));
    app.reduce(Input::FocusObserved(Some(Focus {
        tab: TabId::new((step % TABS).to_string()),
        target: FocusTarget::Content(PaneId::new(pane_id(step % TABS, step % PANES_PER_TAB))),
    })));
    app.reduce(Input::PaneTitleObserved {
        pane: PaneId::new(pane_id(step % TABS, step % PANES_PER_TAB)),
        title: Some(format!("retitled at step {step}")),
    });

    let wire = payload(step);
    app.reduce(Input::Agents(snapshot(&wire)));
    app.reduce(Input::EventSettled);

    let rendered = app.render(area());
    let drawn = ansi::frame_to_ansi(&rendered.frame, rendered.selection.as_ref());
    std::hint::black_box(&drawn);
}

#[test]
fn a_long_running_session_holds_a_steady_heap() {
    let mut app = Application::new(Options::default());
    app.reduce(Input::VisibilityChanged(true));
    app.reduce(Input::SessionNamed(SESSION.to_string()));
    app.reduce(Input::PermissionReported(Permission::Granted));

    for n in 1..=SETTLE {
        step(&mut app, n);
    }
    let early = LIVE.load(Ordering::Relaxed);

    for n in SETTLE + 1..=STEPS {
        step(&mut app, n);
    }
    let late = LIVE.load(Ordering::Relaxed);

    assert!(
        late <= early + TOLERANCE,
        "the heap grew over {} steps: {early} bytes live at step {SETTLE}, {late} at step {STEPS}",
        STEPS - SETTLE,
    );
}
