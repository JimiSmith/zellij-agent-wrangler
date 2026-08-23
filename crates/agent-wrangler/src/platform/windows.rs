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

use agent_wrangler_core::agent::ProcessStartStamp;

use super::ProcessTableRow;

/// A program to run and wait for. A run of this program cannot make a window
/// appear.
///
/// If the parent has no console, Windows gives a console program a console of
/// its own, and draws a window for that console. The daemon runs every program
/// here. The daemon starts detached and so has no console at all. Without the
/// flag, a delivery to a client and a desktop notification each show a window
/// on the screen of the user. `CREATE_NO_WINDOW` asks for a console with no
/// window.
///
/// This builds every program, and no call sets the flag for itself. A new
/// program to run is therefore not a thing to remember.
pub fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Starts a program that outlives the process that started it.
///
/// Side effect: spawns a process and never waits for it. `DETACHED_PROCESS`
/// keeps the new process off the console of the hook. That console goes away
/// with its pane, and cannot take the new process with it.
/// `CREATE_NEW_PROCESS_GROUP` keeps the Ctrl+C of that group away from the new
/// process. Windows ignores `CREATE_NO_WINDOW` while `DETACHED_PROCESS` is set.
/// This asks for it anyway. If somebody ever drops the detachment, the flag
/// stops a console window.
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

/// Whether a process still runs.
///
/// A successful `OpenProcess` is not an answer on its own. A process that
/// exited keeps its kernel object for as long as anything holds a handle to it,
/// and `OpenProcess` still succeeds for it. This therefore reads the exit code
/// as well. A process that exists but is out of reach answers
/// `ERROR_ACCESS_DENIED` and not a failure to find it. That answer still means
/// that the process exists.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `OpenProcess` only reads kernel state, and returns either a
    // handle that this owns or null. The rights are the narrowest rights that
    // give a read of the exit code, so the handle cannot alter the process.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if process.is_null() {
        return io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
    }

    let mut code = 0u32;
    // SAFETY: `process` is a live handle that this opened above, and `code` is
    // a `u32` that this owns. The documentation of the out parameter asks for
    // exactly that.
    let read = unsafe { GetExitCodeProcess(process, &mut code) };
    // SAFETY: `process` came from `OpenProcess` above, and nothing uses it
    // again.
    unsafe { CloseHandle(process) };

    // A handle that opened but gave no answer counts as alive, for the same
    // reason as `still_running`.
    read == FALSE || still_running(code)
}

/// Whether an exit code from a process handle means that the process runs.
///
/// A process is also free to end with `STILL_ACTIVE` as its exit code, and
/// nothing in the API tells the two cases apart. A process that exits with 259
/// therefore reads as alive until somebody closes every handle to it, and until
/// its pid no longer resolves. That is the error to accept. An agent counted
/// live one poll too long leaves a stale row. An agent counted dead while it
/// works makes a row vanish under someone.
fn still_running(exit_code: u32) -> bool {
    exit_code == STILL_ACTIVE as u32
}

/// The start time of a process, or `None` for a process that answers no
/// question.
///
/// Side effect: opens the process for the length of the call. The creation time
/// comes back as a `FILETIME`, which is a count of hundred-nanosecond intervals
/// across two words. This folds the two words back into the one number that
/// they stand for, and keeps those units. Nothing reads the figure. The only
/// use of it is a comparison with another reading of the same process.
pub fn started(pid: u32) -> Option<ProcessStartStamp> {
    if pid == 0 {
        return None;
    }
    // SAFETY: as in `pid_alive`, this only reads kernel state, and returns
    // either a handle that this owns or null. The rights are the narrowest
    // rights that answer the question.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if process.is_null() {
        return None;
    }

    let mut creation = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `process` is a live handle that this opened above, and all four
    // out parameters are records that this owns and that outlive the call. The
    // API writes every one of them, so this passes all four although it wants
    // one.
    let read =
        unsafe { GetProcessTimes(process, &mut creation, &mut exited, &mut kernel, &mut user) };
    // SAFETY: `process` came from `OpenProcess` above, and nothing uses it
    // again.
    unsafe { CloseHandle(process) };

    match read == FALSE {
        true => None,
        false => Some(ProcessStartStamp(moment(creation))),
    }
}

