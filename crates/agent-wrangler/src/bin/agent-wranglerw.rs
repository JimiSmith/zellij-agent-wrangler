//! The client linked for the windows subsystem, which is the whole of the
//! difference. This program never gets a console, so nothing that starts it
//! makes a console window appear.
//!
//! Windows allocates a console for a console program whose parent has none, and
//! it draws a window for that console. The programs that run this one are often
//! exactly such a parent. Two examples are an agent that runs a hook, and a
//! zellij server that runs the client of the sidebar. This program exists to
//! stop a window that appears once per hook. A subsystem decides only whether Windows allocates a
//! console. The handles that a parent passes down stay untouched. A hook still
//! reads its payload from the pipe that it gets, and it still writes back what
//! the caller captures.
//!
//! The attribute does nothing off Windows. There this program is the same
//! program under a second name.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    agent_wrangler::run()
}
