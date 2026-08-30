//! The process primitives this needs, chosen at compile time.
//!
//! Each supported system provides the same six functions. The rest of the
//! crate calls the re-exported ones with no `cfg` of its own. A build for a
//! system with no module here fails to find them, which is the intended
//! answer. A missing port must not become a daemon that cannot tell a live
//! agent from a dead one.
//!
//! The start of a program is one of the six for the same reason. What it takes
//! to start one without disturbance to the user is the business of the system.
//! A caller that built its own start is a caller that must know that business.
//!
//! The claim of a socket name is one of the six because the two systems answer
//! it in different ways. A unix socket is a file, and it outlives the process
//! that bound it. A claim there must first tell a live listener from a file
//! that a dead daemon left behind. A named pipe dies with its process, and
//! Windows refuses to create a name that exists, so the create is the whole
//! claim.
//!
//! Three things derive from the process primitives. The first is the climb up
//! the process table to the agent that a hook belongs to. The second is the
//! start time of what the climb finds. The third is a wait for a program that
//! lasts no longer than it is worth. All three are written once here for every
//! system.

use std::collections::HashMap;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use agent_wrangler_core::agent::Process;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{claim_socket_name, command, pid_alive, processes, spawn_detached, started};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{claim_socket_name, command, pid_alive, processes, spawn_detached, started};

/// One row of the process table: the process that started this process, and the
/// image that this process runs.
///
/// The name is whatever the system calls the image. The name is a bare name on
/// some systems and a path on others. Nothing here makes the name uniform,
/// because the caller decides what counts as a match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessTableRow {
    pub ppid: u32,
    pub name: String,
}

/// The result of a program that this ran and waited for.
///
/// If a program still runs when the wait ends, that result is an answer of its
/// own and not a failure. The program can already do the work that it was asked
/// for, and only the exit is absent. The caller decides what that is worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ran {
    /// The program ended and reported success.
    Worked,
    /// The program ended and reported failure, or it did not start at all.
    Failed,
    /// The program still ran when the wait ended, so this killed and reaped it.
    Abandoned,
}

/// The interval between two questions to a live program about its end.
///
/// When all is well, the programs that this runs return in tens of
/// milliseconds. A question at this interval therefore adds no delay that
/// anybody can see. A wait that runs to its end costs a hundred wakeups a
/// second, and not a thread that spins on a core.
const ASKED_AGAIN: Duration = Duration::from_millis(10);

/// Runs a program, waits for it, and gives up on it after `patience`.
///
/// Side effect: spawns a process. If the process outstays the wait, this kills
/// it.
///
/// The wait is the reason for this function. A child that never exits makes a
/// caller that never returns. `zellij pipe` does exactly that now and then,
/// after it hands its payload over. One such call blocked every delivery of a
/// daemon for the best part of an hour, and every sidebar with it.
///
/// This waits for a child that it gives up on, and does not only kill it. A
/// process that receives a signal and is not reaped stays in the table as a
/// zombie. A daemon runs for as long as the user stays logged in. One zombie
/// per delivery makes a table full of them by the evening.
pub fn ran(program: &mut Command, patience: Duration) -> Ran {
    let Ok(mut child) = program.spawn() else {
        return Ran::Failed;
    };
    let until = Instant::now() + patience;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return match status.success() {
                    true => Ran::Worked,
                    false => Ran::Failed,
                }
            }
            Ok(None) if Instant::now() >= until => return abandon(&mut child),
            // A child that answers no question once will answer no later
            // question. This gives up on it as it does on a child that
            // outstayed the wait. In both cases this process is the only one
            // that can reap it.
            Err(_) => return abandon(&mut child),
            Ok(None) => thread::sleep(ASKED_AGAIN),
        }
    }
}

/// Kills a child and waits for it, so that nothing of it remains.
fn abandon(child: &mut Child) -> Ran {
    let _ = child.kill();
    let _ = child.wait();
    Ran::Abandoned
}

/// The shells that commonly start an agent, and that start a hook in turn. None
/// of these shells is ever the agent itself.
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

