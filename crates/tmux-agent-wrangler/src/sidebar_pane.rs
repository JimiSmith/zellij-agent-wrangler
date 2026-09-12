//! Pane-local ownership for native sidebars.

use crate::tmux_location::{TmuxLocation, TmuxPaneId, TMUX_PROGRAM};
use crate::FatalError;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

/// The pane-local user option reserved for native sidebar ownership.
pub const SIDEBAR_OPTION: &str = "@agent-wrangler-sidebar";

/// Return only panes with a matching, currently existing non-zombie owner.
/// Each call reads a new OS process snapshot and batches distinct process IDs.
pub fn live_sidebar_panes<'a>(
    markers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> BTreeSet<String> {
    live_panes_with(markers, read_process_identities)
}

fn read_process_identities(pids: &[u32]) -> BTreeMap<u32, ProcessIdentity> {
    // A new snapshot cannot retain a start time from an earlier use of a PID.
    let mut system = System::new();
    let requested: Vec<_> = pids.iter().copied().map(Pid::from_u32).collect();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&requested),
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    let identities: BTreeMap<_, _> = requested
        .into_iter()
        .filter_map(|pid| {
            let process = system.process(pid)?;
            Some((
                pid.as_u32(),
                ProcessIdentity {
                    start: process.start_time(),
                    status: process.status(),
                    exists: process.exists(),
                },
            ))
        })
        .collect();
    if pids.iter().any(|pid| {
        identities
            .get(pid)
            .is_none_or(|identity| identity.start == 0)
    }) {
        report_unclassifiable_peer();
    }
    identities
}

fn report_unclassifiable_peer() {
    // Sysinfo cannot distinguish an absent peer from an unreadable peer on every
    // system. Report this limitation once, without caching any process identity.
    static REPORTED: std::sync::Once = std::sync::Once::new();
    REPORTED.call_once(|| {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "tmux-agent-wrangler: sidebar peer identity unavailable; the process may be absent or unreadable. Unverified markers do not hide panes.");
    });
}

#[derive(Clone, Copy)]
struct ProcessIdentity {
    start: u64,
    status: ProcessStatus,
    exists: bool,
}

fn live_panes_with<'a>(
    markers: impl IntoIterator<Item = (&'a str, &'a str)>,
    read_processes: impl FnOnce(&[u32]) -> BTreeMap<u32, ProcessIdentity>,
) -> BTreeSet<String> {
    let markers: Vec<_> = markers
        .into_iter()
        .filter_map(|(pane, text)| {
            let marker = SidebarMarker::parse(text)?;
            (marker.pane_id() == pane).then_some(marker)
        })
        .collect();
    let pids: Vec<_> = markers
        .iter()
        .map(|marker| marker.pid)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let identities = read_processes(&pids);
    markers
        .into_iter()
        .filter(|marker| {
            identities.get(&marker.pid).is_some_and(|identity| {
                identity.exists
                    && identity.start == marker.process_start
                    && identity.status != ProcessStatus::Zombie
            })
        })
        .map(|marker| marker.pane_id().to_string())
        .collect()
}

/// A validated v1 marker. Keep the original text for compare-and-set commands.
/// `TmuxPaneId` and decimal validation exclude tmux command delimiters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarMarker {
    pane: TmuxPaneId,
    value: String,
    pid: u32,
    process_start: u64,
}

impl SidebarMarker {
    /// Reject unknown versions, non-ASCII digits, zero identities and overflows.
    pub fn parse(text: &str) -> Option<Self> {
        let mut fields = text.split(':');
        if fields.next()? != "v1" {
            return None;
        }
        let pane = TmuxPaneId::new(fields.next()?)?;
        let pid = decimal::<u32>(fields.next()?, 10)?;
        let process_start = decimal::<u64>(fields.next()?, 20)?;
        let startup_token = decimal::<u128>(fields.next()?, 39)?;
        if fields.next().is_some() || pid == 0 || process_start == 0 || startup_token == 0 {
            return None;
        }
        Some(Self {
            pane,
            value: text.to_string(),
            pid,
            process_start,
        })
    }

    /// The exact pane target held by this marker.
    pub fn pane_id(&self) -> &str {
        self.pane.as_str()
    }

    /// The accepted marker text without decimal normalization.
    pub fn value(&self) -> &str {
        &self.value
    }
}

