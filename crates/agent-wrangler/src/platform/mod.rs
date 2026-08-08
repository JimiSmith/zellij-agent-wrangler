//! The process primitives this needs, chosen at compile time.
//!
//! Each supported system provides the same four functions, and the rest of the
//! crate calls the re-exported ones with no `cfg` of its own. A build for a
//! system with no module here fails to find them, which is the intended answer:
//! a missing port should not silently become a daemon that cannot tell a live
//! agent from a dead one.
//!
//! What is derived from those four, climbing the process table to find the
//! agent a hook belongs to and dating what it finds, is written once here for
//! all of them.

use std::collections::HashMap;

use agent_wrangler_core::agent::Process;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{pid_alive, processes, spawn_detached, started};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{pid_alive, processes, spawn_detached, started};

/// One row of the process table: who started this process, and what it is
/// running.
///
/// The name is whatever the system calls the running image, which is a bare name
/// on some and a path on others. Nothing here normalizes it, because what counts
/// as a match is the caller's question.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub ppid: u32,
    pub name: String,
}

/// The shells an agent is commonly invoked through, and which invoke a hook in
/// turn. None of them is ever the agent itself.
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "csh",
    "tcsh",
    "cmd",
    "powershell",
    "pwsh",
];

/// The process of the agent that invoked a hook, found by climbing from the
/// hook towards the root.
///
/// The nearest ancestor named for the agent wins. Failing that, the nearest one
/// that is not a shell does: an agent is not always named for itself, and one
/// installed through npm reports as `node`, which is nothing this could have a
/// list of. What both rules have in common is that they do not count steps,
/// because how many shells sit between a hook and its agent varies with how the
/// hook was invoked.
pub fn agent_process(pid: u32, agent: &str, table: &HashMap<u32, Row>, hops: u32) -> Option<u32> {
    let line = ancestors(pid, table, hops);
    let named = |ancestor: &&u32| {
        table
            .get(ancestor)
            .map(|process| runs(&process.name, agent))
            .unwrap_or(false)
    };
    if let Some(found) = line.iter().find(named) {
        return Some(*found);
    }
    line.iter()
        .find(|ancestor| {
            table
                .get(ancestor)
                .map(|process| !is_shell(&process.name))
                .unwrap_or(false)
        })
        .copied()
}

/// The process of the agent that invoked a hook, dated at the moment it is
/// found.
///
/// Side effect: asks the system when that process started, which is a second
/// reading and so a second moment. A process that ends between the two is named
/// undated rather than not named at all: knowing which process an agent was is
/// worth having even where nothing can later tell it from its successor.
pub fn agent_running(
    pid: u32,
    agent: &str,
    table: &HashMap<u32, Row>,
    hops: u32,
) -> Option<Process> {
    let found = agent_process(pid, agent, table, hops)?;
    Some(Process {
        pid: found,
        started: started(found),
    })
}

/// Whether the process a record names is still the process it named.
///
/// Two questions rather than one. Whether anything is running under that number
/// at all, and whether what is running under it began when this did: a number is
/// handed out again once its process has gone, so asking only the first question
/// eventually asks a stranger, and a stranger that is running answers that the
/// agent is.
///
/// Where either end could not be dated the number stands on its own, which is
/// the error worth making: an agent counted live too long is a row that goes
/// stale, an agent counted dead while it works is a row that vanishes under
/// someone.
pub fn running(process: &Process) -> bool {
    if !pid_alive(process.pid) {
        return false;
    }
    match (process.started, started(process.pid)) {
        (Some(then), Some(now)) => then == now,
        _ => true,
    }
}

/// Whether an image name is that program: the last path component, with any
/// extension taken off, compared without regard to case.
fn runs(image: &str, program: &str) -> bool {
    stem(image).eq_ignore_ascii_case(program)
}

/// Whether an image is one of the shells a hook is run through.
fn is_shell(image: &str) -> bool {
    let stem = stem(image);
    SHELLS.iter().any(|shell| stem.eq_ignore_ascii_case(shell))
}

/// The bare name of an image: the last path component, with any extension and
/// any trailing detail taken off.
///
/// A process name is a path on some systems, a bare name with an extension on
/// others, and a name with a thread appended on others again (`node-MainThread`),
/// so all three are cut back to the same thing.
fn stem(image: &str) -> &str {
    let file = image.rsplit(['/', '\\']).next().unwrap_or(image);
    let file = file.split('.').next().unwrap_or(file);
    file.split('-').next().unwrap_or(file)
}

