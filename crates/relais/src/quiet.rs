//! The watcher behind `relais quiet watch --protocol`: it polls the
//! active-run directory that amont and amont-agent write one JSON entry
//! into per quiet run, tails each run's log, and prints the protocol
//! lines the plugin folds into its pane (`docs/quiet-runs.md`).
//!
//! The calculations (line cutting, line splitting, the tail window) are
//! pure; reading the directory and the logs and writing stdout stay in
//! `poll` and `watch`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::procs;

/// How often the directory and the logs are read.
const POLL: Duration = Duration::from_millis(250);
/// A heartbeat line makes a dead reader visible to a watcher that is
/// otherwise silent.
const HEARTBEAT: Duration = Duration::from_secs(5);
/// A new row starts this far from the end of its log.
const FIRST_WINDOW: u64 = 16 * 1024;
/// No poll reads more than this from one log.
const MAX_READ: u64 = 64 * 1024;
/// No run emits more output lines than this in one poll.
const MAX_LINES: usize = 10;
/// The widest stored line, in UTF-16 units (`MAX_LINE` in timeline.ts).
const MAX_LINE: usize = 240;
/// A dead-pid entry that never ended is removed once this old.
const STALE_MS: u64 = 60 * 60 * 1000;
/// Polls a seen run may have a dead pid and no `ended` before it is
/// closed: the tool writes `ended` just after its process stops.
const DEAD_GRACE_POLLS: u32 = 4;

/// The active-run directory: `$QUIET_ACTIVE_DIR`, else the platform's
/// state directory. `get` reads the environment, so the caller owns the
/// ambient access.
pub fn active_dir(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = get("QUIET_ACTIVE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if cfg!(windows) {
        let base = get("LOCALAPPDATA").filter(|d| !d.is_empty())?;
        return Some(PathBuf::from(base).join("quiet").join("active"));
    }
    let state = match get("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(get("HOME").filter(|d| !d.is_empty())?)
            .join(".local")
            .join("state"),
    };
    Some(state.join("quiet").join("active"))
}

/// One active-run entry file (`docs/quiet-runs.md`). Unknown fields are
/// ignored so a newer writer does not blind an older watcher.
#[derive(Debug, Deserialize)]
struct Entry {
    v: u64,
    tool: String,
    session: Option<String>,
    check: String,
    repo: String,
    log: PathBuf,
    pid: u32,
    started: u64,
    exit: Option<i64>,
    ended: Option<u64>,
}