fn decimal<T: std::str::FromStr>(text: &str, max_digits: usize) -> Option<T> {
    if text.is_empty() || text.len() > max_digits || !text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    text.parse().ok()
}

fn current_marker(pane: &TmuxPaneId) -> Result<SidebarMarker, FatalError> {
    let pid = std::process::id();
    let identities = read_process_identities(&[pid]);
    let identity = identities
        .get(&pid)
        .filter(|identity| {
            identity.exists && identity.start != 0 && identity.status != ProcessStatus::Zombie
        })
        .ok_or_else(|| {
            FatalError::SidebarPane(
                "cannot read this process's OS start time; no pane option was changed".to_string(),
            )
        })?;
    let token = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|why| FatalError::SidebarPane(format!("cannot create a startup token: {why}")))?
        .as_nanos();
    SidebarMarker::parse(&format!(
        "v1:{}:{pid}:{}:{token}",
        pane.as_str(),
        identity.start
    ))
    .ok_or_else(|| {
        FatalError::SidebarPane(
            "cannot encode a nonzero process identity and startup token".to_string(),
        )
    })
}

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const REAP_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_OUTPUT_BYTES: usize = 8192;

fn run_bounded_command(command: &mut Command, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|why| format!("cannot start program: {why}"))?;
    let result = (|| {
        let stdout = spawn_output_reader(child.stdout.take().ok_or("no stdout pipe")?)?;
        let stderr = spawn_output_reader(child.stderr.take().ok_or("no stderr pipe")?)?;
        let status = loop {
            if Instant::now() >= deadline {
                return Err("tmux command timed out".to_string());
            }
            if let Some(status) = child.try_wait().map_err(|why| why.to_string())? {
                break status;
            }
            std::thread::sleep(
                COMMAND_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        };
        let output = receive_output(stdout, deadline)?;
        let errors = receive_output(stderr, deadline)?;
        if !status.success() {
            return Err(format!(
                "tmux command exited with {status}: {}",
                errors.trim()
            ));
        }
        Ok(output)
    })();
    if result.is_err() {
        stop_and_reap(child);
    }
    result
}

fn spawn_output_reader(
    reader: impl Read + Send + 'static,
) -> Result<mpsc::Receiver<Result<String, String>>, String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("sidebar-option-output".to_string())
        .spawn(move || {
            let _ = sender.send(read_bounded_output(reader));
        })
        .map_err(|why| format!("cannot read command output: {why}"))?;
    Ok(receiver)
}

fn read_bounded_output(reader: impl Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|why| format!("cannot read command output: {why}"))?;
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err("tmux command output exceeded its byte limit".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "tmux command output is not UTF-8".to_string())
}

fn receive_output(
    receiver: mpsc::Receiver<Result<String, String>>,
    deadline: Instant,
) -> Result<String, String> {
    // A descendant can retain a pipe after the command exits. Never join a
    // reader or wait for its EOF past the command's deadline.
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "tmux command output timed out or its reader stopped".to_string())?
}

fn stop_and_reap(mut child: Child) {
    let _ = child.kill();
    let deadline = Instant::now() + REAP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Err(_) => break,
            Ok(None) if Instant::now() >= deadline => break,
            Ok(None) => std::thread::sleep(COMMAND_POLL_INTERVAL),
        }
    }
    // A process in an uninterruptible OS wait can outlast the kill request.
    // Leave reaping to a thread instead of blocking the registration's Drop.
    let _ = std::thread::Builder::new()
        .name("sidebar-option-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
}

struct TmuxCommandRunner;

impl PaneCommandRunner for TmuxCommandRunner {
    fn run(&self, arguments: &[String]) -> Result<String, String> {
        run_bounded_command(Command::new(TMUX_PROGRAM).args(arguments), COMMAND_TIMEOUT)
    }
}

trait PaneCommandRunner: Send + Sync {
    fn run(&self, arguments: &[String]) -> Result<String, String>;
}

/// Own one pane option. Drop removes it only if its exact value is still ours.
/// Cleanup is best effort and each tmux command has a bounded wait.
pub struct SidebarPaneRegistration {
    location: TmuxLocation,
    marker: SidebarMarker,
    runner: Arc<dyn PaneCommandRunner>,
}

