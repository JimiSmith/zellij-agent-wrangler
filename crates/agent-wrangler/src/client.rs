//! This module reaches the daemon. When there is no daemon, this module starts
//! one.
//!
//! Everything in this module runs inside the turn of somebody else, so nothing
//! can fail loudly or wait long. A daemon that this module cannot reach or start
//! costs the event, and not the agent that reported it.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Stream};

use agent_wrangler_core::agent::FORMAT;

use crate::paths;
use crate::platform::spawn_detached;
use crate::proto::{read_message, write_message, Inbound, Outbound};

/// How long this module tries a socket that a new daemon did not claim yet, as
/// a number of attempts and the pause between them.
const TRIES: u32 = 40;
const PAUSE: Duration = Duration::from_millis(50);

fn connect() -> std::io::Result<Stream> {
    let name = paths::socket_name().to_ns_name::<GenericNamespaced>()?;
    Stream::connect(name)
}

/// Starts a daemon, which is this same executable under another argument.
///
/// Side effect: this function spawns a detached process. It runs the path of
/// this executable, so a daemon and the hook that started it always have the
/// same version.
fn start() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    spawn_detached(&exe, &["daemon"])
}

/// Connects to the daemon. If nothing listens, this function starts a daemon
/// and waits for it.
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

/// Says one thing to the daemon, and then stops.
///
/// This function drops the connection and does not read it back, because the
/// caller can do nothing with an answer. What the daemon does with the message
/// reaches the user through the clients that it delivers to, and not through
/// this function.
pub fn tell(message: &Inbound) -> std::io::Result<()> {
    let stream = reach()?;
    let mut writer = BufWriter::new(&stream);
    write_message(&mut writer, message)
}

/// Asks the daemon for the state that it holds.
///
/// The daemon answers as soon as it reads the question, and it closes only after
/// this end closes. This function therefore reads the reply before it drops the
/// connection.
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

/// Asks the daemon to say what it does, and writes the answer out until the
/// daemon stops.
///
/// Side effect: this function writes one line per record to `out`, and flushes
/// each line. A buffer therefore never holds a record while a reader waits for
/// the next one. When the daemon goes away, this function returns. That end is
/// the only end that a watcher has, because nothing here asks to stop.
pub fn watch<W: Write>(out: &mut W) -> std::io::Result<()> {
    let stream = reach()?;
    {
        let mut writer = BufWriter::new(&stream);
        write_message(&mut writer, &Inbound::Monitor { format: FORMAT })?;
    }
    let mut reader = BufReader::new(&stream);
    // This loop writes each line as it arrived, and does not decode and encode
    // it again. What the daemon says is already one record to a line. A watcher
    // of a later build therefore cannot drop an unknown field without a word.
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
        // A hook runs inside the turn of an agent. Whatever goes wrong, this
        // function must end, and end soon enough that nobody sees the agent
        // held up.
        assert!(PAUSE * TRIES <= Duration::from_secs(3));
    }
}