/// Climb from `pid` towards the root, yielding each ancestor in turn.
///
/// Stops at the root, at a pid with no known parent, and after `hops` steps, so
/// a cycle in a truncated or racing snapshot cannot spin. The starting pid is
/// not yielded.
pub fn ancestors(pid: u32, table: &HashMap<u32, Row>, hops: u32) -> Vec<u32> {
    let mut seen = Vec::new();
    let mut current = pid;
    for _ in 0..hops {
        let Some(parent) = table.get(&current).map(|process| process.ppid) else {
            break;
        };
        if parent == 0 || parent == current || seen.contains(&parent) {
            break;
        }
        seen.push(parent);
        current = parent;
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(rows: &[(u32, u32, &str)]) -> HashMap<u32, Row> {
        rows.iter()
            .map(|(pid, ppid, name)| {
                (
                    *pid,
                    Row {
                        ppid: *ppid,
                        name: name.to_string(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn the_line_of_ancestors_comes_back_in_order() {
        let table = tree(&[(10, 9, "sh"), (9, 8, "claude"), (8, 1, "zellij")]);
        assert_eq!(ancestors(10, &table, 8), vec![9, 8, 1]);
    }

    #[test]
    fn a_pid_with_no_known_parent_ends_the_climb() {
        assert_eq!(ancestors(10, &tree(&[]), 8), Vec::<u32>::new());
    }

    #[test]
    fn the_climb_is_bounded_by_the_hops_it_is_given() {
        let table = tree(&[(10, 9, "a"), (9, 8, "b"), (8, 7, "c"), (7, 6, "d")]);
        assert_eq!(ancestors(10, &table, 2), vec![9, 8]);
    }

    #[test]
    fn a_cycle_cannot_spin() {
        // A snapshot taken while processes were being reaped can name a parent
        // that has since become a child.
        let table = tree(&[(10, 9, "a"), (9, 10, "b")]);
        assert_eq!(ancestors(10, &table, 64), vec![9, 10]);
    }

    #[test]
    fn the_root_ends_the_climb() {
        let table = tree(&[(10, 1, "sh"), (1, 0, "init")]);
        assert_eq!(ancestors(10, &table, 8), vec![1]);
    }

    #[test]
    fn the_agent_is_the_nearest_ancestor_running_it() {
        // A hook is a descendant of the agent with a shell or two in between,
        // and how many varies with how the hook was invoked.
        let table = tree(&[
            (10, 9, "agent-wrangler"),
            (9, 8, "sh"),
            (8, 7, "claude"),
            (7, 1, "bash"),
            (1, 0, "init"),
        ]);
        assert_eq!(agent_process(10, "claude", &table, 8), Some(8));
    }

    #[test]
    fn an_agent_not_named_for_itself_is_the_nearest_thing_that_is_not_a_shell() {
        // An agent installed through npm reports as its runtime, which is not a
        // name this could have a list of.
        let table = tree(&[
            (10, 9, "agent-wrangler"),
            (9, 8, "bash"),
            (8, 7, "node-MainThread"),
            (7, 1, "zsh"),
            (1, 0, "init"),
        ]);
        assert_eq!(agent_process(10, "claude", &table, 8), Some(8));
    }

    #[test]
    fn the_nearer_of_two_running_it_wins() {
        // An agent that started another agent: the hook belongs to the one it
        // is closest to.
        let table = tree(&[
            (10, 9, "agent-wrangler"),
            (9, 8, "claude"),
            (8, 7, "claude"),
            (7, 0, "bash"),
        ]);
        assert_eq!(agent_process(10, "claude", &table, 8), Some(9));
    }

    #[test]
    fn a_name_beats_a_position() {
        // A wrapper that is not a shell sits between the hook and the agent.
        // The one actually named for the agent is the one wanted.
        let table = tree(&[
            (10, 9, "agent-wrangler"),
            (9, 8, "npx"),
            (8, 7, "claude"),
            (7, 0, "bash"),
        ]);
        assert_eq!(agent_process(10, "claude", &table, 8), Some(8));
    }

    #[test]
    fn shells_all_the_way_up_leave_nothing_to_point_at() {
        let table = tree(&[(10, 9, "agent-wrangler"), (9, 8, "bash"), (8, 0, "sh")]);
        assert_eq!(agent_process(10, "claude", &table, 8), None);
    }

    #[test]
    fn an_image_is_matched_by_its_name_wherever_it_lives() {
        assert!(runs("claude", "claude"));
        assert!(runs("/usr/local/bin/claude", "claude"));
        assert!(runs(r"C:\Program Files\claude.exe", "claude"));
        assert!(runs("Claude.EXE", "claude"));
        assert!(!runs("claudius", "claude"));
    }

    #[test]
    fn a_thread_name_is_not_part_of_the_program() {
        // Linux reports a node process as `node-MainThread`.
        assert_eq!(stem("node-MainThread"), "node");
        assert!(is_shell("bash"));
        assert!(is_shell("/bin/zsh"));
        assert!(is_shell("cmd.exe"));
        assert!(!is_shell("claude"));
        assert!(!is_shell("node"));
    }

    #[test]
    fn this_process_is_alive_and_something_absurd_is_not() {
        assert!(pid_alive(std::process::id()));
        // Pid 0 is the scheduler on unix and the idle process on Windows;
        // neither is a process a hook could have descended from.
        assert!(!pid_alive(0));
    }

    #[test]
    fn a_process_is_itself_and_not_whatever_held_its_number_before() {
        // The whole point of dating a process: the number alone would answer
        // this the same way for a stranger that inherited it.
        let mine = Process {
            pid: std::process::id(),
            started: started(std::process::id()),
        };
        assert!(mine.started.is_some(), "this process can be dated");
        assert!(running(&mine));
        assert!(!running(&Process {
            started: Some(agent_wrangler_core::agent::Started(1)),
            ..mine
        }));
    }

    #[test]
    fn a_process_nothing_could_date_is_answered_by_its_number_alone() {
        assert!(running(&Process {
            pid: std::process::id(),
            started: None,
        }));
        assert!(!running(&Process {
            pid: 0,
            started: None,
        }));
    }

    #[test]
    fn the_agent_a_hook_belongs_to_comes_back_dated() {
        // The pid is this process's own ancestry rather than a real agent's,
        // which is enough to say that what is found is dated as it is found.
        let table = processes();
        let me = std::process::id();
        if let Some(process) = agent_running(me, "nothing-runs-under-this-name", &table, 8) {
            assert!(process.started.is_some(), "a live ancestor can be dated");
        }
    }

    #[test]
    fn the_process_table_holds_this_process_and_what_it_is_running() {
        let table = processes();
        let me = table
            .get(&std::process::id())
            .expect("the running process is not in the table it was read from");
        assert!(me.ppid != 0, "this process has a parent");
        assert!(!me.name.is_empty(), "this process is running something");
    }
}