impl SidebarPaneRegistration {
    /// Claim the captured server and pane before the caller takes the terminal.
    /// Refuse live owners, unsafe reserved values and missing tmux capabilities.
    pub fn acquire(location: &TmuxLocation) -> Result<Self, FatalError> {
        let marker = current_marker(location.pane_id())?;
        Self::acquire_with(location, marker, Arc::new(TmuxCommandRunner), |previous| {
            live_sidebar_panes([(previous.pane_id(), previous.value())])
                .contains(previous.pane_id())
        })
    }

    pub fn value(&self) -> &str {
        self.marker.value()
    }

    fn acquire_with(
        location: &TmuxLocation,
        marker: SidebarMarker,
        runner: Arc<dyn PaneCommandRunner>,
        is_live: impl FnOnce(&SidebarMarker) -> bool,
    ) -> Result<Self, FatalError> {
        let before = read_pane_option(location, runner.as_ref())?;
        let previous = before.validated_previous(location.pane_id(), is_live)?;
        // The guard exists before the write, including writes whose reply is lost.
        let registration = Self {
            location: location.clone(),
            marker,
            runner,
        };
        let pane = location.pane_id().as_str();
        let action = format!(
            "set-option -p -t {pane} {SIDEBAR_OPTION} {}",
            registration.value()
        );
        let arguments = conditional_arguments(location, previous.as_ref(), &action);
        registration
            .runner
            .run(&arguments)
            .map_err(|why| capability_error("conditional pane option write", &why))?;
        let after = read_pane_option(location, registration.runner.as_ref())?;
        if after.local.as_deref() != Some(registration.value())
            || after.effective != registration.value()
        {
            return Err(capability_error(
                "pane-local readback",
                "the marker changed or pane-local options were not retained",
            ));
        }
        Ok(registration)
    }
}

impl Drop for SidebarPaneRegistration {
    fn drop(&mut self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let pane = self.location.pane_id().as_str();
            let action = format!("set-option -pu -t {pane} {SIDEBAR_OPTION}");
            let arguments = conditional_arguments(&self.location, Some(&self.marker), &action);
            let _ = self.runner.run(&arguments);
        }));
    }
}

struct PaneOptionSnapshot {
    local: Option<String>,
    effective: String,
}

impl PaneOptionSnapshot {
    fn validated_previous(
        &self,
        pane: &TmuxPaneId,
        is_live: impl FnOnce(&SidebarMarker) -> bool,
    ) -> Result<Option<SidebarMarker>, FatalError> {
        if self.local.is_none() && self.effective.is_empty() {
            return Ok(None);
        }
        let previous = SidebarMarker::parse(&self.effective).ok_or_else(||
            capability_error("reserved pane option", "an unknown or malformed value occupies @agent-wrangler-sidebar; it was not changed"))?;
        if self.local.is_some() && previous.pane_id() != pane.as_str() {
            return Err(capability_error(
                "reserved pane option",
                "a local marker names another pane; it was not changed",
            ));
        }
        if previous.pane_id() == pane.as_str() && is_live(&previous) {
            return Err(capability_error(
                "pane ownership",
                "a live sidebar already owns this pane",
            ));
        }
        Ok(Some(previous))
    }
}

fn capability_error(operation: &str, reason: &str) -> FatalError {
    FatalError::SidebarPane(format!(
        "{operation} failed: {reason}. This sidebar requires pane-local user options, explicit pane targets and if-shell -F."
    ))
}

