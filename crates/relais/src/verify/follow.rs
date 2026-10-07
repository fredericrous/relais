//! Streaming a running check's log into events.
//!
//! The command writes its stdout and stderr to a log file, and that file is
//! the record. The follower reads the growing file with a descriptor of its
//! own and never touches the command's pipes, so it cannot slow or block
//! the check. New output is merged into `output` events every
//! [`OUTPUT_MERGE`], each at most [`OUTPUT_CHUNK_BYTES`] of text; a check
//! puts at most [`OUTPUT_BUDGET_BYTES`] into events in all, and what is not
//! put there is counted and reported once, as `elided_bytes`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};

use crate::protocol::{Event, Events, OUTPUT_BUDGET_BYTES, OUTPUT_CHUNK_BYTES, OUTPUT_MERGE};

/// Follow `log_path` until `ended` fires or its sender is dropped, then
/// read once more and report what was left out.
pub(super) fn follow(log_path: &Path, label: &str, events: &Events, ended: &Receiver<()>) {
    // No follower without a readable log: the check still runs, and the
    // log file is the record either way.
    let Ok(log) = File::open(log_path) else {
        return;
    };
    let mut follower = Follower {
        log,
        label,
        events,
        offset: 0,
        emitted: 0,
        elided: 0,
    };
    loop {
        let over = match ended.recv_timeout(OUTPUT_MERGE) {
            Err(RecvTimeoutError::Timeout) => false,
            Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
        };
        follower.poll();
        if over {
            break;
        }
    }
    follower.report_elided();
}

struct Follower<'a> {
    log: File,
    label: &'a str,
    events: &'a Events,
    /// Where the next unread byte of the log is.
    offset: u64,
    /// Bytes of output text sent into events so far.
    emitted: u64,
    /// Bytes of output left out of events so far.
    elided: u64,
}

impl Follower<'_> {
    /// Merge what the command wrote since the last poll. Past the budget,
    /// or behind it, the follower skips ahead and counts what it skipped.
    fn poll(&mut self) {
        // A log that cannot be read this tick is read on the next one.
        let Ok(len) = self.log.metadata().map(|meta| meta.len()) else {
            return;
        };
        let available = len.saturating_sub(self.offset);
        let take = available.min(OUTPUT_BUDGET_BYTES - self.emitted);
        if available == 0 {
            return;
        }
        let mut bytes = vec![0; take as usize];
        if self.log.seek(SeekFrom::Start(self.offset)).is_err()
            || self.log.read_exact(&mut bytes).is_err()
        {
            return;
        }
        // Everything past what was taken is skipped, not queued.
        let skipped = available - take;
        // A character cut by the end of what was read is left for the
        // next poll, unless nothing more will come of it.
        let whole = if skipped == 0 {
            whole_chars(&bytes)
        } else {
            bytes.len()
        };
        self.offset = if skipped == 0 {
            self.offset + whole as u64
        } else {
            len
        };
        self.elided += skipped;
        self.emitted += whole as u64;
        for piece in pieces(&bytes[..whole]) {
            self.events.emit(Event::Output {
                label: self.label.to_string(),
                text: String::from_utf8_lossy(piece).into_owned(),
                elided_bytes: 0,
            });
        }
    }

    fn report_elided(&self) {
        if self.elided > 0 {
            self.events.emit(Event::Output {
                label: self.label.to_string(),
                text: String::new(),
                elided_bytes: self.elided,
            });
        }
    }
}

/// How many leading bytes of `bytes` end on a character boundary: all of
/// them, unless the last character is cut off.
fn whole_chars(bytes: &[u8]) -> usize {
    let tail = bytes.len().saturating_sub(3);
    for (at, byte) in bytes.iter().enumerate().skip(tail).rev() {
        let width = match byte {
            0x00..=0x7F => return bytes.len(),
            0x80..=0xBF => continue,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xFF => 4,
        };
        return if bytes.len() - at < width {
            at
        } else {
            bytes.len()
        };
    }
    bytes.len()
}

/// `bytes` in pieces of at most [`OUTPUT_CHUNK_BYTES`], none cut inside a
/// character.
fn pieces(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(OUTPUT_CHUNK_BYTES);
        while end < rest.len() && end > 0 && (rest[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        // A piece of nothing but continuation bytes (not UTF-8 at all)
        // is cut at the limit instead.
        if end == 0 {
            end = rest.len().min(OUTPUT_CHUNK_BYTES);
        }
        let (piece, tail) = rest.split_at(end);
        rest = tail;
        Some(piece)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_character_is_left_for_the_next_poll() {
        let text = "aé".as_bytes();
        assert_eq!(whole_chars(text), 3);
        assert_eq!(whole_chars(&text[..2]), 1);
        assert_eq!(whole_chars(b"abc"), 3);
        assert_eq!(whole_chars(b""), 0);
    }

    #[test]
    fn pieces_never_exceed_the_chunk_and_never_split_a_character() {
        let text = "é".repeat(OUTPUT_CHUNK_BYTES);
        let all: Vec<&[u8]> = pieces(text.as_bytes()).collect();
        assert!(all.iter().all(|piece| piece.len() <= OUTPUT_CHUNK_BYTES));
        assert!(all.iter().all(|piece| std::str::from_utf8(piece).is_ok()));
        assert_eq!(all.concat(), text.as_bytes());
    }
}
