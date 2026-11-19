//! `relais programme claim|release|next` (SPEC §31): the claim store through
//! the real binary and, for the races, through the library with one lock
//! opened per thread. Every run uses a scratch `RELAIS_STATE_DIR`; nothing
//! here touches the real state directory.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

use chrono::NaiveDate;
use relais::programme::claim::{self, ClaimKey, Next, ProgrammeState, ReadyTicket, Store};
use relais::programme::model::Mode;
use relais::test_support::{short_temp_dir, TempDir};
use relais::workspace::git_command;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

const MAP: &str = "# Demo

## Destination

Ship it.

## Notes

None.

## Decisions so far

None.

## Not yet specified

None.

## Out of scope

None.
";

fn ticket(mode: &str, due: Option<&str>, name: &str) -> String {
    let due = due.map_or(String::new(), |d| format!("due = \"{d}\"\n"));
    format!(
        "+++\ntype = \"task\"\nmode = \"{mode}\"\nstatus = \"open\"\nblocked_by = []\n{due}+++\n# {name}\n\n## Question\n\nWhich?\n"
    )
}

fn git(dir: &Path, args: &[&str]) {
    let out = git_command(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct World {
    _scratch: TempDir,
    root: PathBuf,
    state: PathBuf,
}

impl World {
    /// `<root>/alpha` and `<root>/beta` are repositories; the programmes
    /// are added with [`World::programme`].
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("claims-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        for repo in ["alpha", "beta"] {
            std::fs::create_dir_all(root.join(repo)).expect("repo dir");
            git(&root.join(repo), &["init", "-q"]);
            git(
                &root.join(repo),
                &["commit", "-q", "--allow-empty", "-m", "init"],
            );
        }
        Self {
            _scratch: scratch,
            root,
            state,
        }
    }

    fn programme(&self, repo: &str, slug: &str, tickets: &[(&str, &str, Option<&str>)]) {
        let dir = self.root.join(repo).join("docs/programmes").join(slug);
        std::fs::create_dir_all(&dir).expect("programme dir");
        std::fs::write(dir.join("map.md"), MAP).expect("map");
        for (id, mode, due) in tickets {
            std::fs::write(dir.join(format!("{id}.md")), ticket(mode, *due, id)).expect("ticket");
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("RELAIS_") || name == "CLAUDE_CODE_SESSION_ID" {
                command.env_remove(&key);
            }
        }
        command
            .env("RELAIS_STATE_DIR", &self.state)
            .arg("programme")
            .args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("relais runs")
    }

    fn next(&self, session: &str, extra: &[&str]) -> Output {
        let root = self.root.to_str().expect("utf8");
        let mut args = vec!["next", "--root", root, "--session", session];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    fn claim(&self, repo: &str, slug: &str, id: &str, session: &str) -> Output {
        let dir = self.root.join(repo);
        self.run(&[
            "claim",
            slug,
            id,
            "--dir",
            dir.to_str().expect("utf8"),
            "--session",
            session,
        ])
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_claim_on_the_first_candidate_makes_next_take_the_second() {
    let world = World::new("second");
    world.programme(
        "alpha",
        "p",
        &[
            ("t1", "afk", None),
            ("t2", "afk", None),
            ("t3", "afk", None),
        ],
    );
    let held = world.claim("alpha", "p", "t1", "session-held-by-someone");
    assert_eq!(held.status.code(), Some(0), "{}", text(&held.stderr));

    let out = world.next("me", &["--from", "alpha:p", "--claim"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "alpha p t2\tt2\n");

    let json = world.next("me2", &["--from", "alpha:p", "--claim", "--json"]);
    assert!(
        text(&json.stdout).contains("\"id\":\"t3\""),
        "{}",
        text(&json.stdout)
    );

    let parked = world.next("me3", &["--from", "alpha:p", "--claim"]);
    assert_eq!(parked.status.code(), Some(22));
    assert_eq!(text(&parked.stdout), "parked: nothing due\n");
    let parked_json = world.next("me3", &["--from", "alpha:p", "--json"]);
    assert_eq!(parked_json.status.code(), Some(22));
    assert!(text(&parked_json.stdout).contains("\"parked\""));
}

#[test]
fn next_skips_the_persons_tickets_and_names_the_soonest_future_due_date() {
    let world = World::new("park");
    world.programme(
        "alpha",
        "p",
        &[
            ("person", "hitl", None),
            ("later", "afk", Some("2999-01-01")),
            ("soon", "afk", Some("2998-06-01")),
        ],
    );
    let out = world.next("me", &["--from", "alpha:p"]);
    assert_eq!(out.status.code(), Some(22));
    assert_eq!(text(&out.stdout), "parked: next due 2998-06-01\n");
}

#[test]
fn a_refused_claim_exits_3_with_an_empty_stdout_and_the_ways_forward() {
    let world = World::new("refuse");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    assert_eq!(
        world
            .claim("alpha", "p", "t1", "holder-session-1")
            .status
            .code(),
        Some(0)
    );
    let out = world.claim("alpha", "p", "t1", "other-session");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(text(&out.stdout), "");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("holder-s"), "{stderr}");
    assert!(!stderr.contains("holder-session-1"), "{stderr}");
    assert!(
        stderr.contains("next: relais programme next --from alpha:p --claim"),
        "{stderr}"
    );
    assert!(
        stderr.contains("relais programme claim p t1") && stderr.contains("--steal"),
        "{stderr}"
    );

    let dir = world.root.join("alpha");
    let stolen = world.run(&[
        "claim",
        "p",
        "t1",
        "--dir",
        dir.to_str().expect("utf8"),
        "--session",
        "other-session",
        "--steal",
    ]);
    assert_eq!(stolen.status.code(), Some(0), "{}", text(&stolen.stderr));

    let released = world.run(&[
        "release",
        "p",
        "t1",
        "--dir",
        dir.to_str().expect("utf8"),
        "--session",
        "other-session",
    ]);
    assert_eq!(released.status.code(), Some(0));
    assert_eq!(
        world.claim("alpha", "p", "t1", "third").status.code(),
        Some(0)
    );
}

#[test]
fn a_writing_command_without_a_session_exits_2_naming_both_ways_to_give_one() {
    let world = World::new("nosession");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    let dir = world.root.join("alpha");
    let out = world.run(&["claim", "p", "t1", "--dir", dir.to_str().expect("utf8")]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("--session") && stderr.contains("CLAUDE_CODE_SESSION_ID"),
        "{stderr}"
    );
}

#[test]
fn a_lock_that_stays_held_exits_1_naming_the_lock_and_the_retry() {
    let world = World::new("locked");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    let first = world.claim("alpha", "p", "t1", "s1");
    assert_eq!(first.status.code(), Some(0));
    let lock_path = std::fs::read_dir(world.state.join("claims"))
        .expect("claims dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| path.join("p/.lock"))
        .find(|path| path.is_file())
        .expect("the slug lock file");
    let _held = relais::procs::LockFile::try_acquire(&lock_path)
        .expect("opens")
        .expect("free");
    let out = world.claim("alpha", "p", "t1", "s2");
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains(lock_path.to_str().expect("utf8")),
        "{stderr}"
    );
    assert!(
        stderr.contains("next: relais programme claim p t1"),
        "{stderr}"
    );
}

#[test]
fn two_concurrent_takeovers_of_one_handoff_claim_give_one_winner() {
    let world = Arc::new(World::new("takeover"));
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    let reserved = world.next("old", &["--from", "alpha:p", "--claim"]);
    assert_eq!(reserved.status.code(), Some(0));
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["racer-a", "racer-b"]
        .into_iter()
        .map(|session| {
            let (world, barrier) = (Arc::clone(&world), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                world.claim("alpha", "p", "t1", session).status.code()
            })
        })
        .collect();
    let mut codes: Vec<Option<i32>> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .collect();
    codes.sort();
    assert_eq!(codes, [Some(0), Some(3)]);
}

fn state_of(repo: &str, slug: &str, ids: &[&str]) -> ProgrammeState {
    let ready: Vec<ReadyTicket> = ids
        .iter()
        .map(|id| ReadyTicket {
            id: (*id).to_string(),
            name: (*id).to_string(),
            due: None,
            mode: Mode::Afk,
        })
        .collect();
    ProgrammeState {
        repo: repo.to_string(),
        repo_path: format!("/r/{repo}"),
        slug: slug.to_string(),
        key: ClaimKey {
            repo_key: claim::repo_key(Path::new(repo)),
            slug: slug.to_string(),
        },
        open: ready.iter().map(|t| t.id.clone()).collect(),
        ready,
        closed: BTreeSet::new(),
        branch_held: BTreeSet::new(),
    }
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 19).expect("a date")
}

#[test]
fn eight_threads_over_fifty_rounds_never_hand_one_ticket_to_two_sessions() {
    let scratch = short_temp_dir("claims-race");
    for round in 0..50 {
        let state = scratch.to_path_buf().join(format!("round-{round}"));
        let programme = Arc::new(state_of("a", "p", &["t1", "t2", "t3", "t4", "t5"]));
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let (state, programme, barrier) =
                    (state.clone(), Arc::clone(&programme), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    // Each thread opens its own store, hence its own lock file description.
                    let store = Store::in_state_dir(&state);
                    barrier.wait();
                    claim::next(
                        &store,
                        Some(&programme),
                        &[],
                        today(),
                        1_800_000_000,
                        Some(&format!("s{n}")),
                    )
                    .expect("next")
                })
            })
            .collect();
        let picks: Vec<String> = handles
            .into_iter()
            .filter_map(|handle| match handle.join().expect("thread") {
                Next::Pick(pick) => Some(pick.id),
                Next::Parked { .. } => None,
            })
            .collect();
        let distinct: BTreeSet<&String> = picks.iter().collect();
        assert_eq!(distinct.len(), picks.len(), "round {round}: {picks:?}");
        assert_eq!(
            picks.len(),
            5,
            "round {round}: every ticket goes to someone"
        );
    }
}

