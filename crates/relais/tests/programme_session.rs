//! `relais programme session --json`: which ticket a session holds by claim,
//! through the real binary on a fixture repository. Every run uses a scratch
//! `RELAIS_STATE_DIR`. The shape the plugin reads is pinned by
//! `claude-plugin/tests/fixtures/programme-session.golden.json`; run with
//! `UPDATE_GOLDEN=1` to rewrite it.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use relais::test_support::{short_temp_dir, TempDir};
use relais::workspace::git_command;
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_relais");

const MAP: &str = "# Demo\n\n## Destination\n\nShip it.\n\n## Notes\n\nNone.\n\n## Decisions so far\n\nNone.\n\n## Not yet specified\n\nNone.\n\n## Out of scope\n\nNone.\n";

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

fn ticket(status: &str, blocked_by: &str, due: Option<&str>, name: &str) -> String {
    let due = due.map_or(String::new(), |d| format!("due = \"{d}\"\n"));
    format!(
        "+++\ntype = \"task\"\nmode = \"afk\"\nstatus = \"{status}\"\nblocked_by = [{blocked_by}]\n{due}+++\n# {name}\n\n## Question\n\nWhich?\n"
    )
}

struct World {
    _scratch: TempDir,
    root: PathBuf,
    state: PathBuf,
}

impl World {
    /// `<root>/alpha` holds the programme `p`: t1 closed, t2 open behind t1,
    /// t3 behind t2, t4 ready with a due date, t5 out of scope.
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("session-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        let repo = root.join("alpha");
        let dir = repo.join("docs/programmes/p");
        std::fs::create_dir_all(&dir).expect("programme dir");
        git(&repo, &["init", "-q"]);
        std::fs::write(dir.join("map.md"), MAP).expect("map");
        let tickets = [
            ("t1", ticket("closed", "", None, "First")),
            ("t2", ticket("open", "\"t1\"", None, "Second")),
            ("t3", ticket("open", "\"t2\"", None, "Third")),
            ("t4", ticket("open", "", Some("2026-12-01"), "Fourth")),
            ("t5", ticket("out-of-scope", "", None, "Fifth")),
        ];
        for (id, text) in tickets {
            std::fs::write(dir.join(format!("{id}.md")), text).expect("ticket");
        }
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self {
            _scratch: scratch,
            root,
            state,
        }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("alpha")
    }

    fn run(&self, args: &[&str]) -> Output {
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
            .args(args)
            .output()
            .expect("relais runs")
    }

    fn claim(&self, id: &str, session: &str) {
        let repo = self.repo();
        let out = self.run(&[
            "claim",
            "p",
            id,
            "--dir",
            repo.to_str().expect("utf8"),
            "--session",
            session,
        ]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    }

    /// Rewrites the recorded repository path of every claim file.
    fn record_repo(&self, recorded: &Path) {
        fn walk(dir: &Path, recorded: &str) {
            for entry in std::fs::read_dir(dir).expect("read dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    walk(&path, recorded);
                } else if path.extension().is_some_and(|ext| ext == "toml") {
                    let text = std::fs::read_to_string(&path).expect("claim");
                    let rewritten: Vec<String> = text
                        .lines()
                        .map(|line| {
                            if line.starts_with("repo = ") {
                                format!("repo = {recorded:?}")
                            } else {
                                line.to_string()
                            }
                        })
                        .collect();
                    std::fs::write(&path, rewritten.join("\n") + "\n").expect("rewritten");
                }
            }
        }
        walk(&self.state.join("claims"), recorded.to_str().expect("utf8"));
    }

    fn session(&self, extra: &[&str]) -> Output {
        let repo = self.repo();
        let mut args = vec![
            "session",
            "--no-fetch",
            "--dir",
            repo.to_str().expect("utf8"),
        ];
        args.extend_from_slice(extra);
        self.run(&args)
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn document(out: &Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert_eq!(stdout.lines().count(), 1, "one JSON line: {stdout}");
    serde_json::from_str(&stdout).expect("stdout is one JSON document")
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../claude-plugin/tests/fixtures/programme-session.golden.json")
}

#[test]
fn a_work_claim_answers_with_the_programme_its_tickets_and_the_ready_list() {
    let world = World::new("golden");
    world.claim("t2", "sess-own");
    let answer = document(&world.session(&["--session", "sess-own", "--json"]));

    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let mut pretty = serde_json::to_string_pretty(&answer).expect("encodes");
        pretty.push('\n');
        std::fs::write(&path, pretty).expect("golden written");
    }
    let golden_text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing: run with UPDATE_GOLDEN=1 to write it",
            path.display()
        )
    });
    let golden: Value = serde_json::from_str(&golden_text).expect("json");
    assert_eq!(answer, golden);

    let programme = &answer["programme"];
    assert_eq!(programme["holder"], "self");
    assert_eq!(programme["kind"], "work");
    assert_eq!(programme["ticket"], "t2");
    assert_eq!(programme["ready"], json!(["t4"]));
    let status = |id: &str| {
        programme["tickets"]
            .as_array()
            .expect("tickets")
            .iter()
            .find(|t| t["id"] == id)
            .map(|t| t["status"].clone())
            .expect("ticket listed")
    };
    assert_eq!(status("t1"), "closed");
    assert_eq!(status("t2"), "in_flight");
    assert_eq!(status("t3"), "blocked");
    assert_eq!(status("t4"), "frontier");
    assert_eq!(status("t5"), "out_of_scope");
}

