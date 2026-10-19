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
use std::time::Duration;

use chrono::{DateTime, Utc};

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

/// What moved in a wait, named in a heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Signal {
    /// Bytes were appended to the check's log.
    LogBytes,
    /// A worker turn or tool event arrived.
    WorkerEvent,
    /// The CPU time of the check's process group rose between samples.
    Cpu,
}

impl Signal {
    fn as_str(self) -> &'static str {
        match self {
            Self::LogBytes => "log_bytes",
            Self::WorkerEvent => "worker_event",
            Self::Cpu => "cpu",
        }
    }
}

/// One reading of the things that count as progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Sample {
    pub log_bytes: u64,
    pub worker_events: u64,
    /// `None` when the CPU time could not be read.
    pub cpu: Option<Duration>,
}

/// When a wait last moved, and by what: the state behind a heartbeat.
pub(super) struct Progress {
    last: Sample,
    progress_at: DateTime<Utc>,
    signal: Option<Signal>,
}

impl Progress {
    pub(super) fn new(now: DateTime<Utc>, first: Sample) -> Self {
        Self {
            last: first,
            progress_at: now,
            signal: None,
        }
    }

    /// Fold in a new sample taken at `now`; the heartbeat it makes. Alive is
    /// `now`; progress is the last time any signal rose.
    pub(super) fn observe(&mut self, now: DateTime<Utc>, sample: Sample) -> Event {
        let moved = if sample.worker_events > self.last.worker_events {
            Some(Signal::WorkerEvent)
        } else if sample.log_bytes > self.last.log_bytes {
            Some(Signal::LogBytes)
        } else if matches!((self.last.cpu, sample.cpu), (Some(before), Some(after)) if after > before)
        {
            Some(Signal::Cpu)
        } else {
            None
        };
        if let Some(signal) = moved {
            self.progress_at = now;
            self.signal = Some(signal);
        }
        self.last = Sample {
            cpu: sample.cpu.or(self.last.cpu),
            ..sample
        };
        Event::Heartbeat {
            alive_at: now.to_rfc3339(),
            progress_at: self.progress_at.to_rfc3339(),
            signal: self.signal.map(|signal| signal.as_str().to_string()),
        }
    }
}

/// Say the runner is alive every `interval` until `ended` fires or its
/// sender is dropped. `probe` reads the progress signals; `clock` says when.
pub(super) fn heartbeat(
    events: &Events,
    interval: Duration,
    ended: &Receiver<()>,
    clock: &dyn Fn() -> DateTime<Utc>,
    probe: &mut dyn FnMut() -> Sample,
) {
    let mut progress = Progress::new(clock(), probe());
    while let Err(RecvTimeoutError::Timeout) = ended.recv_timeout(interval) {
        events.emit(progress.observe(clock(), probe()));
    }
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

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("a time")
    }

    fn cpu(seconds: u64) -> Sample {
        Sample {
            cpu: Some(Duration::from_secs(seconds)),
            ..Sample::default()
        }
    }

    fn parts(event: Event) -> (String, String, Option<String>) {
        match event {
            Event::Heartbeat {
                alive_at,
                progress_at,
                signal,
            } => (alive_at, progress_at, signal),
            other => panic!("not a heartbeat: {other:?}"),
        }
    }

    #[test]
    fn a_check_that_burns_cpu_silently_is_alive_with_progress() {
        let mut progress = Progress::new(at(0), cpu(1));
        let (alive, progressed, signal) = parts(progress.observe(at(60), cpu(40)));
        assert_eq!(alive, at(60).to_rfc3339());
        assert_eq!(progressed, at(60).to_rfc3339());
        assert_eq!(signal.as_deref(), Some("cpu"));
    }

    #[test]
    fn a_check_that_sleeps_past_the_threshold_is_alive_with_no_progress() {
        let mut progress = Progress::new(at(0), cpu(1));
        let mut last = None;
        for minute in 1..=14 {
            last = Some(parts(progress.observe(at(minute * 60), cpu(1))));
        }
        let (alive, progressed, signal) = last.expect("beats");
        assert_eq!(alive, at(14 * 60).to_rfc3339());
        assert_eq!(progressed, at(0).to_rfc3339());
        assert_eq!(signal, None);
    }

    #[test]
    fn log_bytes_and_worker_events_count_as_progress_and_the_last_signal_stays() {
        let mut progress = Progress::new(at(0), Sample::default());
        let logged = Sample {
            log_bytes: 10,
            ..Sample::default()
        };
        assert_eq!(
            parts(progress.observe(at(60), logged)).2.as_deref(),
            Some("log_bytes")
        );
        let (_, progressed, signal) = parts(progress.observe(at(120), logged));
        assert_eq!(progressed, at(60).to_rfc3339());
        assert_eq!(signal.as_deref(), Some("log_bytes"));
        let turn = Sample {
            worker_events: 1,
            ..logged
        };
        assert_eq!(
            parts(progress.observe(at(180), turn)).2.as_deref(),
            Some("worker_event")
        );
    }

    #[test]
    fn the_heartbeat_thread_beats_until_the_wait_ends_and_is_joined() {
        let dir = crate::test_support::short_temp_dir("hb-thread");
        let artifacts = dir.join("run-t");
        let events = Events::for_run("run-t", &artifacts);
        let (ended_tx, ended_rx) = std::sync::mpsc::channel::<()>();
        let mut ended_tx = Some(ended_tx);
        let mut probes = 0;
        let mut probe = move || {
            probes += 1;
            // The wait ends during the fourth sample.
            if probes == 4 {
                ended_tx.take();
            }
            cpu(probes)
        };
        std::thread::scope(|scope| {
            let events = &events;
            scope.spawn(move || {
                heartbeat(
                    events,
                    Duration::from_millis(2),
                    &ended_rx,
                    &|| at(0),
                    &mut probe,
                )
            });
        });
        let text = std::fs::read_to_string(artifacts.join("events.jsonl")).expect("events");
        let beats: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect();
        assert_eq!(beats.len(), 3, "{text}");
        assert!(beats.iter().all(|b| b["event"]["kind"] == "heartbeat"));
        assert!(beats.iter().all(|b| b["event"]["signal"] == "cpu"));
    }

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