#[test]
fn two_callers_from_exhausted_programmes_reserve_at_most_one_ticket_in_one_empty_one() {
    let scratch = short_temp_dir("claims-dest");
    for round in 0..20 {
        let state = scratch.to_path_buf().join(format!("round-{round}"));
        let destination = Arc::new(state_of("z", "empty", &["t1", "t2"]));
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["x", "y"]
            .into_iter()
            .map(|name| {
                let (state, destination, barrier) = (
                    state.clone(),
                    Arc::clone(&destination),
                    Arc::clone(&barrier),
                );
                std::thread::spawn(move || {
                    let store = Store::in_state_dir(&state);
                    let exhausted = state_of(name, "done", &[]);
                    barrier.wait();
                    claim::next(
                        &store,
                        Some(&exhausted),
                        std::slice::from_ref(&*destination),
                        today(),
                        1_800_000_000,
                        Some(name),
                    )
                    .expect("next")
                })
            })
            .collect();
        let picks = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .filter(|chosen| matches!(chosen, Next::Pick(_)))
            .count();
        assert_eq!(
            picks, 1,
            "round {round}: the empty programme is entered once"
        );
        let store = Store::in_state_dir(&state);
        let held = store
            .held_ids(&destination.key, 1_800_000_000)
            .expect("held");
        assert_eq!(held.len(), 1, "round {round}: {held:?}");
    }
}