/// Drop CSI sequences (`ESC [ params intermediates final`), as the
/// first replace of `cutLine` does. An ESC that opens no complete
/// sequence stays for the control-character pass.
fn strip_csi(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        match csi_end(&chars, i) {
            Some(end) => i = end,
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Where the CSI sequence starting at `at` ends, if one starts there.
fn csi_end(chars: &[char], at: usize) -> Option<usize> {
    if chars.get(at) != Some(&'\u{1b}') || chars.get(at + 1) != Some(&'[') {
        return None;
    }
    let mut i = at + 2;
    while matches!(chars.get(i), Some('0'..='9' | ';' | '?')) {
        i += 1;
    }
    while matches!(chars.get(i), Some(' '..='/')) {
        i += 1;
    }
    matches!(chars.get(i), Some('@'..='~')).then_some(i + 1)
}

/// `cutLine` of `claude-plugin/hooks/timeline.ts`: CSI out, then control
/// characters out, tabs to two spaces, and a line wider than 240 UTF-16
/// units cut to 239 plus an ellipsis. A surrogate pair the cut would
/// split is dropped whole, because a Rust string cannot hold half of one.
pub fn cut_line(line: &str) -> String {
    let clean: String = strip_csi(line)
        .chars()
        .filter(|c| !matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'))
        .flat_map(|c| if c == '\t' { vec![' ', ' '] } else { vec![c] })
        .collect();
    if clean.encode_utf16().count() <= MAX_LINE {
        return clean;
    }
    let mut units = 0;
    let mut cut = String::new();
    for c in clean.chars() {
        units += c.len_utf16();
        if units > MAX_LINE - 1 {
            break;
        }
        cut.push(c);
    }
    cut.push('…');
    cut
}

/// Where a log is read from this poll.
#[derive(Debug, PartialEq)]
struct Window {
    /// First byte to read (one before the first wanted byte after a jump,
    /// so a partial line can be told from a whole one).
    from: u64,
    /// The bytes before `from + 1` were skipped: the first line is partial.
    jumped: bool,
    /// The log shrank: the held partial line belongs to the old content.
    reset: bool,
}

fn window(len: u64, offset: Option<u64>) -> Window {
    let (start, reset) = match offset {
        None => (len.saturating_sub(FIRST_WINDOW), false),
        Some(o) if o > len => (0, true),
        Some(o) => (o.max(len.saturating_sub(MAX_READ)), false),
    };
    let jumped = start > 0 && offset != Some(start);
    Window {
        from: if jumped { start - 1 } else { start },
        jumped,
        reset,
    }
}

/// What a log tail remembers between polls.
#[derive(Debug, Default)]
struct Tail {
    offset: Option<u64>,
    /// Bytes of an incomplete trailing line.
    carry: Vec<u8>,
    /// The last chunk ended in `\r`: a leading `\n` completes `\r\n`.
    skip_lf: bool,
}

/// Split `data` on `\n`, `\r` and `\r\n`, completing lines held in
/// `carry`. The trailing incomplete line stays in `carry` unless `flush`.
fn split_lines(tail: &mut Tail, data: &[u8], flush: bool) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    for &b in data {
        if std::mem::take(&mut tail.skip_lf) && b == b'\n' {
            continue;
        }
        match b {
            b'\n' => lines.push(std::mem::take(&mut tail.carry)),
            b'\r' => {
                lines.push(std::mem::take(&mut tail.carry));
                tail.skip_lf = true;
            }
            _ => tail.carry.push(b),
        }
    }
    if flush && !tail.carry.is_empty() {
        lines.push(std::mem::take(&mut tail.carry));
    }
    lines
}

/// Drop the partial first line of a window that began mid-log. `data[0]`
/// is the byte before the wanted ones.
fn after_jump(data: &[u8]) -> &[u8] {
    match data.first() {
        Some(b'\n' | b'\r') => &data[1..],
        Some(_) => match data.iter().position(|b| matches!(b, b'\n' | b'\r')) {
            Some(i) => &data[i + 1..],
            None => &[],
        },
        None => data,
    }
}

/// The new complete lines of a log, cut, at most `MAX_LINES` (the last
/// ones). `flush` also emits a held partial line: the run is over. A
/// missing log reads as no lines.
fn read_lines(path: &Path, tail: &mut Tail, flush: bool) -> Vec<String> {
    let Ok(mut file) = fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let w = window(len, tail.offset);
    if w.reset || w.jumped {
        tail.carry.clear();
        tail.skip_lf = false;
    }
    let mut data = Vec::new();
    let read = file
        .seek(SeekFrom::Start(w.from))
        .and_then(|_| file.take(len - w.from).read_to_end(&mut data));
    let Ok(read) = read else {
        return Vec::new();
    };
    tail.offset = Some(w.from + read as u64);
    let wanted = if w.jumped { after_jump(&data) } else { &data };
    let lines = split_lines(tail, wanted, flush);
    let skip = lines.len().saturating_sub(MAX_LINES);
    lines
        .iter()
        .skip(skip)
        .map(|l| cut_line(&String::from_utf8_lossy(l)))
        .collect()
}

/// What the watcher knows about one run.
#[derive(Debug)]
pub struct Run {
    log: PathBuf,
    tail: Tail,
    /// Polls in a row the pid was dead with no `ended`.
    dead_polls: u32,
    /// `ended` was emitted (the pid died without one); the file is only
    /// waiting to be deleted.
    closed: bool,
}

impl Run {
    fn new(log: &Path) -> Self {
        Run {
            log: log.to_path_buf(),
            tail: Tail::default(),
            dead_polls: 0,
            closed: false,
        }
    }
}

/// The runs a watcher has announced, by entry id.
pub type Runs = BTreeMap<String, Run>;

fn started_event(id: &str, e: &Entry) -> Value {
    json!({"relais": "quiet", "event": "started", "id": id, "session": e.session,
           "tool": e.tool, "check": e.check, "repo": e.repo, "started": e.started})
}

fn output_event(id: &str, lines: Vec<String>) -> Option<Value> {
    (!lines.is_empty())
        .then(|| json!({"relais": "quiet", "event": "output", "id": id, "lines": lines}))
}

fn ended_event(id: &str, exit: Option<i64>, ended: u64) -> Value {
    json!({"relais": "quiet", "event": "ended", "id": id, "exit": exit, "ended": ended})
}

/// The heartbeat line, exactly.
pub fn heartbeat() -> Value {
    json!({"relais": "quiet", "event": "heartbeat"})
}

fn read_entry(path: &Path) -> Option<Entry> {
    let entry: Entry = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (entry.v == 1 && entry.log.is_absolute()).then_some(entry)
}

fn age_ms(path: &Path, now_ms: u64) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let at = modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    Some(now_ms.saturating_sub(at))
}

/// Entry ids and paths in the directory, in id order.
fn entry_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| Some((p.file_stem()?.to_str()?.to_string(), p)))
        .collect();
    files.sort();
    files
}

