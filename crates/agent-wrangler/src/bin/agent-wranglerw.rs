//! The client linked for the windows subsystem, which is the whole of the
//! difference: it is never given a console, so nothing that starts it can make
//! a console window appear.
//!
//! Windows allocates a console for a console program whose parent has none, and
//! draws a window for it. The programs that run this one - an agent running a
//! hook, a zellij server running the sidebar's client - are often exactly that,
//! and a window flashing up once per hook is what this exists to stop. A
//! subsystem decides only whether a console is allocated: the handles a parent
//! passes down are untouched, so a hook still reads its payload from the pipe
//! it is given and still writes back what the caller captures.
//!
//! The attribute does nothing off Windows, where this is the same program under
//! a second name.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    agent_wrangler::run()
}
