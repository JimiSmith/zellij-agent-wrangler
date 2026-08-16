//! The Windows process primitives.

use std::collections::HashMap;
use std::io;
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, FALSE, FILETIME, INVALID_HANDLE_VALUE, STILL_ACTIVE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
    DETACHED_PROCESS, PROCESS_QUERY_LIMITED_INFORMATION,
};

use agent_wrangler_core::agent::Started;

use super::Row;

/// A program to run and wait for, built so that running it cannot make a window
/// appear.
///
/// Windows gives a console program whose parent has no console one of its own,
/// and draws a window for it. Everything that runs a program here is the daemon,
/// which is started detached and so has exactly no console, which would make a
/// delivery to a client and a desktop notification each a window flashing up on
/// the user's screen. `CREATE_NO_WINDOW` is what says to give it a console with
/// no window rather than one with.
///
/// A program is built through this rather than the flag being set at each call,
/// so that adding a program to run is not a thing to remember.
pub fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Start a program that outlives the process that started it.
///
/// Side effect: spawns a process and never waits for it. `DETACHED_PROCESS`
/// keeps it off the console the hook was given, so the console that goes away
/// with that pane cannot take this with it, and `CREATE_NEW_PROCESS_GROUP`
/// keeps the Ctrl+C that group receives from reaching it. `CREATE_NO_WINDOW` is
/// ignored while `DETACHED_PROCESS` is set and is asked for anyway, because it
/// is what stops a console window flashing up if the detachment is ever dropped.
pub fn spawn_detached(program: &Path, args: &[&str]) -> io::Result<()> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

/// Whether a process is still running.
///
/// Opening the process is not on its own an answer. A process that has exited
/// keeps its kernel object, and so keeps answering `OpenProcess`, for as long as
/// anything still holds a handle to it, so the exit code has to be read as well.
/// A process that exists but is out of reach answers `ERROR_ACCESS_DENIED`
/// rather than a failure to find it, which is still an answer that it exists.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `OpenProcess` only reads kernel state and returns either a handle
    // this owns or null. The rights asked for are the narrowest that allow the
    // exit code to be read, so the handle cannot be used to alter the process.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if process.is_null() {
        return io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
    }

    let mut code = 0u32;
    // SAFETY: `process` is a live handle this just opened, and `code` is a `u32`
    // this owns, which is what the out parameter is documented to want.
    let read = unsafe { GetExitCodeProcess(process, &mut code) };
    // SAFETY: `process` came from `OpenProcess` above and is not used again.
    unsafe { CloseHandle(process) };

    // A handle that opened but would not answer is counted as alive, on the same
    // reasoning as `still_running`.
    read == FALSE || still_running(code)
}

/// Whether an exit code read back from a process handle means it is running.
///
/// `STILL_ACTIVE` is also an exit code a process is free to end with, and
/// nothing in the API tells the two apart, so a process that exits with 259
/// reads as alive until every handle to it is closed and its pid stops
/// resolving. That is the error worth making: an agent counted live one poll too
/// long is a row that goes stale, an agent counted dead while it works is a row
/// that vanishes under someone.
fn still_running(exit_code: u32) -> bool {
    exit_code == STILL_ACTIVE as u32
}

/// When a process started, or `None` for one this cannot ask about.
///
/// Side effect: opens the process for the length of the call. The creation time
/// comes back as a `FILETIME`, which is a count of hundred-nanosecond intervals
/// split across two words; the two are folded back into the one number they
/// stand for and left in those units, since nothing reads the figure and only
/// ever compares it with another reading of the same process.
pub fn started(pid: u32) -> Option<Started> {
    if pid == 0 {
        return None;
    }
    // SAFETY: as in `pid_alive`, this only reads kernel state and returns either
    // a handle this owns or null, with the narrowest rights that answer the
    // question.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if process.is_null() {
        return None;
    }

    let mut creation = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `process` is a live handle this just opened, and all four out
    // parameters are records owned here that outlive the call. The API writes
    // every one of them, so all four are passed even though one is wanted.
    let read =
        unsafe { GetProcessTimes(process, &mut creation, &mut exited, &mut kernel, &mut user) };
    // SAFETY: `process` came from `OpenProcess` above and is not used again.
    unsafe { CloseHandle(process) };

    match read == FALSE {
        true => None,
        false => Some(Started(moment(creation))),
    }
}

/// The one number a `FILETIME`'s two words stand for.
fn moment(time: FILETIME) -> u64 {
    ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
}