/// An entry that carries `ended`: emit what was never emitted, then the
/// end, then delete the file. Checked before pid liveness, because the
/// writer has usually exited by then.
fn finish_ended(id: &str, path: &Path, e: &Entry, ended: u64, runs: &mut Runs) -> Vec<Value> {
    let mut events = Vec::new();
    match runs.remove(id) {
        Some(run) if run.closed => {}
        seen => {
            let mut run = seen.unwrap_or_else(|| {
                events.push(started_event(id, e));
                Run::new(&e.log)
            });
            events.extend(output_event(id, read_lines(&e.log, &mut run.tail, true)));
            events.push(ended_event(id, e.exit, ended));
        }
    }
    if let Err(err) = fs::remove_file(path) {
        if err.kind() != io::ErrorKind::NotFound {
            // Keep the run, closed, so the next poll does not announce the
            // same entry again every 250 ms; say why once.
            if !runs.contains_key(id) {
                eprintln!(
                    "relais quiet watch: cannot remove {}: {err}",
                    path.display()
                );
            }
            let mut closed = Run::new(&e.log);
            closed.closed = true;
            runs.insert(id.to_string(), closed);
        }
    }
    events
}

/// Whether a run whose pid is alive is still this run: the pid may have been
/// reused by another process after the run died without writing `ended`. Its
/// log must still exist, and it or the run must be younger than
/// [`STALE_MS`] — a long, silent build is not cut short, but a reused pid
/// cannot keep a dead run on screen forever.
fn plausibly_live(e: &Entry, now_ms: u64) -> bool {
    let Some(log_age) = age_ms(&e.log, now_ms) else {
        return false;
    };
    log_age < STALE_MS || now_ms.saturating_sub(e.started) < STALE_MS
}

/// A run seen live that stopped without writing `ended`: it ends with a
/// null exit, after a last drain of its log.
fn finish_vanished(id: &str, run: &mut Run, now_ms: u64) -> Vec<Value> {
    let mut events: Vec<Value> = output_event(id, read_lines(&run.log, &mut run.tail, true))
        .into_iter()
        .collect();
    events.push(ended_event(id, None, now_ms));
    events
}