#[test]
fn no_claim_answers_null_with_a_reason_and_text_says_so() {
    let world = World::new("none");
    let answer = document(&world.session(&["--session", "nobody", "--json"]));
    assert_eq!(answer, json!({"programme": null, "reason": "none"}));
    let out = world.session(&["--session", "nobody"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "no programme claim\n");
}

#[test]
fn text_output_is_one_line_with_the_slug_ticket_and_kind() {
    let world = World::new("text");
    world.claim("t2", "sess-own");
    let out = world.session(&["--session", "sess-own"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "p t2 (work)\n");
}

#[test]
fn a_handoff_of_the_previous_session_answers_as_previous() {
    let world = World::new("previous");
    let root = world.root.to_str().expect("utf8");
    let next = world.run(&["next", "--root", root, "--session", "sess-old", "--claim"]);
    assert_eq!(next.status.code(), Some(0), "{}", text(&next.stderr));
    let answer =
        document(&world.session(&["--session", "sess-new", "--previous", "sess-old", "--json"]));
    assert_eq!(answer["programme"]["holder"], "previous");
    assert_eq!(answer["programme"]["kind"], "handoff");
    let without = document(&world.session(&["--session", "sess-new", "--json"]));
    assert_eq!(without["programme"], Value::Null);
}

#[test]
fn a_gone_repository_answers_the_error_shape_and_exit_0() {
    let world = World::new("gone");
    world.claim("t2", "sess-own");
    let moved = world.root.join("alpha-moved");
    std::fs::rename(world.repo(), &moved).expect("moved");
    let out = world.run(&[
        "session",
        "--no-fetch",
        "--session",
        "sess-own",
        "--dir",
        world.root.to_str().expect("utf8"),
        "--json",
    ]);
    let answer = document(&out);
    let programme = &answer["programme"];
    assert_eq!(programme["repo"], "alpha");
    assert_eq!(programme["slug"], "p");
    assert_eq!(programme["ticket"], "t2");
    assert_eq!(programme["kind"], "work");
    assert!(programme["error"].is_string(), "{answer}");
    assert!(programme.get("tickets").is_none(), "{answer}");
}

#[test]
fn a_claimed_ticket_missing_from_the_map_answers_the_error_shape() {
    let world = World::new("missing");
    world.claim("t2", "sess-own");
    std::fs::remove_file(world.repo().join("docs/programmes/p/t2.md")).expect("removed");
    let answer = document(&world.session(&["--session", "sess-own", "--json"]));
    assert_eq!(answer["programme"]["ticket"], "t2");
    assert!(answer["programme"]["error"].is_string(), "{answer}");
}

#[test]
fn without_a_session_id_the_command_exits_2_with_an_empty_stdout() {
    let world = World::new("usage");
    let out = world.session(&["--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("relais: no session id"), "{stderr}");
    assert!(stderr.contains("next: pass --session <id>"), "{stderr}");
    let bad = world.session(&["--bogus"]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn an_unreadable_claim_store_exits_1_with_an_empty_stdout() {
    let world = World::new("store");
    let claims = world.state.join("claims");
    std::fs::create_dir_all(claims.join("k/p")).expect("dirs");
    std::fs::create_dir_all(claims.join("k/p/t1.toml")).expect("a directory named like a claim");
    let out = world.session(&["--session", "sess-own", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
}

#[test]
fn a_dir_in_another_repository_falls_through_to_the_claims_own_repository() {
    let world = World::new("other-dir");
    world.claim("t2", "sess-own");
    let beta = world.root.join("beta");
    std::fs::create_dir_all(&beta).expect("beta");
    git(&beta, &["init", "-q"]);
    let out = world.run(&[
        "session",
        "--no-fetch",
        "--session",
        "sess-own",
        "--dir",
        beta.to_str().expect("utf8"),
        "--json",
    ]);
    let answer = document(&out);
    assert_eq!(answer["programme"]["repo"], "alpha");
    assert_eq!(answer["programme"]["ready"], json!(["t4"]));
}

#[test]
fn a_work_claim_wins_over_a_handoff_held_at_the_same_time() {
    let world = World::new("both");
    let root = world.root.to_str().expect("utf8");
    let next = world.run(&["next", "--root", root, "--session", "sess-own", "--claim"]);
    assert_eq!(next.status.code(), Some(0), "{}", text(&next.stderr));
    assert!(text(&next.stdout).contains("t2"), "{}", text(&next.stdout));
    world.claim("t4", "sess-own");
    let answer = document(&world.session(&["--session", "sess-own", "--json"]));
    assert_eq!(answer["programme"]["kind"], "work");
    assert_eq!(answer["programme"]["ticket"], "t4");
}

#[test]
fn a_recorded_worktree_without_the_map_resolves_to_the_main_checkout() {
    let world = World::new("recorded-wt");
    world.claim("t2", "sess-own");
    let worktree = world.root.join("alpha-wt");
    git(
        &world.repo(),
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            worktree.to_str().expect("utf8"),
        ],
    );
    std::fs::remove_dir_all(worktree.join("docs/programmes/p")).expect("map removed");
    world.record_repo(&worktree);
    let out = world.run(&[
        "session",
        "--no-fetch",
        "--session",
        "sess-own",
        "--dir",
        world.root.to_str().expect("utf8"),
        "--json",
    ]);
    let answer = document(&out);
    assert_eq!(answer["programme"]["ready"], json!(["t4"]), "{answer}");
}

#[test]
fn a_removed_recorded_worktree_is_resolved_by_dir() {
    let world = World::new("removed-wt");
    world.claim("t2", "sess-own");
    world.record_repo(&world.root.join("alpha-removed-wt"));
    let answer = document(&world.session(&["--session", "sess-own", "--json"]));
    assert_eq!(answer["programme"]["ready"], json!(["t4"]), "{answer}");
}

#[test]
fn text_mode_with_an_unreadable_map_prints_the_answer_line_and_exits_1() {
    let world = World::new("text-failed");
    world.claim("t2", "sess-own");
    std::fs::remove_file(world.repo().join("docs/programmes/p/t2.md")).expect("removed");
    let out = world.session(&["--session", "sess-own"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "p t2 (work)\n");
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("relais: alpha p: ticket t2 is not in the map"),
        "{stderr}"
    );
    assert!(
        stderr.contains("next: fix the map at ")
            && stderr.contains("/docs/programmes/p")
            && stderr.contains("relais programme release p t2 --session sess-own"),
        "{stderr}"
    );
}

#[test]
fn text_mode_says_when_the_claim_expired() {
    let world = World::new("text-expired");
    world.claim("t2", "sess-own");
    let claims = world.state.join("claims");
    rewrite_timestamps(&claims, STALE_UNIX_SECONDS);
    let out = world.session(&["--session", "sess-own"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "programme claim expired\n");
}

/// Long past any hold: September 2001.
const STALE_UNIX_SECONDS: i64 = 1_000_000_000;

fn rewrite_timestamps(dir: &Path, seconds: i64) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rewrite_timestamps(&path, seconds);
        } else if path.extension().is_some_and(|ext| ext == "toml") {
            let text = std::fs::read_to_string(&path).expect("claim");
            let rewritten: Vec<String> = text
                .lines()
                .map(|line| {
                    if line.starts_with("claimed_at = ") {
                        format!("claimed_at = {seconds}")
                    } else if line.starts_with("refreshed_at = ") {
                        format!("refreshed_at = {seconds}")
                    } else {
                        line.to_string()
                    }
                })
                .collect();
            std::fs::write(&path, rewritten.join("\n") + "\n").expect("rewritten");
        }
    }
}

#[test]
fn text_mode_with_a_gone_repository_names_a_checkout_for_both_choices() {
    let world = World::new("text-gone");
    world.claim("t2", "sess-own");
    let gone = world.root.join("moved-away");
    std::fs::rename(world.repo(), &gone).expect("moved");
    let out = world.session(&["--session", "sess-own"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "p t2 (work)\n");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("the repository is gone"), "{stderr}");
    assert!(
        stderr.contains("next: rerun with --dir <a checkout of alpha>, or relais programme release p t2 --session sess-own --dir <a checkout of alpha>"),
        "{stderr}"
    );
}