/// The one number that the two words of a `FILETIME` stand for.
fn moment(time: FILETIME) -> u64 {
    ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
}

/// Every process, its parent and the image that it runs, as one snapshot.
///
/// Side effect: takes a ToolHelp snapshot, which walks the whole process list at
/// the moment of the call. Each record carries both the parent and the image.
/// One walk therefore answers both questions, and no moment separates the two
/// answers.
///
/// The name comes back as the system gave it. Here that is a bare file name
/// with its extension still on it, `claude.exe` and not `claude`. This strips
/// nothing, because the caller decides what counts as a match.
///
/// When the parent dies, a unix parent id changes. The id here does not. An
/// orphan keeps the pid that started it, and gets no new parent. Windows also
/// uses pids again, so a long climb can arrive at a process that only inherited
/// the number. A climb by name over a bounded number of hops makes that
/// harmless.
pub fn processes() -> HashMap<u32, ProcessTableRow> {
    // SAFETY: `TH32CS_SNAPPROCESS` with pid 0 asks for the process list. That
    // call takes no buffer from this, and returns either a handle that this
    // owns or `INVALID_HANDLE_VALUE`.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return HashMap::new();
    }

    let mut table = HashMap::new();
    // If the size is not filled in, the walk refuses to start. The API reads
    // the size to tell which version of the record it received.
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    // SAFETY: `snapshot` is a live snapshot handle. This owns `entry`, `entry`
    // outlives the call, and `entry` holds the `dwSize` that the API reads to
    // size its write.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != FALSE {
        table.insert(
            entry.th32ProcessID,
            ProcessTableRow {
                ppid: entry.th32ParentProcessID,
                name: image_name(&entry.szExeFile),
            },
        );
        // SAFETY: the same handle and the same owned record. The previous call
        // left its `dwSize` intact.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }

    // SAFETY: `snapshot` came from `CreateToolhelp32Snapshot`, and the walk
    // over it ended.
    unsafe { CloseHandle(snapshot) };
    table
}

/// Reads the image name out of the fixed-width field of a snapshot record.
///
/// The field is a NUL-terminated wide string in an array that always has its
/// full width. The name therefore ends at the first NUL and not at the end of
/// the buffer. This takes a field with no NUL at all whole, and does not drop
/// it. The decode is lossy, because Windows permits a file name that is not
/// well-formed UTF-16. A row with an unpaired surrogate in it is still a row
/// worth a record. It keeps its pid and its parent, which is most of what the
/// climb wants.
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
        // The neighbors of 259 are ordinary exit codes and must read as exited.
        // That is the reason for a comparison with the constant and not with a
        // range.
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
        // The value of the number does not matter. The number is the same
        // number at every reading, which is all that a difference between one
        // process and another needs of it.
        let mine = started(std::process::id()).expect("this process has a start time");
        assert_eq!(started(std::process::id()), Some(mine));
        // Pid 0 is the idle process, and an agent is never the idle process.
        assert_eq!(started(0), None);
    }

    /// A field as the snapshot leaves it: the name, a NUL, and then whatever
    /// the buffer already held.
    fn field(name: &str, trailing: &[u16]) -> Vec<u16> {
        let mut raw: Vec<u16> = name.encode_utf16().collect();
        raw.push(0);
        raw.extend_from_slice(trailing);
        raw
    }

    #[test]
    fn a_name_ends_at_the_first_nul_and_not_at_the_end_of_the_field() {
        // Nothing clears the bytes past the NUL. A read of those bytes turns
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
        // Nothing can decode an unpaired surrogate. A loss of the whole row
        // over it is a loss of the parent id that the climb needs.
        let raw = field("cla\u{FFFF}", &[0xD800, 0]);
        assert!(!image_name(&raw).is_empty());
    }
}
