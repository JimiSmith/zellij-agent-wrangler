//! The unix process primitives.
//!
//! The start time of a process is the one primitive with no shared answer. `ps`
//! reports elapsed time and not a start time, and rounds it, so two readings of
//! one process differ by up to a second. This therefore asks each system in its
//! own way for the figure that the system records.

use std::collections::HashMap;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use agent_wrangler_core::agent::Started;

use super::Row;

/// A program to run and wait for, which here is nothing but the program. A unix
/// process starts with no window in either case.
pub fn command(program: &str) -> Command {
    Command::new(program)
}

/// Starts a program that outlives the process that started it.
///
/// Side effect: spawns a process and never waits for it. The new process gets a
/// process group of its own. The terminal closes with the pane that ran the
/// hook, and the hangup reaches its own foreground group and not the new
/// process.
pub fn spawn_detached(program: &Path, args: &[&str]) -> io::Result<()> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// Whether a process still runs.
///
/// Signal 0 is the signal that asks the question and delivers nothing. A process
/// that exists but belongs to another user answers `EPERM` and not `ESRCH`. That
/// answer still means that the process exists.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 delivers nothing, and only reports whether a
    // signal to the process is possible. It cannot affect this process or any
    // other process.
    let sent = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if sent == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The start time of a process, or `None` when the start time is not available.
///
/// Side effect: reads `/proc`, where Linux keeps the figure. The figure counts
/// clock ticks since the boot of the machine, and stays in those units. Nothing
/// shows the figure to a user. The only use of the figure is a comparison with
/// another reading of the same process.
#[cfg(target_os = "linux")]
pub fn started(pid: u32) -> Option<Started> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    starttime(&stat).map(Started)
}

/// The start time out of a `/proc/<pid>/stat` line: the twenty-second field.
///
/// The second field is the name of the executable in parentheses, and a name can
/// hold spaces and parentheses of its own. The count therefore starts after the
/// last `)` in the line, and not at the start of the line. The first field after
/// that point is the third field of the record.
#[cfg(target_os = "linux")]
fn starttime(stat: &str) -> Option<u64> {
    const STARTTIME: usize = 22;
    const AFTER_NAME: usize = 3;
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace()
        .nth(STARTTIME - AFTER_NAME)?
        .parse()
        .ok()
}

/// The start time of a process, or `None` for a process that answers no
/// question.
///
/// Side effect: asks the kernel for the BSD record of the process, which carries
/// the start moment as a `timeval`. This folds the two halves into microseconds,
/// so that one number stands for the whole moment.
#[cfg(target_os = "macos")]
pub fn started(pid: u32) -> Option<Started> {
    // SAFETY: `proc_bsdinfo` is a plain C record of integers and arrays, so an
    // all-zero record is a valid value of it. The call below overwrites the
    // whole record, or reports that it wrote nothing.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is the record declared above, and it outlives the
    // call. The size is the size of that record, so the kernel cannot write past
    // it. `PROC_PIDTBSDINFO` is the flavor of that record.
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut libc::proc_bsdinfo as *mut libc::c_void,
            size,
        )
    };
    // A read short of the whole record is a record that nothing filled in.
    if read != size {
        return None;
    }
    Some(Started(
        info.pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec),
    ))
}

/// Every process, its parent and the image that it runs, as one snapshot.
///
/// Side effect: runs `ps`. The header-less form prints the same output on Linux
/// and on macOS, so one command serves both systems without `/proc`. One call,
/// and not two calls, reads the parent of a process and its name at the same
/// moment. Between two moments either one can change.
pub fn processes() -> HashMap<u32, Row> {
    let out = Command::new("ps")
        .args(["-e", "-o", "pid=", "-o", "ppid=", "-o", "comm="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(out) => parse_processes(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => HashMap::new(),
    }
}

/// Reads `pid ppid command` rows, one per line, and passes over anything else.
///
/// The command is the rest of the line. `comm` prints a path on macOS and a bare
/// name on Linux, and neither form is always one word.
fn parse_processes(text: &str) -> HashMap<u32, Row> {
    text.lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let pid = columns.next()?.parse().ok()?;
            let ppid = columns.next()?.parse().ok()?;
            let name = columns.collect::<Vec<&str>>().join(" ");
            Some((pid, Row { ppid, name }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(ppid: u32, name: &str) -> Row {
        Row {
            ppid,
            name: name.to_string(),
        }
    }

    #[test]
    fn a_row_per_line_is_read() {
        let text = "  1     0 systemd\n 42     1 bash\n4242   42 claude\n";
        assert_eq!(
            parse_processes(text),
            HashMap::from([
                (1, process(0, "systemd")),
                (42, process(1, "bash")),
                (4242, process(42, "claude")),
            ])
        );
    }

    #[test]
    fn a_command_with_a_space_in_it_is_kept_whole() {
        // `comm` prints a path on macOS, and a path can hold a space.
        let text = "42 1 /Applications/My Agent.app/agent\n";
        assert_eq!(
            parse_processes(text),
            HashMap::from([(42, process(1, "/Applications/My Agent.app/agent"))])
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_start_time_is_the_field_the_kernel_puts_it_in() {
        // A real line, cut to the fields that matter: the pid, the name, and
        // then the rest from the state onwards. The twenty-second field is the
        // one that this wants.
        let stat = "2769470 (cat) R 2769450 2769470 2769450 34816 2769470 4194304 88 0 0 0 \
                    0 0 0 0 20 0 1 0 53723518 3358720 418 18446744073709551615";
        assert_eq!(starttime(stat), Some(53_723_518));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_holding_spaces_and_brackets_does_not_shift_the_count() {
        // The name is whatever the program called itself. The count of the
        // fields starts after the name for exactly this reason.
        let stat = "42 (my ) program) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 99 x y";
        assert_eq!(starttime(stat), Some(99));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_line_that_is_not_a_stat_record_says_nothing() {
        assert_eq!(starttime(""), None);
        assert_eq!(starttime("42 (cat) R 1 2 3"), None);
        assert_eq!(
            starttime("42 (cat) R 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 later"),
            None
        );
    }

    #[test]
    fn this_process_started_at_a_moment_it_keeps_reporting() {
        // The value of the number does not matter. The number is the same
        // number at every reading, which is all that a difference between one
        // process and another needs of it.
        let mine = started(std::process::id()).expect("this process has a start time");
        assert_eq!(started(std::process::id()), Some(mine));
    }

    #[test]
    fn anything_that_is_not_a_row_is_passed_over() {
        let text = "PID PPID COMMAND\n\n  1 0 init\nnonsense\n  x y z\n  7\n";
        assert_eq!(
            parse_processes(text),
            HashMap::from([(1, process(0, "init"))])
        );
    }
}
