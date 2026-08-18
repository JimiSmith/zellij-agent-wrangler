//! The installation, and the removal, of the agent lifecycle hooks that call
//! the hook client.
//!
//! The embedded manifest says which of an agent's events maps to which action.
//! This module writes those events into the agent's own config. It writes the
//! absolute path of the executable that runs, so the hooks work wherever the
//! binary lives. Each agent's `format` selects one of two config shapes:
//!
//! - `claude`: a shared `settings.json` that holds the user's other keys too.
//!   This module touches only its own hook groups. It keeps every other key,
//!   the order of the keys, the permissions of the file, and a backup of the
//!   old content.
//! - `copilot`: a dedicated file, written whole.
//!
//! A second run of install writes identical output, so you can run it as often
//! as you want.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use agent_wrangler_core::command::words;

/// Which of each agent's events call which action. The manifest is embedded, so
/// the installed binary carries it.
const MANIFEST_JSON: &str = include_str!("../hooks-manifest.json");

/// The name of the hook client, which is what identifies this project's hook
/// commands.
const CLIENT: &str = "agent-wrangler";

/// The name of the client's windowless twin on Windows. The hooks written on
/// Windows run this twin.
const WINDOWLESS: &str = "agent-wranglerw";

/// The suffix of the copy that this module takes before it rewrites a shared
/// config.
const BACKUP: &str = ".agent-wrangler.bak";

/// A string quoted for a POSIX shell. If every character is safe, the string
/// does not change. If not, single quotes wrap the string and an escape covers
/// each quote inside it.
fn shell_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".to_string();
    }
    let safe = text.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
            )
    });
    if safe {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\"'\"'"))
    }
}

/// The command that a hook runs: this executable's `hook <agent> <action>`.
fn hook_command(exe: &str, agent: &str, action: &str) -> String {
    format!("{} hook {agent} {action}", shell_quote(exe))
}

/// Whether a program is one of the two that this project installs hooks for.
///
/// Either name counts wherever the code reads the file. Both clients read a
/// config that either one of them wrote. An upgrade that no longer recognizes
/// the work of an earlier version adds its hooks beside those hooks, and not
/// over them.
///
/// The comparison removes a `.exe` and ignores case, because that is how the
/// name arrives on the system that has both. It removes nothing else. Only the
/// extension that Windows puts on the file is not part of the name of the
/// program.
///
/// Both separators end a path here, and not only the separator of the system
/// that runs this code. The code reads a config file that names a path. A test
/// of this function can therefore say what a Windows path does on any system.
fn is_client(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let bare = match name.rsplit_once('.') {
        Some((stem, extension)) if extension.eq_ignore_ascii_case("exe") => stem,
        _ => name,
    };
    bare.eq_ignore_ascii_case(CLIENT) || bare.eq_ignore_ascii_case(WINDOWLESS)
}

/// Whether a hook command is one that this installer owns for `agent`. The
/// installer replaces such a command rather than adds another one beside it.
///
/// The command must run one of this project's clients, with `hook` and that
/// agent as its first two arguments. The test is on the *name of the program
/// that runs* and not on any word in the line. The installer therefore does not
/// claim a command that only mentions a similar name, or that runs a similarly
/// named program from somewhere else.
fn is_ours(command: &str, agent: &str) -> bool {
    let words = words(command);
    let [exe, hook, named, ..] = words.as_slice() else {
        return false;
    };
    if hook != "hook" || named != agent {
        return false;
    }
    is_client(exe)
}

/// The `(matcher, actions)` groups that one manifest event describes. A list of
/// action names is one group that answers to everything. A list of objects is
/// one group for each object.
fn groups(value: &Value) -> Vec<(Option<String>, Vec<String>)> {
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    if array.first().map(Value::is_object).unwrap_or(false) {
        return array
            .iter()
            .map(|group| {
                (
                    group.get("matcher").and_then(Value::as_str).map(Into::into),
                    strings(group.get("actions")),
                )
            })
            .collect();
    }
    vec![(None, strings(Some(value)))]
}

/// The string elements of an optional JSON array.
fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(|item| item.as_str().map(Into::into))
                .collect()
        })
        .unwrap_or_default()
}