fn targeted_arguments(location: &TmuxLocation, command: &str, flags: &str) -> Vec<String> {
    [
        "-S",
        location.server_socket(),
        command,
        flags,
        "-t",
        location.pane_id().as_str(),
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn conditional_arguments(
    location: &TmuxLocation,
    previous: Option<&SidebarMarker>,
    action: &str,
) -> Vec<String> {
    // Marker validation excludes every tmux format or command delimiter.
    let expected = previous.map_or("", SidebarMarker::value);
    let mut arguments = targeted_arguments(location, "if-shell", "-F");
    arguments.push(format!("#{{==:#{{{SIDEBAR_OPTION}}},{expected}}}"));
    arguments.push(action.to_string());
    arguments
}

fn output_line(output: &str) -> Result<&str, FatalError> {
    let text = output
        .strip_suffix("\r\n")
        .or_else(|| output.strip_suffix('\n'))
        .unwrap_or(output);
    if text.contains(['\r', '\n', '\0']) {
        return Err(capability_error(
            "pane option read",
            "tmux returned more than one line or a control character",
        ));
    }
    Ok(text)
}

fn read_pane_option(
    location: &TmuxLocation,
    runner: &dyn PaneCommandRunner,
) -> Result<PaneOptionSnapshot, FatalError> {
    let mut local_arguments = targeted_arguments(location, "show-option", "-pqv");
    local_arguments.push(SIDEBAR_OPTION.to_string());
    let local_output = runner
        .run(&local_arguments)
        .map_err(|why| capability_error("local pane option read", &why))?;
    // An unset option prints no bytes. A local empty value prints a newline.
    let local = if local_output.is_empty() {
        None
    } else {
        Some(output_line(&local_output)?.to_string())
    };
    let mut format_arguments = targeted_arguments(location, "display-message", "-p");
    format_arguments.push(format!("#{{pane_id}}\t#{{{SIDEBAR_OPTION}}}"));
    let formatted_output = runner
        .run(&format_arguments)
        .map_err(|why| capability_error("targeted pane format read", &why))?;
    let (pane, effective) = output_line(&formatted_output)?
        .split_once('\t')
        .ok_or_else(|| {
            capability_error(
                "targeted pane format read",
                "tmux did not return a pane id and option value",
            )
        })?;
    if pane != location.pane_id().as_str()
        || local.as_deref().is_some_and(|value| value != effective)
    {
        return Err(capability_error(
            "targeted pane format read",
            "the target or its local option disagreed with the format result",
        ));
    }
    Ok(PaneOptionSnapshot {
        local,
        effective: effective.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct ScriptedRunner {
        replies: Mutex<VecDeque<Result<String, String>>>,
        commands: Mutex<Vec<Vec<String>>>,
    }

    impl ScriptedRunner {
        fn new(replies: impl IntoIterator<Item = Result<String, String>>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into_iter().collect()),
                commands: Mutex::new(Vec::new()),
            })
        }
    }

    impl PaneCommandRunner for ScriptedRunner {
        fn run(&self, arguments: &[String]) -> Result<String, String> {
            self.commands.lock().unwrap().push(arguments.to_vec());
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted command")
        }
    }

    fn location() -> TmuxLocation {
        TmuxLocation::from_variables(|name| match name {
            "TMUX" => Some("captured-socket,123,0".to_string()),
            "TMUX_PANE" => Some("%2".to_string()),
            _ => None,
        })
        .unwrap()
    }

    fn marker() -> SidebarMarker {
        SidebarMarker::parse("v1:%2:42:123:7").unwrap()
    }

    #[test]
    fn acquisition_reads_back_a_local_marker_on_the_explicit_pane() {
        let runner = ScriptedRunner::new(
            [
                "",
                "%2\t\n",
                "",
                "v1:%2:42:123:7\n",
                "%2\tv1:%2:42:123:7\n",
                "",
            ]
            .map(|text| Ok(text.to_string())),
        );
        let registration =
            SidebarPaneRegistration::acquire_with(&location(), marker(), runner.clone(), |_| false)
                .expect("acquire an empty pane");
        assert_eq!(registration.value(), "v1:%2:42:123:7");
        let commands = runner.commands.lock().unwrap().clone();
        assert_eq!(commands.len(), 5);
        assert_eq!(
            commands[0],
            [
                "-S",
                "captured-socket",
                "show-option",
                "-pqv",
                "-t",
                "%2",
                SIDEBAR_OPTION
            ]
        );
        assert_eq!(
            commands[1],
            [
                "-S",
                "captured-socket",
                "display-message",
                "-p",
                "-t",
                "%2",
                "#{pane_id}\t#{@agent-wrangler-sidebar}"
            ]
        );
        assert_eq!(
            commands[2],
            [
                "-S",
                "captured-socket",
                "if-shell",
                "-F",
                "-t",
                "%2",
                "#{==:#{@agent-wrangler-sidebar},}",
                "set-option -p -t %2 @agent-wrangler-sidebar v1:%2:42:123:7"
            ]
        );
        assert_eq!(commands[3], commands[0]);
        assert_eq!(commands[4], commands[1]);
        drop(registration);
    }

    #[test]
    fn cleanup_compares_the_exact_owner_before_unsetting_only_its_pane() {
        let runner = ScriptedRunner::new([Err("server vanished".to_string())]);
        let registration = SidebarPaneRegistration {
            location: location(),
            marker: marker(),
            runner: runner.clone(),
        };
        drop(registration);
        let commands = runner.commands.lock().unwrap();
        assert_eq!(
            commands.as_slice(),
            [vec![
                "-S",
                "captured-socket",
                "if-shell",
                "-F",
                "-t",
                "%2",
                "#{==:#{@agent-wrangler-sidebar},v1:%2:42:123:7}",
                "set-option -pu -t %2 @agent-wrangler-sidebar"
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()]
        );
    }

    #[test]
    fn a_write_failure_still_attempts_owner_safe_cleanup() {
        let runner = ScriptedRunner::new([
            Ok(String::new()),
            Ok("%2\t\n".to_string()),
            Err("reply lost after write".to_string()),
            Ok(String::new()),
        ]);
        assert!(SidebarPaneRegistration::acquire_with(
            &location(),
            marker(),
            runner.clone(),
            |_| false
        )
        .is_err());
        let commands = runner.commands.lock().unwrap();
        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands[3][6],
            "#{==:#{@agent-wrangler-sidebar},v1:%2:42:123:7}"
        );
        assert_eq!(
            commands[3][7],
            "set-option -pu -t %2 @agent-wrangler-sidebar"
        );
    }

    #[test]
    fn the_command_runner_collects_output_without_a_shell() {
        let answer = run_bounded_command(
            std::process::Command::new("rustc").arg("--version"),
            std::time::Duration::from_secs(2),
        )
        .unwrap();
        assert!(answer.starts_with("rustc "), "{answer:?}");
    }

    #[test]
    fn an_own_marker_uses_the_os_start_time_and_a_startup_token() {
        let own = current_marker(location().pane_id()).unwrap();
        assert_eq!(own.pid, std::process::id());
        assert_eq!(own.pane_id(), "%2");
        assert_eq!(
            read_process_identities(&[own.pid])[&own.pid].start,
            own.process_start
        );
        assert_ne!(own.process_start, 0);
        let token: u128 = own.value().rsplit(':').next().unwrap().parse().unwrap();
        assert_ne!(token, 0);
    }

    #[test]
    fn the_public_filter_reads_the_current_os_identity() {
        let pid = Pid::from_u32(std::process::id());
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let process = system.process(pid).expect("own process is readable");
        let text = format!("v1:%2:{}:{}:7", pid.as_u32(), process.start_time());
        assert_eq!(
            live_sidebar_panes([("%2", text.as_str())]),
            BTreeSet::from(["%2".to_string()])
        );
    }

    #[test]
    fn a_live_identity_keeps_sleeping_stopped_and_debugged_owners() {
        for status in [
            ProcessStatus::Run,
            ProcessStatus::Sleep,
            ProcessStatus::Stop,
            ProcessStatus::Tracing,
            ProcessStatus::Dead,
            ProcessStatus::Unknown(0),
        ] {
            let panes = live_panes_with([("%2", "v1:%2:42:123:7")], |pids| {
                assert_eq!(pids, [42]);
                BTreeMap::from([(
                    42,
                    ProcessIdentity {
                        start: 123,
                        status,
                        exists: true,
                    },
                )])
            });
            assert_eq!(panes, BTreeSet::from(["%2".to_string()]), "{status:?}");
        }
    }

    #[test]
    fn schema_rejects_unsafe_unknown_zero_and_out_of_range_fields() {
        for text in [
            "",
            "v2:%2:42:123:7",
            "v1:%2:42:123",
            "v1:%2:42:123:7:8",
            "v1:2:42:123:7",
            "v1:%:42:123:7",
            "v1:%4294967296:42:123:7",
            "v1:%00000000002:42:123:7",
            "v1:%2:0:123:7",
            "v1:%2:42:0:7",
            "v1:%2:42:123:0",
            "v1:%2:4294967296:123:7",
            "v1:%2:00000000042:123:7",
            "v1:%2:42:18446744073709551616:7",
            "v1:%2:42:000000000000000000123:7",
            "v1:%2:42:123:340282366920938463463374607431768211456",
            "v1:%2:42:123:0000000000000000000000000000000000000007",
            "v1:%2:+42:123:7",
            "v1:%2:-42:123:7",
            "v1:%2:４２:123:7",
            "v1:%2:42:12.3:7",
            "v1:%2:42:123:7\n",
            " v1:%2:42:123:7",
            "v1:%2:42:123:7 ",
            "v1:%2:42:123:7\0",
            "v1:%2:42:123:7\r",
            "v1:%2:42:123:7;kill-server",
            "v1:%2:42:123:#{pid}",
            "v1:%2:42:123:7,1",
            "v1:%2:42:123:'7'",
            "v1:%2:42:123:\t7",
        ] {
            assert!(SidebarMarker::parse(text).is_none(), "accepted {text:?}");
        }
        assert!(SidebarMarker::parse("v1:%4294967295:4294967295:18446744073709551615:340282366920938463463374607431768211455").is_some());
        assert!(SidebarMarker::parse("v1:%0:1:1:1").is_some());
    }

    #[test]
    fn absent_zombie_exited_and_reused_processes_do_not_hide_panes() {
        for identity in [
            None,
            Some(ProcessIdentity {
                start: 123,
                status: ProcessStatus::Zombie,
                exists: true,
            }),
            Some(ProcessIdentity {
                start: 123,
                status: ProcessStatus::Run,
                exists: false,
            }),
            Some(ProcessIdentity {
                start: 124,
                status: ProcessStatus::Run,
                exists: true,
            }),
            Some(ProcessIdentity {
                start: 0,
                status: ProcessStatus::Run,
                exists: true,
            }),
        ] {
            let panes = live_panes_with([("%2", marker().value())], |_| {
                identity
                    .map(|identity| BTreeMap::from([(42, identity)]))
                    .unwrap_or_default()
            });
            assert!(panes.is_empty());
        }
    }

    #[test]
    fn filtering_batches_distinct_pids_only_for_valid_correct_pane_markers() {
        let panes = live_panes_with(
            [
                ("%2", "v1:%2:42:123:7"),
                ("%2", "v1:%2:42:123:7"),
                ("%3", "v1:%3:42:123:8"),
                ("%4", "v1:%4:43:123:9"),
                ("%5", "v1:%2:99:123:7"),
                ("%6", "unknown"),
            ],
            |pids| {
                assert_eq!(pids, [42, 43]);
                BTreeMap::from([(
                    42,
                    ProcessIdentity {
                        start: 123,
                        status: ProcessStatus::Run,
                        exists: true,
                    },
                )])
            },
        );
        assert_eq!(panes, BTreeSet::from(["%2".to_string(), "%3".to_string()]));
    }

    #[test]
    fn a_live_local_owner_prevents_every_write() {
        let runner = ScriptedRunner::new([
            Ok(format!("{}\n", marker().value())),
            Ok(format!("%2\t{}\n", marker().value())),
        ]);
        let result =
            SidebarPaneRegistration::acquire_with(&location(), marker(), runner.clone(), |_| true);
        assert!(
            matches!(result, Err(FatalError::SidebarPane(reason)) if reason.contains("live sidebar"))
        );
        assert_eq!(runner.commands.lock().unwrap().len(), 2);
    }

    #[test]
    fn unsafe_or_empty_reserved_local_values_are_not_clobbered() {
        for value in [
            "",
            "unknown",
            "v2:%2:42:123:7",
            "v1:%3:42:123:7",
            "#{pane_id};kill-server",
        ] {
            let runner =
                ScriptedRunner::new([Ok(format!("{value}\n")), Ok(format!("%2\t{value}\n"))]);
            assert!(SidebarPaneRegistration::acquire_with(
                &location(),
                marker(),
                runner.clone(),
                |_| false
            )
            .is_err());
            assert_eq!(runner.commands.lock().unwrap().len(), 2, "{value}");
        }
    }

    #[test]
    fn dead_local_and_safe_inherited_markers_use_exact_compare_and_set() {
        for (local, previous, live) in [
            (true, "v1:%2:00042:00123:0009", false),
            (false, "v1:%3:00042:00123:0009", true),
        ] {
            let runner = ScriptedRunner::new([
                Ok(if local {
                    format!("{previous}\n")
                } else {
                    String::new()
                }),
                Ok(format!("%2\t{previous}\n")),
                Ok(String::new()),
                Ok(format!("{}\n", marker().value())),
                Ok(format!("%2\t{}\n", marker().value())),
                Ok(String::new()),
            ]);
            let registration = SidebarPaneRegistration::acquire_with(
                &location(),
                marker(),
                runner.clone(),
                |_| live,
            )
            .unwrap();
            assert_eq!(
                runner.commands.lock().unwrap()[2][6],
                format!("#{{==:#{{@agent-wrangler-sidebar}},{previous}}}")
            );
            drop(registration);
        }
    }

    #[test]
    fn unsupported_or_wrong_target_reads_fail_without_mutation() {
        for replies in [
            vec![Err("unknown flag -p".to_string())],
            vec![Ok(String::new()), Err("format unavailable".to_string())],
            vec![Ok(String::new()), Ok("%3\t\n".to_string())],
            vec![Ok(String::new()), Ok("\n".to_string())],
            vec![Ok("one\nline\n".to_string())],
            vec![
                Ok(format!("{}\n", marker().value())),
                Ok("%2\tother\n".to_string()),
            ],
            vec![Ok(String::new()), Ok("%2\t#{bad}\n".to_string())],
        ] {
            let expected_commands = replies.len();
            let runner = ScriptedRunner::new(replies);
            let result = SidebarPaneRegistration::acquire_with(
                &location(),
                marker(),
                runner.clone(),
                |_| false,
            );
            assert!(
                matches!(result, Err(FatalError::SidebarPane(reason)) if reason.contains("requires pane-local"))
            );
            assert_eq!(runner.commands.lock().unwrap().len(), expected_commands);
        }
    }

    #[test]
    fn every_partial_startup_failure_attempts_conditional_cleanup() {
        for after_write in [
            vec![Err("local read timed out".to_string())],
            vec![
                Ok(format!("{}\n", marker().value())),
                Err("format read failed".to_string()),
            ],
            vec![Ok(String::new()), Ok(format!("%2\t{}\n", marker().value()))],
            vec![
                Ok("v1:%2:43:124:8\n".to_string()),
                Ok("%2\tv1:%2:43:124:8\n".to_string()),
            ],
            vec![
                Ok(format!("{}\n", marker().value())),
                Ok(format!("%3\t{}\n", marker().value())),
            ],
        ] {
            let mut replies = vec![
                Ok(String::new()),
                Ok("%2\t\n".to_string()),
                Ok(String::new()),
            ];
            replies.extend(after_write);
            replies.push(Ok(String::new()));
            let expected_commands = replies.len();
            let runner = ScriptedRunner::new(replies);
            assert!(SidebarPaneRegistration::acquire_with(
                &location(),
                marker(),
                runner.clone(),
                |_| false
            )
            .is_err());
            let commands = runner.commands.lock().unwrap();
            assert_eq!(commands.len(), expected_commands);
            assert_eq!(
                commands.last().unwrap()[6],
                "#{==:#{@agent-wrangler-sidebar},v1:%2:42:123:7}"
            );
            assert_eq!(
                commands.last().unwrap()[7],
                "set-option -pu -t %2 @agent-wrangler-sidebar"
            );
        }
    }

    #[test]
    fn output_readers_enforce_byte_and_time_limits() {
        assert_eq!(
            read_bounded_output(std::io::Cursor::new(b"ok\n")).unwrap(),
            "ok\n"
        );
        assert!(read_bounded_output(std::io::repeat(b'x'))
            .unwrap_err()
            .contains("byte limit"));
        assert!(read_bounded_output(std::io::Cursor::new([255]))
            .unwrap_err()
            .contains("UTF-8"));
        let (_sender, receiver) = mpsc::channel();
        assert!(receive_output(receiver, Instant::now())
            .unwrap_err()
            .contains("timed out"));
    }

    #[test]
    fn a_command_timeout_kills_and_reaps_without_waiting_for_output() {
        let start = Instant::now();
        let error = run_bounded_command(Command::new("rustc").arg("--version"), Duration::ZERO)
            .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(start.elapsed() < COMMAND_TIMEOUT);
    }

    #[test]
    fn command_failures_return_status_and_bounded_stderr() {
        let error = run_bounded_command(
            Command::new("rustc").arg("--invalid-sidebar-test-option"),
            COMMAND_TIMEOUT,
        )
        .unwrap_err();
        assert!(error.contains("exited with"), "{error}");
        assert!(error.contains("invalid-sidebar-test-option"), "{error}");
    }

    #[test]
    fn an_old_guard_does_not_clear_a_newer_startup_token() {
        struct ConditionalOption(Mutex<Option<String>>);
        impl PaneCommandRunner for ConditionalOption {
            fn run(&self, arguments: &[String]) -> Result<String, String> {
                assert_eq!(
                    &arguments[..6],
                    ["-S", "captured-socket", "if-shell", "-F", "-t", "%2"]
                );
                assert_eq!(arguments[7], "set-option -pu -t %2 @agent-wrangler-sidebar");
                let mut value = self.0.lock().unwrap();
                let condition = format!(
                    "#{{==:#{{@agent-wrangler-sidebar}},{}}}",
                    value.as_deref().unwrap_or("")
                );
                if arguments[6] == condition {
                    *value = None;
                }
                Ok(String::new())
            }
        }
        let newer = SidebarMarker::parse("v1:%2:42:123:8").unwrap();
        let runner = Arc::new(ConditionalOption(Mutex::new(Some(
            newer.value().to_string(),
        ))));
        drop(SidebarPaneRegistration {
            location: location(),
            marker: marker(),
            runner: runner.clone(),
        });
        assert_eq!(runner.0.lock().unwrap().as_deref(), Some(newer.value()));
        drop(SidebarPaneRegistration {
            location: location(),
            marker: newer,
            runner: runner.clone(),
        });
        assert!(runner.0.lock().unwrap().is_none());
    }

    #[test]
    fn a_captured_named_pipe_is_preserved_as_one_server_argument() {
        let location = TmuxLocation::from_variables(|name| match name {
            "TMUX" => Some(r"\\.\pipe\psmux named server,123,0".to_string()),
            "TMUX_PANE" => Some("%2".to_string()),
            _ => None,
        })
        .unwrap();
        let arguments = targeted_arguments(&location, "show-option", "-pqv");
        assert_eq!(arguments[1], r"\\.\pipe\psmux named server");
        assert_eq!(arguments[5], "%2");
    }

    #[test]
    #[ignore = "requires TMUX and TMUX_PANE for an isolated wrangler-test pane"]
    fn real_tmux_registration_preserves_a_newer_owner() {
        let location = TmuxLocation::from_environment().unwrap();
        assert!(location.server_socket().ends_with("wrangler-test"));
        let runner = Arc::new(TmuxCommandRunner);
        assert!(read_pane_option(&location, runner.as_ref())
            .unwrap()
            .local
            .is_none());
        let guard = SidebarPaneRegistration::acquire(&location).unwrap();
        assert!(
            live_sidebar_panes([(location.pane_id().as_str(), guard.value())])
                .contains(location.pane_id().as_str())
        );
        assert!(SidebarPaneRegistration::acquire(&location).is_err());
        drop(guard);
        assert!(read_pane_option(&location, runner.as_ref())
            .unwrap()
            .local
            .is_none());

        let guard = SidebarPaneRegistration::acquire(&location).unwrap();
        let newer = SidebarMarker::parse(&format!("{}1", guard.value())).unwrap();
        let mut replace = targeted_arguments(&location, "set-option", "-p");
        replace.extend([SIDEBAR_OPTION.to_string(), newer.value().to_string()]);
        runner.run(&replace).unwrap();
        assert_eq!(
            read_pane_option(&location, runner.as_ref())
                .unwrap()
                .local
                .as_deref(),
            Some(newer.value())
        );
        drop(guard);
        assert_eq!(
            read_pane_option(&location, runner.as_ref())
                .unwrap()
                .local
                .as_deref(),
            Some(newer.value())
        );
        drop(SidebarPaneRegistration {
            location: location.clone(),
            marker: newer,
            runner: runner.clone(),
        });
        assert!(read_pane_option(&location, runner.as_ref())
            .unwrap()
            .local
            .is_none());
    }

    #[test]
    fn a_valid_marker_preserves_its_exact_decimal_spelling() {
        let text =
            "v1:%0002:0000000042:00000000000000000123:000000000000000000000000000000000000007";
        let marker = SidebarMarker::parse(text).expect("valid marker");
        assert_eq!(marker.pane_id(), "%0002");
        assert_eq!(marker.value(), text);
    }
}
