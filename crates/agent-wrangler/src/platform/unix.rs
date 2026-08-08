//! The unix process primitives.

use std::collections::HashMap;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::Process;

/// Start a program that outlives the process that started it.
///
/// Side effect: spawns a process and never waits for it. It is put in a process
/// group of its own, so the terminal that closes on the pane the hook ran in
/// hangs up its own foreground group without reaching this.
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

/// Whether a process is still running.
///
/// Signal 0 is the one that checks without delivering. A process that exists but
/// belongs to another user answers `EPERM` rather than `ESRCH`, which is still
/// an answer that it exists.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 delivers nothing and only reports whether the
    // process could be signalled. It cannot affect this process or any other.
    let sent = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if sent == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Every process, its parent and what it is running, as one snapshot.
///
/// Side effect: runs `ps`. The header-less form prints the same on Linux and on
/// macOS, which is what lets one command serve both without reaching for
/// `/proc`. One call rather than two is what keeps the parent of a process and
/// its name from being read a moment apart, when either could have changed.
pub fn processes() -> HashMap<u32, Process> {
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

/// Read `pid ppid command` rows, one per line, skipping anything else.
///
/// The command is the rest of the line, because `comm` prints a path on macOS
/// and a bare name on Linux and neither is guaranteed to be one word.
fn parse_processes(text: &str) -> HashMap<u32, Process> {
    text.lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let pid = columns.next()?.parse().ok()?;
            let ppid = columns.next()?.parse().ok()?;
            let name = columns.collect::<Vec<&str>>().join(" ");
            Some((pid, Process { ppid, name }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(ppid: u32, name: &str) -> Process {
        Process {
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

    #[test]
    fn anything_that_is_not_a_row_is_passed_over() {
        let text = "PID PPID COMMAND\n\n  1 0 init\nnonsense\n  x y z\n  7\n";
        assert_eq!(
            parse_processes(text),
            HashMap::from([(1, process(0, "init"))])
        );
    }
}