/// Every process, its parent and what it is running, as one snapshot.
///
/// Side effect: takes a ToolHelp snapshot, which walks the whole process list at
/// the moment it is called. Each record already carries both the parent and the
/// image, so one walk answers both and neither can be read a moment apart from
/// the other.
///
/// The name comes back as the system gave it, which here is a bare file name
/// with its extension still on it, `claude.exe` rather than `claude`. Nothing is
/// stripped, because what counts as a match belongs to the caller.
///
/// Unlike a unix parent id, the one recorded here is not updated when the parent
/// dies: an orphan keeps naming the pid it was started by rather than being
/// reparented, and Windows reuses pids, so a long enough climb can arrive at a
/// process that merely inherited the number. Climbing by name over a bounded
/// number of hops is what keeps that from mattering.
pub fn processes() -> HashMap<u32, Row> {
    // SAFETY: `TH32CS_SNAPPROCESS` with pid 0 asks for the process list, which
    // takes no buffer from this and returns either a handle this owns or
    // `INVALID_HANDLE_VALUE`.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return HashMap::new();
    }

    let mut table = HashMap::new();
    // The walk refuses to start unless the size is filled in, which is how the
    // API tells which version of the record it has been handed.
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    // SAFETY: `snapshot` is a live snapshot handle and `entry` is owned here,
    // outlives the call, and has the `dwSize` the API reads to size its write.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != FALSE {
        table.insert(
            entry.th32ProcessID,
            Row {
                ppid: entry.th32ParentProcessID,
                name: image_name(&entry.szExeFile),
            },
        );
        // SAFETY: the same handle and the same owned record, which the previous
        // call left with its `dwSize` intact.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }

    // SAFETY: `snapshot` came from `CreateToolhelp32Snapshot` and the walk over
    // it has finished.
    unsafe { CloseHandle(snapshot) };
    table
}

/// Read the image name out of the fixed-width field a snapshot record carries.
///
/// The field is a NUL-terminated wide string in an array that is always its full
/// width, so the name ends at the first NUL and not at the end of the buffer. A
/// field with no NUL at all is taken whole rather than dropped. Decoding is lossy
/// because a file name Windows permits need not be well-formed UTF-16, and a row
/// with an unpaired surrogate in it is still a row worth having: it keeps its pid
/// and its parent, which is most of what the climb wants.
fn image_name(raw: &[u16]) -> String {
    let end = raw.iter().position(|unit| *unit == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_that_means_running_is_the_only_one_that_does() {
        assert!(still_running(STILL_ACTIVE as u32));
        assert!(!still_running(0));
        assert!(!still_running(1));
    }

    #[test]
    fn an_ordinary_exit_code_is_not_mistaken_for_running() {
        // The neighbours of 259 are ordinary exit codes and must read as exited,
        // which is what pins the comparison to the constant rather than a range.
        assert!(!still_running(258));
        assert!(!still_running(260));
    }

    #[test]
    fn the_two_words_of_a_moment_make_one_number() {
        assert_eq!(
            moment(FILETIME {
                dwHighDateTime: 0x01DB_0000,
                dwLowDateTime: 0x1234_5678,
            }),
            0x01DB_0000_1234_5678
        );
        assert_eq!(moment(FILETIME::default()), 0);
    }

    #[test]
    fn this_process_started_at_a_moment_it_keeps_reporting() {
        // Whatever the number is, it is the same number every time it is asked
        // for, which is the whole of what telling one process from another
        // needs of it.
        let mine = started(std::process::id()).expect("this process has a start time");
        assert_eq!(started(std::process::id()), Some(mine));
        // Pid 0 is the idle process, which is nothing an agent could be.
        assert_eq!(started(0), None);
    }

    /// A field as the snapshot leaves it: the name, a NUL, then whatever was
    /// already in the buffer.
    fn field(name: &str, trailing: &[u16]) -> Vec<u16> {
        let mut raw: Vec<u16> = name.encode_utf16().collect();
        raw.push(0);
        raw.extend_from_slice(trailing);
        raw
    }

    #[test]
    fn a_name_ends_at_the_first_nul_and_not_at_the_end_of_the_field() {
        // The bytes past the NUL are not cleared, and reading them would turn
        // the name of one process into the name of whatever was there before.
        let raw = field("claude.exe", &[b'j' as u16, b'u' as u16, 0, 0, 0]);
        assert_eq!(image_name(&raw), "claude.exe");
    }

    #[test]
    fn the_extension_and_the_case_are_left_on_the_name() {
        let raw = field("Claude.EXE", &[0; 4]);
        assert_eq!(image_name(&raw), "Claude.EXE");
    }

    #[test]
    fn a_field_with_no_nul_in_it_is_taken_whole() {
        let raw: Vec<u16> = "claude.exe".encode_utf16().collect();
        assert_eq!(image_name(&raw), "claude.exe");
    }

    #[test]
    fn an_empty_field_is_an_empty_name() {
        assert_eq!(image_name(&[0; 8]), "");
        assert_eq!(image_name(&[]), "");
    }

    #[test]
    fn a_name_that_is_not_well_formed_still_yields_a_row() {
        // An unpaired surrogate cannot be decoded, and losing the whole row over
        // it would lose the parent id that the climb actually needs.
        let raw = field("cla\u{FFFF}", &[0xD800, 0]);
        assert!(!image_name(&raw).is_empty());
    }
}
