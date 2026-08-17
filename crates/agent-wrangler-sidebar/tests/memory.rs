//! The sidebar's state must not grow with how long a session has been running.
//!
//! The plugin lives in wasm linear memory, which only ever grows: nothing is
//! handed back to the host, so anything the application keeps per event becomes
//! memory the pane never gives up. This drives `Application` through the burst
//! of inputs a real session produces and compares the live heap early against
//! the live heap much later. Steady state is the only acceptable answer; a
//! reading that climbs with the step count is state accumulating per call.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent_wrangler_core::agent::{self, Agent, Meta, Record, SessionId, Turn};
use agent_wrangler_core::origin::Origin;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_sidebar::{
    AgentSnapshot, Application, Focus, FocusTarget, Input, Options, PaneId, PaneReport, Permission,
    SessionLayout, SidebarPaneReport, TabId, TabLayout, TabReport,
};
use agent_wrangler_ui::model::TabPosition;
use agent_wrangler_ui::{ansi, Rect};

static LIVE: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, counting the bytes currently handed out.
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

/// Where the readings are taken, and how far apart they are allowed to drift.
///
/// The early reading is late enough that everything reached once has been
/// reached, so what separates the two checkpoints is only what the steps
/// between them left behind. Standing still costs tens of bytes across this
/// many steps while anything held per attention call runs into hundreds of
/// kilobytes, which leaves room for a tolerance loose enough never to be flaky
/// and still decisive.
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
            // Tab names change as the user renames and as zellij renumbers.
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
                        // A shell prompt retitles its pane constantly; every
                        // one of these is a string the sidebar has not seen.
                        title: format!("~/work/repo-{tab}-{pane} $ step {step}"),
                        focused: tab == step % TABS && pane == step % PANES_PER_TAB,
                    })
                    .collect(),
                sidebar_pane: Some(SidebarPaneReport { focused: false }),
            })
            .collect(),
    }
}

/// The wire payload one state message carries, built the way the daemon builds
/// it: every session it holds, encoded as a run of records.
///
/// Every agent sits in the pane the user is focusing, so each step answers
/// whichever of them are calling and the cost of an answered call is what the
/// readings measure.
fn payload(step: usize) -> String {
    let mut registry = Registry::default();
    for n in 0..AGENTS {
        // A real session id is a uuid, so the id is that long here too.
        let session = SessionId::new(&format!("ba3d783b-cba5-4059-8053-a13c0000000{n}")).unwrap();
        let origin = Origin::from(|name| match name {
            "ZELLIJ" => Some("0".to_string()),
            "ZELLIJ_SESSION_NAME" => Some(SESSION.to_string()),
            "ZELLIJ_PANE_ID" => Some(pane_id(step % TABS, step % PANES_PER_TAB)),
            _ => None,
        });
        let mut record = Agent::new(
            session,
            "claude",
            Meta {
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
    agent::state(&registry.encode())
}

/// Read a state message into the registry and pane placement a snapshot
/// carries, keeping only the records this session is the one drawing.
fn snapshot(payload: &str) -> AgentSnapshot {
    let (_, records) = agent::read_state(payload).expect("a state message");
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

/// One burst of host events and one frame, exactly as the plugin receives and
/// draws them.
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
    let drawn = ansi::pane(&rendered.frame, rendered.selection.as_ref());
    std::hint::black_box(&drawn);
}

#[test]
fn a_long_running_session_holds_a_steady_heap() {
    let mut app = Application::new(Options::default(), "zellij");
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
