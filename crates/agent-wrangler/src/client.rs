//! Reaching the daemon, and starting it when there is none.
//!
//! Everything here runs inside somebody else's turn, so nothing is allowed to
//! fail loudly or to wait long. A daemon that cannot be reached or started
//! costs the event, not the agent that reported it.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Stream};

use agent_wrangler_core::agent::FORMAT;

use crate::paths;
use crate::platform::spawn_detached;
use crate::proto::{read_message, write_message, Inbound, Outbound};

/// How long to keep trying a socket that a freshly started daemon has not
/// claimed yet, as attempts and the pause between them.
const TRIES: u32 = 40;
const PAUSE: Duration = Duration::from_millis(50);

fn connect() -> std::io::Result<Stream> {
    let name = paths::socket_name().to_ns_name::<GenericNamespaced>()?;
    Stream::connect(name)
}

/// Start a daemon, which is this same executable under another argument.
///
/// Side effect: spawns a detached process. Running our own path is what makes a
/// daemon and the hook that started it impossible to have at different versions.
fn start() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    spawn_detached(&exe, &["daemon"])
}

/// Connect, starting a daemon and waiting for it if nothing is listening.
fn reach() -> std::io::Result<Stream> {
    if let Ok(stream) = connect() {
        return Ok(stream);
    }
    start()?;
    let mut last = None;
    for _ in 0..TRIES {
        std::thread::sleep(PAUSE);
        match connect() {
            Ok(stream) => return Ok(stream),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no daemon")))
}

/// Say one thing to the daemon and stop listening.
///
/// The connection is dropped rather than read back, because there is nothing the
/// caller could do with an answer: what the daemon does with the message reaches
/// the user through the clients it delivers to, not through here.
pub fn tell(message: &Inbound) -> std::io::Result<()> {
    let stream = reach()?;
    let mut writer = BufWriter::new(&stream);
    write_message(&mut writer, message)
}

/// Ask the daemon for the state it holds.
///
/// The daemon answers as soon as it reads the question and closes only when this
/// end does, so the reply is read before the connection is dropped.
pub fn ask() -> std::io::Result<String> {
    let stream = reach()?;
    {
        let mut writer = BufWriter::new(&stream);
        write_message(&mut writer, &Inbound::Snapshot)?;
    }
    let mut reader = BufReader::new(&stream);
    match read_message::<_, Outbound>(&mut reader)? {
        Some(Outbound::Agents { records, .. }) => Ok(records),
        None => Err(std::io::Error::other("the daemon said nothing")),
    }
}

/// Ask the daemon to say what it does, and write it out until it stops.
///
/// Side effect: writes a line per record to `out`, flushing each, so that a run
/// being read as it happens is not held in a buffer waiting for the next one.
/// Returns when the daemon goes away, which for a watcher is the only ending
/// there is: nothing here asks to stop.
pub fn watch<W: Write>(out: &mut W) -> std::io::Result<()> {
    let stream = reach()?;
    {
        let mut writer = BufWriter::new(&stream);
        write_message(&mut writer, &Inbound::Monitor { format: FORMAT })?;
    }
    let mut reader = BufReader::new(&stream);
    // Written out as it arrived rather than decoded and encoded again: what the
    // daemon says is already one record to a line, and passing it through means
    // a watcher of a later build cannot silently drop a field it does not know.
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        out.write_all(line.as_bytes())?;
        out.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn giving_up_takes_a_bounded_time() {
        // A hook runs inside an agent's turn. Whatever goes wrong, this has to
        // end, and end soon enough that the agent is not visibly held up.
        assert!(PAUSE * TRIES <= Duration::from_secs(3));
    }
}