/// One of claude's hook groups: `{matcher?, hooks: [{type, command}, ...]}`.
fn claude_group(matcher: Option<String>, actions: &[String], exe: &str, agent: &str) -> Value {
    let hooks: Vec<Value> = actions
        .iter()
        .map(|action| json!({"type": "command", "command": hook_command(exe, agent, action)}))
        .collect();
    let mut group = Map::new();
    if let Some(matcher) = matcher {
        group.insert("matcher".to_string(), json!(matcher));
    }
    group.insert("hooks".to_string(), Value::Array(hooks));
    Value::Object(group)
}

/// Whether one of claude's hook groups holds a command that this installer
/// owns.
fn group_is_ours(group: &Value, agent: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .map(|command| is_ours(command, agent))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Puts this installer's hook groups into a shared settings document, or takes
/// them out again. Every other group and key stays exactly as it was.
///
/// This function is the whole of the merge. It reads no file and writes no
/// file, so its effect on a user's settings is a value.
pub fn merge(settings: &mut Value, agent: &str, events: &Value, exe: &str, uninstall: bool) {
    let Some(document) = settings.as_object_mut() else {
        return;
    };
    let mut hooks: Map<String, Value> = document
        .get("hooks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    for (event, value) in events.as_object().cloned().unwrap_or_default() {
        let mut kept: Vec<Value> = hooks
            .get(&event)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|group| !group_is_ours(group, agent))
            .collect();

        if !uninstall {
            for (matcher, actions) in groups(&value) {
                kept.push(claude_group(matcher, &actions, exe, agent));
            }
        }

        match kept.is_empty() {
            true => hooks.shift_remove(&event),
            false => hooks.insert(event, Value::Array(kept)),
        };
    }

    match hooks.is_empty() {
        true => document.shift_remove("hooks"),
        false => document.insert("hooks".to_string(), Value::Object(hooks)),
    };
}

/// The document that this installer writes whole to the file that copilot
/// reads.
pub fn copilot_document(agent: &str, events: &Value, exe: &str) -> Value {
    let mut hooks = Map::new();
    for (event, value) in events.as_object().cloned().unwrap_or_default() {
        let mut entries = Vec::new();
        for (matcher, actions) in groups(&value) {
            for action in &actions {
                let mut entry = Map::new();
                entry.insert("type".to_string(), json!("command"));
                if let Some(matcher) = &matcher {
                    entry.insert("matcher".to_string(), json!(matcher));
                }
                entry.insert("bash".to_string(), json!(hook_command(exe, agent, action)));
                entries.push(Value::Object(entry));
            }
        }
        hooks.insert(event, Value::Array(entries));
    }
    json!({"version": 1, "hooks": hooks})
}