/// The process of the agent that started a hook. The climb runs from the hook
/// towards the root.
///
/// The nearest ancestor named for the agent wins. If no ancestor carries that
/// name, the nearest ancestor that is not a shell wins. An agent is not always
/// named for itself. An agent installed through npm reports as `node`, and no
/// list here can hold every such name. Neither rule counts steps, because the
/// number of shells between a hook and its agent depends on how the hook
/// started.
pub fn agent_process(
    pid: u32,
    agent: &str,
    table: &HashMap<u32, ProcessTableRow>,
    hops: u32,
) -> Option<u32> {
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

/// The process of the agent that started a hook, with its start time.
///
/// Side effect: asks the system for the start time of that process. That
/// question is a second reading, and so a second moment. If the process ends
/// between the two readings, the result names the process with no start time,
/// and does not drop the name. The identity of the agent process is worth a
/// record, even when nothing can later tell that process from its successor.
pub fn agent_running(
    pid: u32,
    agent: &str,
    table: &HashMap<u32, ProcessTableRow>,
    hops: u32,
) -> Option<Process> {
    let found = agent_process(pid, agent, table, hops)?;
    Some(Process {
        pid: found,
        started: started(found),
    })
}

/// Whether the process that a record names is still the process that it named.
///
/// This asks two questions and not one. The first question is whether any
/// process runs under that number. The second question is whether the process
/// under that number started when this record started. The system hands a
/// number out again after its process ends. The first question alone therefore
/// reaches a stranger in the end, and a live stranger answers that the agent is
/// live.
///
/// If either reading carries no start time, the number alone answers the
/// question. That is the error to accept. An agent counted live too long leaves
/// a stale row. An agent counted dead while it works makes a row vanish under
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

/// Whether an image name is that program. The comparison uses the last path
/// component, without the extension, and ignores case.
fn runs(image: &str, program: &str) -> bool {
    stem(image).eq_ignore_ascii_case(program)
}

/// Whether an image is one of the shells that start a hook.
fn is_shell(image: &str) -> bool {
    let stem = stem(image);
    SHELLS.iter().any(|shell| stem.eq_ignore_ascii_case(shell))
}

/// The bare name of an image: the last path component, without the extension
/// and without any detail that follows the name.
///
/// A process name is a path on some systems. On other systems it is a bare name
/// with an extension. On other systems again it is a name with a thread name
/// appended, such as `node-MainThread`. This cuts all three forms back to the
/// same name.
fn stem(image: &str) -> &str {
    let file = image.rsplit(['/', '\\']).next().unwrap_or(image);
    let file = file.split('.').next().unwrap_or(file);
    file.split('-').next().unwrap_or(file)
}

/// Climbs from `pid` towards the root and returns each ancestor in turn.
///
/// The climb stops at the root, at a pid with no known parent, and after `hops`
/// steps. A cycle in a snapshot that is truncated, or that was taken during a
/// change, therefore cannot spin. The result does not hold the pid that the
/// climb started from.
pub fn ancestors(pid: u32, table: &HashMap<u32, ProcessTableRow>, hops: u32) -> Vec<u32> {
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

    fn tree(rows: &[(u32, u32, &str)]) -> HashMap<u32, ProcessTableRow> {
        rows.iter()
            .map(|(pid, ppid, name)| {
                (
                    *pid,
                    ProcessTableRow {
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
        // A snapshot taken during the reap of processes can name a parent that
        // later became a child.
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
        // A hook is a descendant of the agent, with one or two shells in
        // between. The number of shells depends on how the hook started.
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
        // An agent installed through npm reports as its runtime. No list here
        // can hold that name.
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
        // One agent started another agent. The hook belongs to the nearer of
        // the two.
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
        // The process named for the agent is the process that this wants.
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
        // Pid 0 is the scheduler on unix and the idle process on Windows. A
        // hook descends from neither of them.
        assert!(!pid_alive(0));
    }

    #[test]
    fn a_process_is_itself_and_not_whatever_held_its_number_before() {
        // This is the reason for the start time on a process. The number alone
        // gives the same answer for a stranger that inherited it.
        let mine = Process {
            pid: std::process::id(),
            started: started(std::process::id()),
        };
        assert!(mine.started.is_some(), "this process can be dated");
        assert!(running(&mine));
        assert!(!running(&Process {
            started: Some(agent_wrangler_core::agent::ProcessStartStamp(1)),
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
        // The climb walks the ancestry of this process and not the ancestry of
        // a real agent. That is enough to show that the result carries a start
        // time.
        let table = processes();
        let me = std::process::id();
        if let Some(process) = agent_running(me, "nothing-runs-under-this-name", &table, 8) {
            assert!(process.started.is_some(), "a live ancestor can be dated");
        }
    }

    // A program that never exits is the reason for the wait. The two systems
    // share no spelling of such a program. The failure is not unix-only. The
    // sleeper is unix-only.
    #[cfg(unix)]
    const LONGER_THAN_ANY_TEST: &str = "3600";

    /// The number of children of this process that run `sleep`.
    ///
    /// A child that this killed and did not wait for is still one of these. The
    /// process table holds that child until somebody reaps it. A kill without a
    /// wait leaves nothing else behind.
    #[cfg(unix)]
    fn sleepers() -> usize {
        let me = std::process::id();
        processes()
            .values()
            .filter(|row| row.ppid == me && stem(&row.name) == "sleep")
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_worked_says_so() {
        assert_eq!(
            ran(&mut command("true"), Duration::from_secs(5)),
            Ran::Worked
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_refused_says_so_without_being_waited_out() {
        // A client that went away gives this answer, and the answer is fast. A
        // wait on it is a wait on every delivery.
        let began = Instant::now();
        assert_eq!(
            ran(&mut command("false"), Duration::from_secs(30)),
            Ran::Failed
        );
        assert!(began.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_program_that_is_not_there_is_a_program_that_failed() {
        assert_eq!(
            ran(
                &mut command("/nonexistent/agent-wrangler/program"),
                Duration::from_millis(50)
            ),
            Ran::Failed
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_never_ends_is_given_up_on_and_leaves_nothing_behind() {
        // This is the failure that the wait exists for. Without the wait, this
        // call is the end of the thread that made it.
        let mut sleeper = command("sleep");
        sleeper.arg(LONGER_THAN_ANY_TEST);
        let began = Instant::now();
        assert_eq!(
            ran(&mut sleeper, Duration::from_millis(200)),
            Ran::Abandoned
        );
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "the wait ended long before the program would have"
        );
        assert_eq!(sleepers(), 0, "the child was reaped as well as killed");
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
