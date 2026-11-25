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
use relais::programme::claim::{self, ClaimKey, Intent, Next, ProgrammeState, ReadyTicket, Store};
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
    let shape: serde_json::Value = serde_json::from_slice(&parked_json.stdout).expect("json");
    assert_eq!(
        shape,
        serde_json::json!({"parked": true, "next_due": null, "elsewhere": [], "elsewhere_partial": false})
    );
}

#[test]
fn an_exhausted_from_programme_parks_and_lists_the_others_in_order_without_claiming_there() {
    let world = World::new("elsewhere");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    world.programme("beta", "q", &[("q1", "afk", None)]);
    world.programme("beta", "r", &[("r1", "afk", None), ("r2", "afk", None)]);
    world.programme("alpha", "s", &[("s1", "afk", Some("2000-01-01"))]);
    let held = world.claim("alpha", "p", "t1", "someone-else");
    assert_eq!(held.status.code(), Some(0), "{}", text(&held.stderr));
    let before = claim_files(&world);

    let out = world.next("me", &["--from", "alpha:p", "--claim"]);
    assert_eq!(out.status.code(), Some(22), "{}", text(&out.stderr));
    // Soonest due first, then most ready, then name; pasteable into --from.
    assert_eq!(
        text(&out.stdout),
        "parked: nothing due\n\
         ready elsewhere: alpha:s (1 ready)\n\
         ready elsewhere: beta:r (2 ready)\n\
         ready elsewhere: beta:q (1 ready)\n"
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(claim_files(&world), before, "nothing is reserved elsewhere");

    let json = world.next("me", &["--from", "alpha:p", "--claim", "--json"]);
    assert_eq!(json.status.code(), Some(22));
    let shape: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(
        shape,
        serde_json::json!({
            "parked": true,
            "next_due": null,
            "elsewhere": [
                {"repo": "alpha", "slug": "s", "ready": 1},
                {"repo": "beta", "slug": "r", "ready": 2},
                {"repo": "beta", "slug": "q", "ready": 1},
            ],
            "elsewhere_partial": false,
        })
    );
    assert_eq!(claim_files(&world), before);
}

#[test]
fn a_busy_from_lock_exits_1_with_the_retry_and_claims_nothing_elsewhere() {
    let world = World::new("busy-from");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    world.programme("beta", "q", &[("q1", "afk", None)]);
    let first = world.claim("alpha", "p", "t1", "someone-else");
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    let before = claim_files(&world);
    let _held = relais::procs::LockFile::try_acquire(&slug_lock_path(&world, "p"))
        .expect("opens")
        .expect("free");
    let out = world.next("me", &["--from", "alpha:p", "--claim"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "");
    let stderr = text(&out.stderr);
    // The programme skill retries `next`'s exit 1 only on this wording.
    assert!(stderr.contains("stayed held for"), "{stderr}");
    assert!(
        stderr.contains("next: relais programme next --from alpha:p --claim --root"),
        "{stderr}"
    );
    // The retry is the same command: it claims for the same session.
    assert!(
        stderr.contains("--claim --root") && stderr.contains(" --session me"),
        "{stderr}"
    );
    let json = world.next("me", &["--from", "alpha:p", "--claim", "--json"]);
    assert_eq!(json.status.code(), Some(1), "{}", text(&json.stderr));
    assert!(
        text(&json.stderr).contains(" --session me --json"),
        "{}",
        text(&json.stderr)
    );
    assert_eq!(claim_files(&world), before);
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
    // The programme driver retries a claim's exit 1 only on this wording
    // (the programme skill's Drive section); a rewording turns every lock
    // wait into a stop.
    assert!(stderr.contains("stayed held for"), "{stderr}");
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
        open: ready
            .iter()
            .map(|t| (t.id.clone(), t.name.clone()))
            .collect(),
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
                        &relais::shared_state::node::NodeReservations(
                            &relais::shared_state::LocalState::new(),
                        ),
                        &store,
                        Some(&programme),
                        &[],
                        today(),
                        1_800_000_000,
                        Intent::Reserve {
                            session: &format!("s{n}"),
                        },
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
fn a_caller_from_an_exhausted_programme_reserves_nothing_in_an_empty_one() {
    let scratch = short_temp_dir("claims-from-none");
    let destination = state_of("z", "empty", &["t1", "t2"]);
    let exhausted = state_of("x", "done", &[]);
    let store = Store::in_state_dir(&scratch);
    let chosen = claim::next(
        &relais::shared_state::node::NodeReservations(&relais::shared_state::LocalState::new()),
        &store,
        Some(&exhausted),
        std::slice::from_ref(&destination),
        today(),
        1_800_000_000,
        Intent::Reserve { session: "x" },
    )
    .expect("next");
    assert!(matches!(chosen, Next::Parked { .. }), "{chosen:?}");
    assert!(store
        .held_ids(&destination.key, 1_800_000_000)
        .expect("held")
        .is_empty());
}

#[test]
fn two_callers_without_from_reserve_at_most_one_ticket_in_one_empty_one() {
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
                    barrier.wait();
                    claim::next(
                        &relais::shared_state::node::NodeReservations(
                            &relais::shared_state::LocalState::new(),
                        ),
                        &store,
                        None,
                        std::slice::from_ref(&*destination),
                        today(),
                        1_800_000_000,
                        Intent::Reserve { session: name },
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

fn slug_lock_path(world: &World, slug: &str) -> PathBuf {
    std::fs::read_dir(world.state.join("claims"))
        .expect("claims dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| path.join(slug).join(".lock"))
        .find(|path| path.is_file())
        .expect("the slug lock file")
}

#[test]
fn a_claim_prints_the_ticket_as_repo_slug_id_on_stderr() {
    let world = World::new("wording");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    let first = world.claim("alpha", "p", "t1", "s1");
    assert!(
        text(&first.stderr).contains("relais: claimed alpha p t1\n"),
        "{}",
        text(&first.stderr)
    );
    let again = world.claim("alpha", "p", "t1", "s1");
    assert!(
        text(&again.stderr).contains("relais: refreshed your claim on alpha p t1\n"),
        "{}",
        text(&again.stderr)
    );
    let refused = world.claim("alpha", "p", "t1", "s2");
    let stderr = text(&refused.stderr);
    assert!(
        stderr.contains("relais: alpha p t1 is held by s1 ("),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "to take it over (only when the person says so):\nrelais programme claim p t1"
        ),
        "{stderr}"
    );
}

#[test]
fn next_claim_run_twice_by_one_session_names_the_same_ticket() {
    let world = World::new("idempotent");
    world.programme("alpha", "p", &[("t1", "afk", None), ("t2", "afk", None)]);
    let first = world.next("me", &["--from", "alpha:p", "--claim"]);
    let second = world.next("me", &["--from", "alpha:p", "--claim"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert_eq!(text(&first.stdout), "alpha p t1\tt1\n");
    assert_eq!(text(&second.stdout), text(&first.stdout));
    let other = world.next("you", &["--from", "alpha:p", "--claim"]);
    assert_eq!(text(&other.stdout), "alpha p t2\tt2\n");
}

#[test]
fn a_busy_lock_in_another_programme_leaves_next_claim_at_exit_0_with_its_pick() {
    let world = World::new("busy-refresh");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    world.programme("beta", "q", &[("b1", "afk", None)]);
    let own = world.claim("beta", "q", "b1", "me");
    assert_eq!(own.status.code(), Some(0), "{}", text(&own.stderr));
    let _held = relais::procs::LockFile::try_acquire(&slug_lock_path(&world, "q"))
        .expect("opens")
        .expect("free");
    let out = world.next("me", &["--from", "alpha:p", "--claim"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "alpha p t1\tt1\n");
    assert!(
        text(&out.stderr).contains("warning"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_from_programme_that_fails_to_load_exits_1_and_only_a_failed_load_without_from_refuses_to_park()
{
    let world = World::new("broken");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    std::fs::write(
        world.root.join("alpha/docs/programmes/p/t9.md"),
        [0xff, 0xfe, 0xfd],
    )
    .expect("a ticket that does not read");
    let from_broken = world.next("me", &["--from", "alpha:p"]);
    assert_eq!(
        from_broken.status.code(),
        Some(1),
        "{}",
        text(&from_broken.stderr)
    );
    assert_eq!(text(&from_broken.stdout), "");

    // Another programme failing to load cannot hide a ticket of beta:q, so
    // the park stands, flagged partial.
    world.programme("beta", "q", &[("person", "hitl", None)]);
    let parks = world.next("me", &["--from", "beta:q"]);
    assert_eq!(parks.status.code(), Some(22), "{}", text(&parks.stderr));
    assert_eq!(text(&parks.stdout), "parked: nothing due\n");
    assert!(
        text(&parks.stderr).contains(
            "relais: the ready-elsewhere list may be partial: a programme failed to load (see above)"
        ),
        "{}",
        text(&parks.stderr)
    );
    let json = world.next("me", &["--from", "beta:q", "--json"]);
    assert_eq!(json.status.code(), Some(22));
    let shape: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(shape["elsewhere_partial"], serde_json::json!(true));

    let would_park = world.next("me", &[]);
    assert_eq!(
        would_park.status.code(),
        Some(1),
        "{}",
        text(&would_park.stderr)
    );
    assert_eq!(text(&would_park.stdout), "");
}

#[test]
fn two_next_claim_processes_started_together_never_share_a_ticket_and_the_second_parks() {
    for round in 0..5 {
        let world = Arc::new(World::new(&format!("procs-{round}")));
        world.programme("alpha", "alpha", &[("a1", "afk", None)]);
        world.programme("beta", "beta", &[("b1", "afk", None)]);
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["racer-a", "racer-b"]
            .into_iter()
            .map(|session| {
                let (world, barrier) = (Arc::clone(&world), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    world.next(session, &["--from", "alpha:alpha", "--claim"])
                })
            })
            .collect();
        let outputs: Vec<Output> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();
        let mut results: Vec<(Option<i32>, String)> = outputs
            .iter()
            .map(|out| (out.status.code(), text(&out.stdout)))
            .collect();
        results.sort();
        assert_eq!(
            results,
            [
                (Some(0), "alpha alpha a1\ta1\n".to_string()),
                (
                    Some(22),
                    "parked: nothing due\nready elsewhere: beta:beta (1 ready)\n".to_string()
                ),
            ],
            "round {round}: {}",
            outputs.iter().map(|o| text(&o.stderr)).collect::<String>()
        );
    }
}

// --- f12: a relayed session holds its ticket (`relais programme adopt`) ---

/// Every claim file under the state directory.
fn claim_files(world: &World) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for repo in std::fs::read_dir(world.state.join("claims")).expect("claims dir") {
        let repo = repo.expect("entry").path();
        if !repo.is_dir() || repo.file_name().is_some_and(|name| name == "ended") {
            continue;
        }
        for slug in std::fs::read_dir(&repo).expect("repo dir") {
            let slug = slug.expect("entry").path();
            if !slug.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(&slug).expect("slug dir") {
                let file = file.expect("entry").path();
                if file.extension().is_some_and(|ext| ext == "toml") {
                    files.push(file);
                }
            }
        }
    }
    files.sort();
    files
}

impl World {
    /// Moves every claim `secs` into the past, as a clock moving forward
    /// would: the CLI reads the wall clock, so the claims move instead.
    fn age_claims(&self, secs: i64) {
        for file in claim_files(self) {
            let text = std::fs::read_to_string(&file).expect("claim");
            let mut claim: toml::Table = toml::from_str(&text).expect("claim parses");
            for field in ["claimed_at", "refreshed_at"] {
                let at = claim[field].as_integer().expect("a timestamp");
                claim.insert(field.to_string(), toml::Value::Integer(at - secs));
            }
            std::fs::write(&file, toml::to_string(&claim).expect("encodes")).expect("write");
        }
    }

    fn adopt(&self, previous: &str, session: &str) -> Output {
        self.run(&["adopt", "--previous", previous, "--session", session])
    }
}

const OLD: &str = "aaaaaaaa-old-session";
const NEW: &str = "bbbbbbbb-new-session";

#[test]
fn a_relayed_session_that_adopts_its_handoff_keeps_the_ticket_past_the_handoff_hold() {
    let world = World::new("adopt-1121");
    world.programme("alpha", "p", &[("t19", "afk", None), ("t20", "afk", None)]);
    // 05:21: A reserves t19 for its successor and clears.
    let reserved = world.next(OLD, &["--from", "alpha:p", "--claim"]);
    assert_eq!(
        text(&reserved.stdout),
        "alpha p t19\tt19\n",
        "{}",
        text(&reserved.stderr)
    );
    // 05:22: B starts on the relayed prompt; the plugin adopts, B's agent claims nothing.
    let adopted = world.adopt(OLD, NEW);
    assert_eq!(adopted.status.code(), Some(0), "{}", text(&adopted.stderr));
    assert_eq!(text(&adopted.stdout), "");
    assert_eq!(
        text(&adopted.stderr),
        "relais: took over alpha p t19 from aaaaaaaa\n"
    );
    // Three hours on, past the 2 h handoff hold.
    world.age_claims(3 * 3600);
    // 08:36: another session's boundary does not reserve t19.
    let other = world.next("cccccccc-other", &["--from", "alpha:p", "--claim"]);
    assert_eq!(
        text(&other.stdout),
        "alpha p t20\tt20\n",
        "{}",
        text(&other.stderr)
    );
    // And a direct claim on t19 is refused: B holds it.
    let refused = world.claim("alpha", "p", "t19", "dddddddd-relay");
    assert_eq!(refused.status.code(), Some(3), "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("alpha p t19 is held by bbbbbbbb ("),
        "{}",
        text(&refused.stderr)
    );
}

#[test]
fn a_successor_that_claims_itself_still_takes_the_handoff_over_and_adopt_then_finds_nothing() {
    let world = World::new("adopt-claim");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    world.next(OLD, &["--from", "alpha:p", "--claim"]);
    let claimed = world.claim("alpha", "p", "t1", NEW);
    assert_eq!(claimed.status.code(), Some(0), "{}", text(&claimed.stderr));
    assert!(
        text(&claimed.stderr).contains("relais: took over alpha p t1 from aaaaaaaa\n"),
        "{}",
        text(&claimed.stderr)
    );
    let adopted = world.adopt(OLD, NEW);
    assert_eq!(adopted.status.code(), Some(0), "{}", text(&adopted.stderr));
    assert_eq!(text(&adopted.stderr), "");
    let again = world.claim("alpha", "p", "t1", NEW);
    assert!(
        text(&again.stderr).contains("refreshed your claim on alpha p t1"),
        "{}",
        text(&again.stderr)
    );
}

#[test]
fn a_handoff_another_session_took_over_stays_with_it_and_adopt_is_silent_twice() {
    let world = World::new("adopt-taken");
    world.programme("alpha", "p", &[("t1", "afk", None)]);
    world.next(OLD, &["--from", "alpha:p", "--claim"]);
    let taken = world.claim("alpha", "p", "t1", "eeeeeeee-first");
    assert_eq!(taken.status.code(), Some(0), "{}", text(&taken.stderr));
    for _ in 0..2 {
        let adopted = world.adopt(OLD, NEW);
        assert_eq!(adopted.status.code(), Some(0), "{}", text(&adopted.stderr));
        assert_eq!(text(&adopted.stdout), "");
        assert_eq!(text(&adopted.stderr), "");
    }
    let refused = world.claim("alpha", "p", "t1", NEW);
    assert_eq!(refused.status.code(), Some(3), "{}", text(&refused.stderr));
}

/// A second handoff of `OLD` beside the first, as a copy of its claim file
/// under `id`, with `repo` changed when one is given.
fn copy_handoff(world: &World, id: &str, repo: Option<&Path>) {
    let first = claim_files(world).pop().expect("one claim file");
    let mut claim: toml::Table =
        toml::from_str(&std::fs::read_to_string(&first).expect("claim")).expect("parses");
    if let Some(repo) = repo {
        claim.insert(
            "repo".to_string(),
            toml::Value::String(repo.to_str().expect("utf8").to_string()),
        );
    }
    let path = first.with_file_name(format!("{id}.toml"));
    std::fs::write(path, toml::to_string(&claim).expect("encodes")).expect("write");
}

#[test]
fn adopt_takes_the_open_handoff_and_skips_the_one_on_a_closed_ticket() {
    let world = World::new("adopt-closed");
    world.programme("alpha", "p", &[("t1", "afk", None), ("t2", "afk", None)]);
    world.next(OLD, &["--from", "alpha:p", "--claim"]);
    copy_handoff(&world, "t2", None);
    copy_handoff(&world, "t3", None);
    let t1 = world.root.join("alpha/docs/programmes/p/t1.md");
    let closed = std::fs::read_to_string(&t1)
        .expect("ticket")
        .replace("status = \"open\"", "status = \"closed\"");
    std::fs::write(&t1, closed).expect("close t1");
    let adopted = world.adopt(OLD, NEW);
    assert_eq!(adopted.status.code(), Some(0), "{}", text(&adopted.stderr));
    let stderr = text(&adopted.stderr);
    assert!(
        stderr.contains("relais: alpha p t1 skipped: the ticket is closed\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("relais: alpha p t3 not adopted: no such ticket\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("relais: took over alpha p t2 from aaaaaaaa\n"),
        "{stderr}"
    );
}

#[test]
fn a_handoff_recorded_in_a_removed_worktree_is_adopted_through_the_primary_checkout() {
    let world = World::new("adopt-wt");
    world.programme("alpha", "p", &[("t1", "afk", None), ("t2", "afk", None)]);
    world.next(OLD, &["--from", "alpha:p", "--claim"]);
    copy_handoff(&world, "t2", Some(&world.root.join("alpha-wt-gone")));
    let adopted = world.adopt(OLD, NEW);
    assert_eq!(adopted.status.code(), Some(0), "{}", text(&adopted.stderr));
    let stderr = text(&adopted.stderr);
    assert!(
        stderr.contains("relais: took over alpha p t1 from aaaaaaaa\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("relais: took over alpha p t2 from aaaaaaaa\n"),
        "{stderr}"
    );
}

#[test]
fn a_handoff_whose_repository_is_gone_is_not_adopted_and_adopt_exits_0() {
    let world = World::new("adopt-gone");
    world.programme("alpha", "p", &[("t1", "afk", None), ("t2", "afk", None)]);
    world.next(OLD, &["--from", "alpha:p", "--claim"]);
    copy_handoff(&world, "t2", Some(&world.root.join("nowhere")));
    let adopted = world.adopt(OLD, NEW);
    assert_eq!(adopted.status.code(), Some(0), "{}", text(&adopted.stderr));
    let stderr = text(&adopted.stderr);
    assert!(
        stderr.contains("relais: took over alpha p t1 from aaaaaaaa\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("p t2 not adopted: its repository is not found"),
        "{stderr}"
    );
}

#[test]
fn adopt_refuses_an_empty_previous_session_or_one_equal_to_its_own() {
    let world = World::new("adopt-usage");
    let empty = world.adopt("  ", NEW);
    assert_eq!(empty.status.code(), Some(2));
    assert!(
        text(&empty.stderr).contains("--previous is empty"),
        "{}",
        text(&empty.stderr)
    );
    let out = world.adopt(NEW, NEW);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("--previous is this session"),
        "{}",
        text(&out.stderr)
    );
}
