//! Terminal bytes translated into portable user actions.
use agent_wrangler_sidebar::UserAction;

#[derive(Default)]
pub struct InputDecoder {
    sequence: Vec<u8>,
    discard_control_string: bool,
    string_escape: bool,
    paste: bool,
    paste_end: usize,
    oversized: bool,
}

impl InputDecoder {
    pub fn push(&mut self, byte: u8) -> Option<UserAction> {
        if self.paste {
            const END: &[u8] = b"\x1b[201~";
            self.paste_end = if byte == END[self.paste_end] {
                self.paste_end + 1
            } else {
                usize::from(byte == END[0])
            };
            if self.paste_end == END.len() {
                self.paste = false;
                self.paste_end = 0;
            }
            return None;
        }
        if self.discard_control_string {
            if byte == 0x07 || (self.string_escape && byte == b'\\') {
                self.discard_control_string = false;
            }
            self.string_escape = byte == 0x1b;
            return None;
        }
        if self.oversized {
            if (0x40..=0x7e).contains(&byte) {
                self.oversized = false;
            }
            return None;
        }
        if self.sequence.is_empty() {
            return match byte {
                0x1b => {
                    self.sequence.push(byte);
                    None
                }
                b'j' => Some(UserAction::Next),
                b'k' => Some(UserAction::Previous),
                b'\r' | b'\n' => Some(UserAction::Activate),
                b' ' => Some(UserAction::OpenOrClosePreview),
                b'q' | b'Q' | 0x03 | 0x11 => Some(UserAction::Quit),
                _ => None,
            };
        }
        self.sequence.push(byte);
        if self.sequence.len() == 2 && matches!(byte, b']' | b'P' | b'_' | b'^' | b'X') {
            self.discard_control_string = true;
            self.sequence.clear();
            return None;
        }
        if self.sequence.len() > 64 {
            self.sequence.clear();
            self.oversized = !(0x40..=0x7e).contains(&byte);
            return None;
        }
        if self.sequence.len() == 2 && matches!(byte, b'[' | b'O') {
            return None;
        }
        if self.sequence.len() > 2 && !(0x40..=0x7e).contains(&byte) {
            return None;
        }
        let action = match self.sequence.as_slice() {
            b"\x1b[200~" => {
                self.paste = true;
                None
            }
            b"\x1b[A" | b"\x1bOA" => Some(UserAction::Previous),
            b"\x1b[B" | b"\x1bOB" => Some(UserAction::Next),
            sequence => decode_mouse(sequence),
        };
        self.sequence.clear();
        action
    }
}

fn decode_mouse(sequence: &[u8]) -> Option<UserAction> {
    let report = std::str::from_utf8(sequence)
        .ok()?
        .strip_prefix("\x1b[<")?
        .strip_suffix('M')?;
    let mut fields = report.split(';');
    let button: u8 = fields.next()?.parse().ok()?;
    let column = fields.next()?.parse::<usize>().ok()?.checked_sub(1)?;
    let row = fields.next()?.parse::<usize>().ok()?.checked_sub(1)?;
    if fields.next().is_some() {
        return None;
    }
    match button {
        0 => Some(UserAction::Click(row, column)),
        64 => Some(UserAction::Previous),
        65 => Some(UserAction::Next),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_sequences_and_pastes_never_become_commands() {
        assert!(
            decode(b"\x1bq\x1b[1;2A\x1b]0;jq\x07\x1bPqj\x1b\\\x1b[200~jq\r \x1b[201~").is_empty()
        );
        let mut decoder = InputDecoder::default();
        for byte in b"\x1b["
            .iter()
            .copied()
            .chain(std::iter::repeat_n(b'1', 4096))
            .chain([b'q'])
        {
            assert_eq!(decoder.push(byte), None);
            assert!(decoder.sequence.len() <= 64);
        }
        assert_eq!(decoder.push(b'j'), Some(UserAction::Next));
    }

    #[test]
    fn mouse_reports_keep_zero_based_row_and_column() {
        assert_eq!(
            decode(b"\x1b[<0;12;3M\x1b[<0;12;3m\x1b[<64;1;1M\x1b[<65;1;1M"),
            vec![
                UserAction::Click(2, 11),
                UserAction::Previous,
                UserAction::Next
            ]
        );
        assert!(decode(b"\x1b[<0;0;3M\x1b[<32;2;3M\x1b[<2;2;3M").is_empty());
    }

    fn decode(bytes: &[u8]) -> Vec<UserAction> {
        let mut decoder = InputDecoder::default();
        bytes
            .iter()
            .filter_map(|byte| decoder.push(*byte))
            .collect()
    }

    #[test]
    fn keys_and_fragmented_arrows_produce_actions() {
        assert_eq!(
            decode(b"jk\r \x1b[A\x1b[B\x1bOA\x1bOBqQ\x03\x11"),
            vec![
                UserAction::Next,
                UserAction::Previous,
                UserAction::Activate,
                UserAction::OpenOrClosePreview,
                UserAction::Previous,
                UserAction::Next,
                UserAction::Previous,
                UserAction::Next,
                UserAction::Quit,
                UserAction::Quit,
                UserAction::Quit,
                UserAction::Quit,
            ]
        );
    }
}