/// One poll of the directory: the events it yields, in order.
pub fn poll(dir: &Path, runs: &mut Runs, now_ms: u64) -> Vec<Value> {
    let mut events = Vec::new();
    let files = entry_files(dir);
    for (id, path) in &files {
        let Some(e) = read_entry(path) else { continue };
        if let Some(ended) = e.ended {
            events.extend(finish_ended(id, path, &e, ended, runs));
        } else if procs::alive(e.pid) && plausibly_live(&e, now_ms) {
            let run = runs.entry(id.clone()).or_insert_with(|| {
                events.push(started_event(id, &e));
                Run::new(&e.log)
            });
            run.dead_polls = 0;
            events.extend(output_event(id, read_lines(&e.log, &mut run.tail, false)));
        } else if let Some(run) = runs.get_mut(id) {
            run.dead_polls += 1;
            if run.dead_polls >= DEAD_GRACE_POLLS && !run.closed {
                run.closed = true;
                events.extend(finish_vanished(id, run, now_ms));
            }
        } else if age_ms(path, now_ms).is_some_and(|age| age > STALE_MS) {
            let _ = fs::remove_file(path);
        }
    }
    let present: Vec<&String> = files.iter().map(|(id, _)| id).collect();
    let gone: Vec<String> = runs
        .keys()
        .filter(|id| !present.contains(id))
        .cloned()
        .collect();
    for id in gone {
        if let Some(mut run) = runs.remove(&id) {
            if !run.closed {
                events.extend(finish_vanished(&id, &mut run, now_ms));
            }
        }
    }
    events
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn emit(out: &mut impl Write, event: &Value) -> io::Result<()> {
    writeln!(out, "{event}")?;
    out.flush()
}

/// Poll until a stdout write fails or the parent changes. Never reads
/// stdin. The dead reader shows as a failed write, which the heartbeat
/// guarantees within 5 s; the first one goes out at once.
pub fn watch(dir: &Path, out: &mut impl Write) -> io::Result<()> {
    let parent = procs::parent_pid();
    let mut runs = Runs::new();
    let mut last_beat: Option<std::time::Instant> = None;
    loop {
        if procs::parent_pid() != parent {
            return Ok(());
        }
        for event in poll(dir, &mut runs, now_ms()) {
            emit(out, &event)?;
        }
        if last_beat.is_none_or(|at| at.elapsed() >= HEARTBEAT) {
            emit(out, &heartbeat())?;
            last_beat = Some(std::time::Instant::now());
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_line_strips_colour_and_controls_and_widens_tabs() {
        assert_eq!(cut_line("\u{1b}[31mred\u{1b}[0m\tx\u{7}y\u{0}"), "red  xy");
        assert_eq!(cut_line("\u{1b}[1;32;?m!"), "!");
        // An ESC with no complete sequence is only a control character.
        assert_eq!(cut_line("a\u{1b}[b"), "a");
        assert_eq!(cut_line("a\u{1b}[zb"), "ab");
    }

    #[test]
    fn cut_line_counts_utf16_units() {
        let bmp = "日".repeat(300);
        assert_eq!(cut_line(&bmp), format!("{}…", "日".repeat(239)));
        assert_eq!(cut_line(&"日".repeat(240)), "日".repeat(240));
        // Astral characters are two units: 120 fit, 121 are cut; the pair the
        // cut would split is dropped whole.
        assert_eq!(cut_line(&"😀".repeat(120)), "😀".repeat(120));
        assert_eq!(
            cut_line(&"😀".repeat(121)),
            format!("{}…", "😀".repeat(119))
        );
    }

    #[test]
    fn lines_split_on_cr_lf_and_crlf_and_hold_the_tail() {
        let mut tail = Tail::default();
        let lines = split_lines(&mut tail, b"a\r\nb\rc\nd", false);
        assert_eq!(lines, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        assert_eq!(tail.carry, b"d");
        // A CRLF split across two reads is still one terminator.
        let more = split_lines(&mut tail, b"\r", false);
        assert_eq!(more, vec![b"d".to_vec()]);
        assert!(split_lines(&mut tail, b"\ne", false).is_empty());
        assert_eq!(split_lines(&mut tail, b"", true), vec![b"e".to_vec()]);
    }

    #[test]
    fn window_starts_16k_from_the_end_and_reads_at_most_64k() {
        assert_eq!(window(100, None).from, 0);
        let first = window(1_000_000, None);
        assert_eq!(
            (first.from, first.jumped),
            (1_000_000 - FIRST_WINDOW - 1, true)
        );
        let burst = window(1_000_000, Some(10));
        assert_eq!((burst.from, burst.jumped), (1_000_000 - MAX_READ - 1, true));
        let next = window(50, Some(20));
        assert_eq!((next.from, next.jumped, next.reset), (20, false, false));
        assert!(window(5, Some(20)).reset);
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("relais-quiet-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry_json(log: &Path, pid: u32, started: u64, extra: &str) -> String {
        format!(
            r#"{{"v":1,"tool":"amont","session":"s","check":"c","repo":"/r","log":{},"pid":{pid},"started":{started}{extra}}}"#,
            serde_json::to_string(&log.to_string_lossy()).unwrap()
        )
    }

    #[test]
    fn a_live_pid_whose_log_is_gone_is_not_this_run() {
        let dir = scratch_dir("reuse");
        let log = dir.join("gone.log.tmp");
        let now = super::now_ms();
        fs::write(
            dir.join("1-0.json"),
            entry_json(&log, std::process::id(), now, ""),
        )
        .unwrap();
        let mut runs = Runs::new();
        let events = poll(&dir, &mut runs, now);
        assert!(events.iter().all(|e| e["event"] != "started"), "{events:?}");
    }

    #[cfg(unix)]
    #[test]
    fn an_ended_entry_that_cannot_be_removed_is_announced_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("stuck");
        let log = dir.join("x.log");
        fs::write(&log, "line\n").unwrap();
        let now = super::now_ms();
        fs::write(
            dir.join("1-0.json"),
            entry_json(&log, 0x7fff_fff0, now, r#","exit":1,"ended":1"#),
        )
        .unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let mut runs = Runs::new();
        let first = poll(&dir, &mut runs, now);
        let second = poll(&dir, &mut runs, now + 250);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(first.iter().any(|e| e["event"] == "ended"), "{first:?}");
        assert!(second.is_empty(), "announced again: {second:?}");
    }
}