/// The serialization that this module writes: a two-space indent and a newline
/// at the end.
fn dumps(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// Expands a leading `~` to the user's home, by whichever name this system
/// gives it. If there is no home to expand against, the path stays as it is.
fn expand_user(path: &str) -> PathBuf {
    match path.strip_prefix("~/").zip(crate::paths::home()) {
        Some((rest, home)) => home.join(rest),
        None => PathBuf::from(path),
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Replaces the contents of a file in one step. The function writes a sibling
/// file and renames it over the target, so a failure part way through leaves
/// the original file in place.
///
/// Side effect: the function creates the parent directory. It leaves no
/// temporary file behind on either path.
fn atomic_write(path: &Path, text: &str, mode: u32) -> std::io::Result<()> {
    fs::create_dir_all(parent_dir(path))?;
    let mut name = std::ffi::OsString::from(format!(".{CLIENT}-tmp-{}-", std::process::id()));
    name.push(path.file_name().unwrap_or_default());
    let temp = parent_dir(path).join(name);
    let written = (|| {
        fs::write(&temp, text)?;
        set_mode(&temp, mode)?;
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Gives a file the permissions that the write uses.
///
/// A settings file of this kind holds credentials. On a system with file modes,
/// the code sets the mode explicitly and does not leave it to the umask.
/// Windows has no such mode. A file on Windows inherits the access rules of the
/// directory that it is created in, and those rules are its protection.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// The permissions that a file already has, so that a rewrite keeps them.
/// `None` on a system with no file modes, and for a file that is not there yet.
#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .ok()
        .map(|data| data.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

/// Merges this installer's hooks into the shared settings file that an agent
/// reads.
///
/// Side effect: the function copies the file beside itself before the rewrite,
/// and keeps the permissions that the file already had. The function creates a
/// file that does not exist yet as a private file, because settings of this
/// kind hold credentials.
fn install_shared(agent: &str, spec: &Value, exe: &str, uninstall: bool) -> Result<String, String> {
    let path = expand_user(spec["target"].as_str().unwrap_or_default());
    let mut settings: Value = match fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .map_err(|e| format!("{}: is not valid JSON: {e}", path.display()))?,
        _ => json!({}),
    };
    if !settings.is_object() {
        return Err(format!("{}: is not a JSON object", path.display()));
    }

    merge(&mut settings, agent, &spec["events"], exe, uninstall);

    let (mode, existed) = match file_mode(&path) {
        Some(mode) => (mode, true),
        None => (0o600, false),
    };
    if existed {
        let mut backup = path.clone().into_os_string();
        backup.push(BACKUP);
        fs::copy(&path, PathBuf::from(backup))
            .map_err(|e| format!("{}: could not be backed up: {e}", path.display()))?;
    }
    atomic_write(&path, &dumps(&settings), mode).map_err(|e| format!("{}: {e}", path.display()))?;

    let verb = if uninstall {
        "removed from"
    } else {
        "written to"
    };
    Ok(format!("{agent}: {verb} {}", path.display()))
}

/// Writes, or deletes, the dedicated file that this installer owns for an
/// agent.
fn install_own(agent: &str, spec: &Value, exe: &str, uninstall: bool) -> Result<String, String> {
    let path = expand_user(spec["target"].as_str().unwrap_or_default());
    if uninstall {
        return Ok(match fs::remove_file(&path) {
            Ok(()) => format!("{agent}: removed {}", path.display()),
            Err(_) => format!("{agent}: nothing to remove at {}", path.display()),
        });
    }
    let document = copilot_document(agent, &spec["events"], exe);
    let mode = file_mode(&path).unwrap_or(0o644);
    atomic_write(&path, &dumps(&document), mode).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{agent}: written to {}", path.display()))
}

fn install_agent(agent: &str, spec: &Value, exe: &str, uninstall: bool) -> Result<String, String> {
    match spec.get("format").and_then(Value::as_str) {
        Some("claude") => install_shared(agent, spec, exe, uninstall),
        Some("copilot") => install_own(agent, spec, exe, uninstall),
        other => Err(format!(
            "{agent}: is described as {other:?}, which is not a config this can write"
        )),
    }
}

/// The client that a hook must run, from the path of the client that installs
/// it.
///
/// On Windows that client is the windowless twin beside it, and not the file
/// that runs this code. Windows gives a console of its own to a console program
/// whose parent has no console, and draws a window for it. The program that
/// runs a hook is an agent, and an agent is often exactly such a parent. A hook
/// that names the console client therefore flashes a window up once for each
/// event.
///
/// The path of the twin comes from whichever of the two clients runs, so an
/// install from either one writes the same path. A client without its twin
/// beside it names itself. A hook that flashes still reports what the agent
/// did, and a hook that names a file that is not there reports nothing at all.
#[cfg(windows)]
fn hook_client(exe: PathBuf) -> PathBuf {
    let twin = exe.with_file_name(format!("{WINDOWLESS}.exe"));
    match twin.is_file() {
        true => twin,
        false => exe,
    }
}

/// The client that a hook must run. Off Windows that client is the one that
/// installs it, because there is no console to give and no second client to
/// name.
#[cfg(not(windows))]
fn hook_client(exe: PathBuf) -> PathBuf {
    exe
}

/// The path that the installed hooks will run.
fn exe_path() -> String {
    std::env::current_exe()
        .ok()
        .map(hook_client)
        .and_then(|path| path.to_str().map(Into::into))
        .unwrap_or_else(|| CLIENT.to_string())
}

pub const USAGE: &str = "usage: agent-wrangler install-hooks [all|claude|copilot] [--uninstall]";

/// Installs, or removes, the hooks for the named agents. Returns what happened,
/// line by line, and whether all of it worked.
pub fn run(args: &[String]) -> (Vec<String>, bool) {
    let mut selector = "all";
    let mut uninstall = false;
    for arg in args {
        match arg.as_str() {
            "--uninstall" => uninstall = true,
            "all" | "claude" | "copilot" => selector = arg,
            other => return (vec![format!("unknown argument '{other}'\n{USAGE}")], false),
        }
    }

    let manifest: Value =
        serde_json::from_str(MANIFEST_JSON).expect("the embedded manifest is valid JSON");
    let exe = exe_path();
    let agents: Vec<String> = match selector {
        "all" => manifest
            .as_object()
            .map(|agents| agents.keys().cloned().collect())
            .unwrap_or_default(),
        one => vec![one.to_string()],
    };

    let mut said = Vec::new();
    let mut ok = true;
    for agent in agents {
        match manifest.get(&agent) {
            Some(spec) => match install_agent(&agent, spec, &exe, uninstall) {
                Ok(line) => said.push(line),
                Err(line) => {
                    said.push(line);
                    ok = false;
                }
            },
            None => {
                said.push(format!("{agent}: is not in the manifest"));
                ok = false;
            }
        }
    }
    (said, ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/home/u/.local/bin/agent-wrangler";

    fn events() -> Value {
        json!({
            "SessionStart": ["start"],
            "PreToolUse": [{"matcher": "AskUserQuestion", "actions": ["needsAttention"]}],
        })
    }

    fn installed(settings: &mut Value) {
        merge(settings, "claude", &events(), EXE, false);
    }

    #[test]
    fn a_path_needing_quoting_gets_it() {
        assert_eq!(shell_quote("/home/u/bin/wrangler"), "/home/u/bin/wrangler");
        assert_eq!(shell_quote("/home/my files/w"), "'/home/my files/w'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn a_command_running_the_client_is_ours() {
        assert!(is_ours(
            "/home/u/bin/agent-wrangler hook claude start",
            "claude"
        ));
        assert!(is_ours(
            "'/home/my files/agent-wrangler' hook claude start",
            "claude"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn a_hook_runs_the_windowless_client_where_there_is_one() {
        let dir = std::env::temp_dir().join("agent-wrangler-install-twin");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a directory to install into");
        let exe = dir.join(format!("{CLIENT}.exe"));
        let twin = dir.join(format!("{WINDOWLESS}.exe"));

        // On its own, a client is the only thing that a hook can run.
        assert_eq!(hook_client(exe.clone()), exe);

        fs::write(&twin, "").expect("a twin to find");
        assert_eq!(hook_client(exe.clone()), twin);
        // An install from the twin itself writes the same path. Which of the
        // two ran `install-hooks` cannot change what the hooks say.
        assert_eq!(hook_client(twin.clone()), twin);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_command_this_writes_is_always_one_it_recognises() {
        // The two halves must agree on one property. Whatever the path, the
        // next run claims a freshly written command. The next run does not
        // install the command a second time beside itself.
        for exe in [
            "/home/u/bin/agent-wrangler",
            "/home/my files/agent-wrangler",
            "/home/o'brien/bin/agent-wrangler",
            "/home/u/Development/zellij-agent-wrangler/target/debug/agent-wrangler",
        ] {
            let command = hook_command(exe, "claude", "start");
            assert!(is_ours(&command, "claude"), "{command}");
        }
    }

    #[test]
    fn either_client_is_recognised_however_windows_names_the_file() {
        // A settings file written on Windows names the windowless client. The
        // name carries the extension that the system puts on it, and whatever
        // case the path arrived in. All of it must read back as ours.
        for exe in [
            r"C:\Users\u\AppData\Local\Programs\agent-wrangler\agent-wranglerw.exe",
            r"C:\Users\u\AppData\Local\Programs\agent-wrangler\agent-wrangler.exe",
            r"C:\Users\u\AppData\Local\Programs\agent-wrangler\Agent-Wrangler.EXE",
        ] {
            let command = format!("'{exe}' hook claude start");
            assert!(is_ours(&command, "claude"), "{command}");
        }
    }

    #[test]
    fn a_similarly_named_program_from_elsewhere_is_not_ours() {
        let other = "/home/u/.cache/tmux-agent-wrangler/wrangler-d250518 hook claude start";
        assert!(!is_ours(other, "claude"));
    }

    #[test]
    fn a_command_for_another_agent_is_not_ours_to_replace() {
        assert!(!is_ours("/bin/agent-wrangler hook copilot start", "claude"));
    }

    #[test]
    fn anything_that_is_not_a_hook_invocation_is_not_ours() {
        for command in [
            "",
            "agent-wrangler",
            "agent-wrangler claude start",
            "my-linter --fix agent-wrangler/hook/claude",
        ] {
            assert!(!is_ours(command, "claude"), "{command}");
        }
    }

    #[test]
    fn installing_writes_a_group_per_event() {
        let mut settings = json!({});
        installed(&mut settings);
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            json!(format!("{EXE} hook claude start"))
        );
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["matcher"],
            json!("AskUserQuestion")
        );
    }

    #[test]
    fn installing_twice_leaves_exactly_one_of_each() {
        let mut once = json!({});
        installed(&mut once);
        let mut twice = once.clone();
        installed(&mut twice);
        assert_eq!(once, twice);
    }

    #[test]
    fn everything_else_in_the_file_is_left_untouched() {
        let mut settings = json!({
            "model": "opus",
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "~/.claude/hooks/mine"}]}
                ],
                "Stop": [
                    {"hooks": [{"type": "command", "command": "notify-send done"}]}
                ]
            }
        });
        installed(&mut settings);
        assert_eq!(settings["model"], json!("opus"));
        // The merge adds ours beside the user's, and it does not touch an event
        // that the manifest says nothing about.
        assert_eq!(
            settings["hooks"]["SessionStart"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            settings["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            json!("~/.claude/hooks/mine")
        );
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn uninstalling_takes_back_exactly_what_installing_added() {
        let before = json!({
            "model": "opus",
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "~/.claude/hooks/mine"}]}
                ]
            }
        });
        let mut settings = before.clone();
        installed(&mut settings);
        merge(&mut settings, "claude", &events(), EXE, true);
        assert_eq!(settings, before);
    }

    #[test]
    fn uninstalling_from_a_file_that_had_nothing_else_leaves_no_hooks_key() {
        let mut settings = json!({"model": "opus"});
        installed(&mut settings);
        merge(&mut settings, "claude", &events(), EXE, true);
        assert_eq!(settings, json!({"model": "opus"}));
    }

    #[test]
    fn a_hook_command_that_is_not_ours_survives_both_directions() {
        let theirs = json!({"hooks": {"SessionStart": [
            {"hooks": [{"type": "command",
                        "command": "/home/u/.cache/tmux-agent-wrangler/wrangler-d1 hook claude start"}]}
        ]}});
        let mut settings = theirs.clone();
        installed(&mut settings);
        merge(&mut settings, "claude", &events(), EXE, true);
        assert_eq!(settings, theirs);
    }

    #[test]
    fn an_entry_from_another_path_is_replaced_rather_than_doubled() {
        // The installed path moves with the binary. A command from an earlier
        // install is still this project's to replace.
        let mut settings = json!({"hooks": {"SessionStart": [
            {"hooks": [{"type": "command",
                        "command": "/home/u/Development/zellij-agent-wrangler/target/debug/agent-wrangler hook claude start"}]}
        ]}});
        installed(&mut settings);
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            json!(format!("{EXE} hook claude start"))
        );
    }

    #[test]
    fn the_manifest_describes_every_agent_in_a_shape_this_can_write() {
        let manifest: Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        for (agent, spec) in manifest.as_object().unwrap() {
            let format = spec["format"].as_str().unwrap_or_default();
            assert!(
                matches!(format, "claude" | "copilot"),
                "{agent} is described as {format}"
            );
            assert!(spec["target"].as_str().is_some(), "{agent} names no target");
            for (event, value) in spec["events"].as_object().unwrap() {
                assert!(
                    !groups(value).is_empty(),
                    "{agent}'s {event} describes no group"
                );
            }
        }
    }

    #[test]
    fn copilots_file_carries_a_matcher_on_the_entry_itself() {
        let document = copilot_document("copilot", &events(), EXE);
        assert_eq!(document["version"], json!(1));
        assert_eq!(
            document["hooks"]["PreToolUse"][0]["matcher"],
            json!("AskUserQuestion")
        );
        assert_eq!(
            document["hooks"]["SessionStart"][0]["bash"],
            json!(format!("{EXE} hook copilot start"))
        );
    }
}