fn programme_dir(world: &World, repo: &str, slug: &str) -> String {
    world
        .root
        .join(repo)
        .join("docs/programmes")
        .join(slug)
        .to_str()
        .expect("utf8")
        .to_string()
}

fn claimed_world(tag: &str) -> World {
    let world = World::new(tag);
    world.programme("alpha", "p", &[("t1", "afk", None), ("t2", "afk", None)]);
    let claimed = world.claim("alpha", "p", "t1", "abcdef0123456789");
    assert_eq!(claimed.status.code(), Some(0), "{}", text(&claimed.stderr));
    world
}

#[test]
fn frontier_leaves_a_claimed_ticket_out_and_all_shows_it_as_the_fifth_field() {
    let world = claimed_world("frontier-text");
    let dir = programme_dir(&world, "alpha", "p");

    let ready = world.run(&["frontier", &dir, "--no-fetch"]);
    assert_eq!(ready.status.code(), Some(0), "{}", text(&ready.stderr));
    let ready = text(&ready.stdout);
    assert!(ready.contains("t2\t") && !ready.contains("t1\t"), "{ready}");

    let all = world.run(&["frontier", &dir, "--no-fetch", "--all"]);
    let all = text(&all.stdout);
    let rows: Vec<Vec<&str>> = all.lines().map(|line| line.split('\t').collect()).collect();
    assert_eq!(rows.len(), 2, "{all}");
    assert!(rows.iter().all(|row| row.len() == 5), "five fields: {all}");
    let t1 = rows.iter().find(|row| row[0] == "t1").expect("t1");
    assert_eq!(t1[4], "claimed abcdef01 0m", "{all}");
    let t2 = rows.iter().find(|row| row[0] == "t2").expect("t2");
    assert_eq!(t2[4], "", "{all}");
}

#[test]
fn frontier_and_show_json_keep_in_flight_and_add_held_by() {
    let world = claimed_world("frontier-json");
    let dir = programme_dir(&world, "alpha", "p");

    let all = world.run(&["frontier", &dir, "--no-fetch", "--all", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&all.stdout).expect("json");
    let by_id = |id: &str| {
        rows.as_array()
            .expect("array")
            .iter()
            .find(|row| row["id"] == id)
            .expect("row")
            .clone()
    };
    assert_eq!(by_id("t1")["held_by"]["holder"], "claim");
    assert_eq!(by_id("t1")["held_by"]["session"], "abcdef0123456789");
    assert_eq!(by_id("t1")["held_by"]["kind"], "work");
    assert!(by_id("t1")["branch"].is_null());
    assert!(by_id("t2")["held_by"].is_null());

    let show = world.run(&["show", &dir, "--no-fetch", "--json"]);
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).expect("json");
    assert_eq!(shown["counts"]["in_flight"], 1);
    assert_eq!(shown["in_flight"][0]["id"], "t1");
    assert_eq!(
        shown["in_flight"][0]["held_by"]["session"],
        "abcdef0123456789"
    );

    let text_out = text(&world.run(&["show", &dir, "--no-fetch"]).stdout);
    assert!(
        text_out.contains("In flight\n  t1 (t1): claimed abcdef01 0m\n"),
        "{text_out}"
    );
}

#[test]
fn a_released_claim_lets_the_ticket_back_onto_the_frontier() {
    let world = claimed_world("release");
    let dir = world.root.join("alpha");
    let released = world.run(&[
        "release",
        "p",
        "t1",
        "--dir",
        dir.to_str().expect("utf8"),
        "--session",
        "abcdef0123456789",
    ]);
    assert_eq!(
        released.status.code(),
        Some(0),
        "{}",
        text(&released.stderr)
    );
    let ready = world.run(&[
        "frontier",
        &programme_dir(&world, "alpha", "p"),
        "--no-fetch",
    ]);
    assert!(
        text(&ready.stdout).contains("t1\t"),
        "{}",
        text(&ready.stdout)
    );
}
