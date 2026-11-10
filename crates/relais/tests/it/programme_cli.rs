//! `relais programme list|frontier|show|check` through the real binary
//! (SPEC §31): a map and its tickets in a real git repository, queried the
//! way a session queries them. Every command runs with `RELAIS_STATE_DIR`
//! pointing at an empty read-only directory, which proves none of them
//! opens the ledger.

use crate::common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use relais::test_support::{short_temp_dir, TempDir};
use relais::workspace::git_command;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

const MAP: &str = "# Demo programme

## Destination

Ship the demo.
Then stop.

## Notes

Nothing yet.

## Decisions so far

- [Choose the store](A.md)

## Not yet specified

Nothing yet.

## Out of scope

- [A mobile app](E.md)
";

fn ticket(
    status: &str,
    blocked_by: &str,
    repos: Option<&str>,
    name: &str,
    resolution: Option<&str>,
) -> String {
    let repos = repos.map_or(String::new(), |r| format!("repos = {r}\n"));
    let resolution =
        resolution.map_or(String::new(), |text| format!("\n## Resolution\n\n{text}\n"));
    format!(
        "+++\ntype = \"task\"\nmode = \"afk\"\nstatus = \"{status}\"\nblocked_by = {blocked_by}\n{repos}+++\n# {name}\n\n## Question\n\nWhich one?\n\n## Look at\n\nThe code.\n\n## Answer looks like\n\nA name.\n{resolution}"
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
    /// `<root>/alpha` holds the demo programme (A closed, B blocked by A,
    /// C blocked by D, D open, E out of scope); `<root>/other` is a second
    /// repository.
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("prog-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555))
            .expect("read-only state");
        for repo in ["alpha", "other"] {
            std::fs::create_dir_all(root.join(repo)).expect("repo dir");
            git(&root.join(repo), &["init", "-q"]);
            git(
                &root.join(repo),
                &["commit", "-q", "--allow-empty", "-m", "init"],
            );
        }
        let world = Self {
            _scratch: scratch,
            root,
            state,
        };
        std::fs::create_dir_all(world.dir()).expect("programme dir");
        world.write("map.md", MAP);
        world.write(
            "A.md",
            &ticket("closed", "[]", None, "Choose the store", Some("Decided.")),
        );
        world.write("B.md", &ticket("open", "[\"A\"]", None, "Name of B", None));
        world.write("C.md", &ticket("open", "[\"D\"]", None, "Name of C", None));
        world.write("D.md", &ticket("open", "[]", None, "Name of D", None));
        world.write(
            "E.md",
            &ticket("out-of-scope", "[]", None, "A mobile app", None),
        );
        world
    }

    fn dir(&self) -> PathBuf {
        self.root.join("alpha/docs/programmes/demo")
    }

    fn write(&self, file: &str, text: &str) {
        std::fs::write(self.dir().join(file), text).expect("write");
    }

    fn relais(&self, args: &[&str]) -> Output {
        common::relais(BIN)
            .env("RELAIS_STATE_DIR", &self.state)
            .args(args)
            .output()
            .expect("relais runs")
    }

    fn programme(&self, args: &[&str]) -> Output {
        let dir = self.dir();
        let mut all = vec!["programme", args[0], dir.to_str().expect("utf8")];
        all.extend_from_slice(&args[1..]);
        self.relais(&all)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // Writable again so the scratch directory can be removed.
        let writable = std::fs::Permissions::from_mode(0o755);
        drop(std::fs::set_permissions(&self.state, writable)); // a failure only leaves the scratch directory behind
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ids(stdout: &[u8]) -> Vec<String> {
    text(stdout)
        .lines()
        .map(|line| line.split('\t').next().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn the_frontier_lists_ready_tickets_and_follows_branches_and_resolutions() {
    let world = World::new("frontier");
    let out = world.programme(&["frontier"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "B\ttask/afk\talpha\tName of B\nD\ttask/afk\talpha\tName of D\n"
    );
    assert_eq!(ids(&out.stdout), ["B", "D"]);
    assert_eq!(text(&out.stderr), "");

    git(&world.root.join("alpha"), &["branch", "prog/demo/D"]);
    let out = world.programme(&["frontier"]);
    assert_eq!(ids(&out.stdout), ["B"]);
    let out = world.programme(&["frontier", "--all"]);
    assert_eq!(
        text(&out.stdout),
        "B\ttask/afk\talpha\tName of B\t\nD\ttask/afk\talpha\tName of D\tin-flight prog/demo/D 0m\n"
    );

    git(
        &world.root.join("alpha"),
        &["update-ref", "refs/remotes/origin/prog/demo/B", "HEAD"],
    );
    let out = world.programme(&["frontier"]);
    assert_eq!(text(&out.stdout), "");
    assert!(world.programme(&["check"]).status.success());

    git(
        &world.root.join("alpha"),
        &["update-ref", "-d", "refs/remotes/origin/prog/demo/B"],
    );
    git(&world.root.join("alpha"), &["branch", "-D", "prog/demo/D"]);
    world.write(
        "D.md",
        &ticket("closed", "[]", None, "Name of D", Some("Decided.")),
    );
    world.write(
        "map.md",
        &MAP.replace(
            "- [Choose the store](A.md)\n",
            "- [Choose the store](A.md)\n- [Name of D](D.md)\n",
        ),
    );
    let out = world.programme(&["frontier"]);
    assert_eq!(ids(&out.stdout), ["B", "C"]);
    assert_eq!(world.programme(&["check"]).status.code(), Some(0));
}

#[test]
fn an_empty_frontier_says_why_on_stderr_and_exits_zero() {
    let world = World::new("empty");
    git(&world.root.join("alpha"), &["branch", "prog/demo/B"]);
    git(&world.root.join("alpha"), &["branch", "prog/demo/D"]);
    let out = world.programme(&["frontier"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        format!(
            "no open ticket is ready: 1 blocked, 2 in flight\nnext: relais programme show {}\n",
            world.dir().display()
        )
    );
    let out = world.programme(&["frontier", "--json"]);
    assert_eq!(text(&out.stdout), "[]\n");
}

#[test]
fn frontier_json_carries_the_ticket_fields() {
    let world = World::new("json");
    let out = world.programme(&["frontier", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(rows[0]["id"], "B");
    assert_eq!(rows[0]["type"], "task");
    assert_eq!(rows[0]["repos"][0], "alpha");
    assert_eq!(rows[1]["name"], "Name of D");
}

#[test]
fn show_gives_the_destination_counts_blockers_and_branches() {
    let world = World::new("show");
    git(&world.root.join("alpha"), &["branch", "prog/demo/D"]);
    let out = world.programme(&["show"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Destination\nShip the demo.\nThen stop.\n\n\
         Read from working-tree\n\n\
         open 3 (frontier 1, blocked 1, in flight 1), closed 1, out of scope 1\n\n\
         Blocked\n  Name of C (C): blocked by D\n\n\
         In flight\n  Name of D (D): prog/demo/D 0m\n"
    );
    let out = world.programme(&["show", "--json"]);
    let shown: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(shown["counts"]["in_flight"], 1);
    assert_eq!(shown["blocked"][0]["blocked_by"][0], "D");
    assert_eq!(shown["in_flight"][0]["branch"], "prog/demo/D");
}

#[test]
fn check_accepts_the_valid_fixture_silently() {
    let world = World::new("valid");
    let out = world.programme(&["check"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(text(&out.stderr), "");
    let out = world.programme(&["check", "--json"]);
    assert_eq!(
        text(&out.stdout),
        "{\"problems\":[],\"source\":\"working-tree\",\"warnings\":[]}\n"
    );
}

#[test]
fn check_reports_each_problem_with_path_and_line_and_nothing_on_stdout() {
    type Edit = fn(&World);
    let cases: [(&str, Edit, String); 5] = [
        (
            "cycle",
            |w| w.write("D.md", &ticket("open", "[\"C\"]", None, "Name of D", None)),
            "C.md:5: blocked_by cycle: C -> D -> C".to_string(),
        ),
        (
            "dangling",
            |w| w.write("B.md", &ticket("open", "[\"Z\"]", None, "Name of B", None)),
            "B.md:5: blocked_by names `Z`, which is not a ticket here".to_string(),
        ),
        (
            "blocked-by-out-of-scope",
            |w| {
                w.write(
                    "B.md",
                    &ticket("open", "[\"A\", \"E\"]", None, "Name of B", None),
                )
            },
            "B.md:5: open ticket is blocked by out-of-scope ticket `E`".to_string(),
        ),
        (
            "unlinked",
            |w| w.write("map.md", &MAP.replace("- [Choose the store](A.md)\n", "")),
            "A.md:4: closed ticket is not linked from `## Decisions so far` in map.md: \
             a line must contain `(A.md)`"
                .to_string(),
        ),
        (
            "unknown-repo",
            |w| {
                w.write(
                    "D.md",
                    &ticket("open", "[]", Some("[\"ghost\"]"), "Name of D", None),
                )
            },
            "D.md:6: repos names `ghost`, which is not a git repository under the root".to_string(),
        ),
    ];
    for (tag, edit, expected) in cases {
        let world = World::new(&format!("bad-{tag}"));
        edit(&world);
        let out = world.programme(&["check"]);
        assert_eq!(out.status.code(), Some(2), "{tag}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "", "{tag}");
        let (file, rest) = expected.split_once(':').expect("path");
        assert_eq!(
            text(&out.stderr),
            format!("{}:{rest}\n1 problems\n", world.dir().join(file).display()),
            "{tag}"
        );
    }
}

#[test]
fn check_warns_without_failing_on_an_open_afk_ticket_missing_a_section() {
    for section in ["## Look at", "## Answer looks like"] {
        let world = World::new("warn");
        let full = ticket("open", "[]", None, "Name of D", None);
        let start = full.find(section).expect("section");
        let end = full[start + 2..]
            .find("\n## ")
            .map_or(full.len(), |n| start + 2 + n + 1);
        let trimmed = format!("{}{}", &full[..start], &full[end..]);
        assert!(!trimmed.contains(section));
        world.write("D.md", &trimmed);
        let out = world.programme(&["check"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "");
        assert_eq!(
            text(&out.stderr),
            format!(
                "{}:7: warning: open afk ticket has no `{section}` section\n",
                world.dir().join("D.md").display()
            )
        );
        let out = world.programme(&["check", "--json"]);
        let found: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        assert_eq!(found["problems"], serde_json::json!([]));
        assert_eq!(found["warnings"].as_array().map(Vec::len), Some(1));
        assert_eq!(found["warnings"][0]["line"], 7);
    }
}

#[test]
fn check_does_not_warn_about_hitl_tickets() {
    let world = World::new("nowarn");
    let bare = "+++\ntype = \"task\"\nmode = \"hitl\"\nstatus = \"open\"\nblocked_by = []\n+++\n# Name of D\n\n## Question\n\nWhich?\n";
    world.write("D.md", bare);
    let out = world.programme(&["check"]);
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn check_reports_every_problem_not_only_the_first() {
    let world = World::new("many");
    world.write(
        "B.md",
        &ticket("open", "[\"Z\", \"E\"]", None, "Name of B", None),
    );
    world.write("map.md", &MAP.replace("## Notes", "## Extra\n\n## Notes"));
    let out = world.programme(&["check", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let found: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(found["problems"].as_array().map(Vec::len), Some(2));
    assert!(
        text(&out.stderr).ends_with("2 problems\n"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn list_finds_the_maps_of_every_repository_under_the_root() {
    let world = World::new("list");
    let other = world.root.join("other/docs/programmes/second");
    std::fs::create_dir_all(&other).expect("dir");
    std::fs::write(
        other.join("map.md"),
        MAP.replace("Ship the demo.", "Second goal."),
    )
    .expect("map");
    std::fs::write(
        other.join("A.md"),
        ticket("closed", "[]", None, "Choose the store", Some("Decided.")),
    )
    .expect("ticket");
    std::fs::write(
        other.join("E.md"),
        ticket("out-of-scope", "[]", None, "A mobile app", None),
    )
    .expect("ticket");
    std::fs::write(
        other.join("F.md"),
        ticket("open", "[]", None, "Name of F", None),
    )
    .expect("ticket");
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!(
            "{root}/alpha/docs/programmes/demo/map.md\tShip the demo.\topen 3 frontier 2 closed 1\tworking-tree\n\
             {root}/other/docs/programmes/second/map.md\tSecond goal.\topen 1 frontier 1 closed 1\tworking-tree\n"
        )
    );
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(rows.as_array().map(Vec::len), Some(2));
    assert_eq!(rows[1]["frontier"], 1);
    assert_eq!(rows[1]["source"], "working-tree");
}

#[test]
fn a_linked_worktree_behaves_like_its_main_checkout() {
    let world = World::new("worktree");
    world.write(
        "D.md",
        &ticket("open", "[]", Some("[\"other\"]"), "Name of D", None),
    );
    git(&world.root.join("alpha"), &["add", "."]);
    git(
        &world.root.join("alpha"),
        &["commit", "-q", "-m", "programme"],
    );
    let linked = world.root.join("linked-copy");
    git(
        &world.root.join("alpha"),
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf8"),
            "-b",
            "side",
        ],
    );
    let dir = linked.join("docs/programmes/demo");
    let dir = dir.to_str().expect("utf8");

    let out = world.relais(&["programme", "check", dir]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");

    let out = world.relais(&["programme", "frontier", dir]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "B\ttask/afk\talpha\tName of B\nD\ttask/afk\tother\tName of D\n"
    );
}

#[test]
fn an_unknown_ticket_type_is_reported_and_left_off_the_frontier() {
    let world = World::new("poetry");
    let poem = ticket("open", "[]", None, "Name of D", None).replace("\"task\"", "\"poetry\"");
    world.write("D.md", &poem);
    let out = world.programme(&["frontier"]);
    assert_eq!(ids(&out.stdout), ["B"]);
    let out = world.programme(&["check"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    let stderr = text(&out.stderr);
    let prefix = format!("{}:2: unknown type", world.dir().join("D.md").display());
    assert!(stderr.contains(&prefix), "{stderr}");
}

/// Commits on `branch` in `repo` with the committer date `epoch` (Unix seconds).
fn commit_dated(repo: &Path, branch: &str, epoch: i64) {
    let date = format!("{epoch} +0000");
    let out = git_command(repo)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "work",
        ])
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_AUTHOR_DATE", &date)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "{}", text(&out.stderr));
    git(repo, &["branch", "-f", branch, "HEAD"]);
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

#[test]
fn an_in_flight_ticket_shows_its_branch_last_commit_age() {
    let world = World::new("age");
    let alpha = world.root.join("alpha");
    commit_dated(&alpha, "prog/demo/D", unix_now() - 3 * 3600);

    let out = world.programme(&["frontier", "--all"]);
    assert_eq!(
        text(&out.stdout),
        "B\ttask/afk\talpha\tName of B\t\nD\ttask/afk\talpha\tName of D\tin-flight prog/demo/D 3h\n"
    );
    let fields: Vec<usize> = text(&out.stdout)
        .lines()
        .map(|line| line.split('\t').count())
        .collect();
    assert_eq!(fields, [5, 5]);
    let out = world.programme(&["show"]);
    assert!(text(&out.stdout).contains("In flight\n  Name of D (D): prog/demo/D 3h\n"));

    let out = world.programme(&["frontier", "--all", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let age = rows[1]["branch_age_secs"].as_i64().expect("integer");
    assert!((10_800..=10_860).contains(&age), "{age}");
    assert!(rows[0]["branch_age_secs"].is_null());
    let out = world.programme(&["show", "--json"]);
    let shown: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let age = shown["in_flight"][0]["branch_age_secs"]
        .as_i64()
        .expect("integer");
    assert!((10_800..=10_860).contains(&age), "{age}");

    commit_dated(&alpha, "prog/demo/D", unix_now() + 7200);
    let out = world.programme(&["frontier", "--all"]);
    assert!(text(&out.stdout).ends_with("in-flight prog/demo/D 0m\n"));
    let out = world.programme(&["show", "--json"]);
    let shown: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(shown["in_flight"][0]["branch_age_secs"], 0);
}

#[test]
fn list_reads_maps_from_the_origin_default_branch() {
    let world = World::new("list-origin");
    let alpha = world.root.join("alpha");
    git(&alpha, &["add", "."]);
    git(&alpha, &["commit", "-q", "-m", "programme"]);
    let upstream = world.root.join("upstream.git");
    let upstream_s = upstream.to_str().expect("utf8");
    git(
        &world.root,
        &[
            "clone",
            "-q",
            "--bare",
            alpha.to_str().expect("utf8"),
            upstream_s,
        ],
    );
    // Upstream closes D and charts a second programme the checkout has not pulled.
    let pusher = world.root.join("pusher");
    git(
        &world.root,
        &["clone", "-q", upstream_s, pusher.to_str().expect("utf8")],
    );
    let demo = pusher.join("docs/programmes/demo");
    std::fs::write(
        demo.join("D.md"),
        ticket("closed", "[]", None, "Name of D", Some("Decided.")),
    )
    .expect("ticket");
    let later = pusher.join("docs/programmes/later");
    std::fs::create_dir_all(&later).expect("dir");
    std::fs::write(
        later.join("map.md"),
        MAP.replace("Ship the demo.", "Later goal."),
    )
    .expect("map");
    git(&pusher, &["add", "."]);
    git(&pusher, &["commit", "-q", "-m", "upstream"]);
    git(&pusher, &["push", "-q", "origin", "HEAD"]);
    std::fs::remove_dir_all(&pusher).expect("pusher gone");
    let branch = {
        let out = git_command(&alpha)
            .args(["branch", "--show-current"])
            .output()
            .expect("git runs");
        text(&out.stdout).trim().to_string()
    };
    git(&alpha, &["remote", "add", "origin", upstream_s]);
    git(&alpha, &["fetch", "-q", "origin"]);
    git(&alpha, &["remote", "set-head", "origin", &branch]);

    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!(
            "{root}/alpha/docs/programmes/demo/map.md\tShip the demo.\topen 2 frontier 2 closed 2\torigin/{branch}\n\
             {root}/alpha/docs/programmes/later/map.md\tLater goal.\topen 0 frontier 0 closed 0\torigin/{branch}\n"
        )
    );
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(rows[0]["source"], format!("origin/{branch}"));
}

#[test]
fn list_reads_the_files_when_origin_head_dangles() {
    let world = World::new("list-dangling");
    let alpha = world.root.join("alpha");
    git(
        &alpha,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/gone",
        ],
    );
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(rows.as_array().map(Vec::len), Some(1));
    assert_eq!(rows[0]["source"], "working-tree");
    assert_eq!(rows[0]["open"], 3);
}

#[test]
fn list_names_a_map_once_though_a_linked_worktree_holds_it() {
    let world = World::new("list-once");
    let alpha = world.root.join("alpha");
    git(&alpha, &["add", "."]);
    git(&alpha, &["commit", "-q", "-m", "programme"]);
    let linked = world.root.join("alpha-wt-x");
    git(
        &alpha,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf8"),
            "-b",
            "side",
        ],
    );
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).lines().count(),
        1,
        "{}",
        text(&out.stdout)
    );
}

/// Commits everything in `repo` with the committer and author date `epoch`.
fn commit_all_at(repo: &Path, epoch: i64, message: &str) {
    git(repo, &["add", "-A"]);
    let date = format!("{epoch} +0000");
    let out = git_command(repo)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ])
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_AUTHOR_DATE", &date)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "{}", text(&out.stderr));
}

/// `alpha` with its programme committed and a bare `upstream.git` as its
/// `origin`, fetched, `origin/HEAD` set. Returns the default branch name.
fn with_upstream(world: &World) -> String {
    let alpha = world.root.join("alpha");
    git(&alpha, &["add", "."]);
    git(&alpha, &["commit", "-q", "-m", "programme"]);
    let upstream = world.root.join("upstream.git");
    git(
        &world.root,
        &[
            "clone",
            "-q",
            "--bare",
            alpha.to_str().expect("utf8"),
            upstream.to_str().expect("utf8"),
        ],
    );
    let out = git_command(&alpha)
        .args(["branch", "--show-current"])
        .output()
        .expect("git runs");
    let branch = text(&out.stdout).trim().to_string();
    git(
        &alpha,
        &["remote", "add", "origin", upstream.to_str().expect("utf8")],
    );
    git(&alpha, &["fetch", "-q", "origin"]);
    git(&alpha, &["remote", "set-head", "origin", &branch]);
    branch
}

/// Commits `edit`'s changes to the upstream from a throwaway clone, so
/// `alpha` sees them only after it fetches.
fn push_upstream(world: &World, edit: impl FnOnce(&Path)) {
    let pusher = world.root.join("pusher");
    git(
        &world.root,
        &[
            "clone",
            "-q",
            world.root.join("upstream.git").to_str().expect("utf8"),
            pusher.to_str().expect("utf8"),
        ],
    );
    edit(&pusher.join("docs/programmes/demo"));
    git(&pusher, &["add", "-A"]);
    git(&pusher, &["commit", "-q", "-m", "upstream"]);
    git(&pusher, &["push", "-q", "origin", "HEAD"]);
    std::fs::remove_dir_all(&pusher).expect("pusher gone");
}

fn close_d(demo: &Path) {
    std::fs::write(
        demo.join("D.md"),
        ticket("closed", "[]", None, "Name of D", Some("Decided.")),
    )
    .expect("ticket");
}

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("json")
}

#[test]
fn list_fetches_origin_first_unless_told_not_to() {
    let world = World::new("fetch");
    with_upstream(&world);
    push_upstream(&world, close_d);
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root, "--json", "--no-fetch"]);
    assert_eq!(json_of(&out)[0]["closed"], 1, "{}", text(&out.stderr));
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(json_of(&out)[0]["closed"], 2);
    assert_eq!(
        text(&out.stderr),
        "",
        "a fetch that worked prints nothing off a terminal"
    );
}

#[test]
fn a_failed_fetch_is_named_and_the_last_fetch_is_read() {
    let world = World::new("fetch-fail");
    let branch = with_upstream(&world);
    let alpha = world.root.join("alpha");
    let missing = world.root.join("missing.git");
    git(
        &alpha,
        &[
            "remote",
            "set-url",
            "origin",
            missing.to_str().expect("utf8"),
        ],
    );
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with(&format!("relais: {}: fetch failed (", alpha.display())),
        "{stderr}"
    );
    assert!(stderr.contains("; reading the last fetch"), "{stderr}");
    assert!(
        stderr.ends_with("relais: run with --no-fetch to skip fetching\n"),
        "{stderr}"
    );
    assert_eq!(json_of(&out)[0]["source"], format!("origin/{branch}"));
}

#[test]
fn frontier_and_show_read_origin_when_the_checkout_has_no_local_work() {
    let world = World::new("frontier-origin");
    with_upstream(&world);
    push_upstream(&world, |demo| {
        close_d(demo);
        let later = demo.parent().expect("programmes").join("later");
        std::fs::create_dir_all(&later).expect("dir");
        std::fs::write(
            later.join("map.md"),
            MAP.replace("Ship the demo.", "Later."),
        )
        .expect("map");
    });
    let out = world.programme(&["frontier"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(ids(&out.stdout), ["B", "C"]);
    let out = world.programme(&["show", "--json", "--no-fetch"]);
    assert_eq!(json_of(&out)["counts"]["closed"], 2);

    let later = world.root.join("alpha/docs/programmes/later");
    assert!(!later.exists());
    let out = world.relais(&["programme", "frontier", later.to_str().expect("utf8")]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

#[test]
fn a_checkout_with_uncommitted_work_is_read_from_its_files() {
    let world = World::new("dirty");
    with_upstream(&world);
    push_upstream(&world, close_d);
    world.write("B.md", &ticket("open", "[\"A\"]", None, "B renamed", None));
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    assert_eq!(
        json_of(&out)[0]["source"],
        "working-tree",
        "{}",
        text(&out.stderr)
    );
    let out = world.programme(&["frontier"]);
    assert_eq!(ids(&out.stdout), ["B", "D"]);
}

/// `path` with its symlinks resolved: git names a worktree by its real path.
fn real(path: &Path) -> PathBuf {
    path.canonicalize().expect("exists")
}

/// The first column of each `list` row, resolved.
fn real_paths(stdout: &[u8]) -> Vec<PathBuf> {
    ids(stdout).iter().map(|p| real(Path::new(p))).collect()
}

#[test]
fn list_names_a_programme_charted_in_a_worktree_and_not_pushed() {
    let world = World::new("unpushed");
    with_upstream(&world);
    let alpha = world.root.join("alpha");
    let linked = world.root.join("alpha-wt-x");
    git(
        &alpha,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf8"),
            "-b",
            "side",
        ],
    );
    let new = linked.join("docs/programmes/new");
    std::fs::create_dir_all(&new).expect("dir");
    std::fs::write(
        new.join("map.md"),
        MAP.replace("Ship the demo.", "New goal."),
    )
    .expect("map");
    commit_all_at(&linked, unix_now(), "chart");
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        real_paths(&out.stdout),
        [
            real(&world.root.join("alpha/docs/programmes/demo/map.md")),
            real(&linked.join("docs/programmes/new/map.md")),
        ]
    );
}

#[test]
fn the_newest_local_copy_wins_and_a_tie_goes_to_the_main_checkout() {
    let world = World::new("newest");
    with_upstream(&world);
    let alpha = world.root.join("alpha");
    let mut linked = Vec::new();
    for name in ["alpha-wt-a", "alpha-wt-b"] {
        let path = world.root.join(name);
        git(
            &alpha,
            &[
                "worktree",
                "add",
                "-q",
                path.to_str().expect("utf8"),
                "-b",
                name,
            ],
        );
        linked.push(path);
    }
    let edit = |checkout: &Path, name: &str, epoch: i64| {
        std::fs::write(
            checkout.join("docs/programmes/demo/B.md"),
            ticket("open", "[\"A\"]", None, name, None),
        )
        .expect("ticket");
        commit_all_at(checkout, epoch, name);
    };
    let now = unix_now();
    // Both copies are newer than origin's last change, which is now.
    edit(&linked[0], "B in a", now + 3600);
    edit(&linked[1], "B in b", now + 7200);
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root, "--no-fetch"]);
    assert_eq!(
        real_paths(&out.stdout),
        [real(&linked[1].join("docs/programmes/demo/map.md"))],
        "{}",
        text(&out.stderr)
    );
    edit(&alpha, "B in main", now + 7200);
    let out = world.relais(&["programme", "list", "--root", root, "--no-fetch"]);
    assert_eq!(
        real_paths(&out.stdout),
        [real(&alpha.join("docs/programmes/demo/map.md"))]
    );
}

#[test]
fn a_worktree_whose_branch_was_squash_merged_reads_origin() {
    let world = World::new("squashed");
    with_upstream(&world);
    let alpha = world.root.join("alpha");
    let linked = world.root.join("alpha-wt-s");
    git(
        &alpha,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf8"),
            "-b",
            "side",
        ],
    );
    let renamed = ticket("open", "[\"A\"]", None, "B renamed", None);
    std::fs::write(linked.join("docs/programmes/demo/B.md"), &renamed).expect("ticket");
    commit_all_at(&linked, unix_now(), "rename B");
    // The same change lands upstream as a squash, then upstream closes D.
    push_upstream(&world, |demo| {
        std::fs::write(demo.join("B.md"), &renamed).expect("ticket");
    });
    push_upstream(&world, close_d);
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    let rows = json_of(&out);
    assert_eq!(
        rows.as_array().map(Vec::len),
        Some(1),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(
        rows[0]["source"],
        format!("origin/{}", {
            let out = git_command(&alpha)
                .args(["branch", "--show-current"])
                .output()
                .expect("git");
            text(&out.stdout).trim().to_string()
        })
    );
    assert_eq!(rows[0]["closed"], 2);
    let dir = linked.join("docs/programmes/demo");
    let out = world.relais(&["programme", "frontier", dir.to_str().expect("utf8")]);
    assert_eq!(ids(&out.stdout), ["B", "C"], "{}", text(&out.stderr));
}

fn poetry() -> String {
    ticket("open", "[]", None, "Name of X", None).replace("\"task\"", "\"poetry\"")
}

#[test]
fn check_tags_the_problems_it_read_from_origin() {
    let world = World::new("check-origin");
    let branch = with_upstream(&world);
    push_upstream(&world, |demo| {
        std::fs::write(demo.join("X.md"), poetry()).expect("ticket");
    });
    let out = world.programme(&["check", "--json"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let expected = format!(
        "{}:2: (origin/{branch}) unknown type",
        world.dir().join("X.md").display()
    );
    assert!(
        text(&out.stderr).contains(&expected),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(json_of(&out)["source"], format!("origin/{branch}"));
}

#[test]
fn check_reads_the_edits_being_made() {
    let world = World::new("check-edits");
    with_upstream(&world);
    world.write("X.md", &poetry());
    let out = world.programme(&["check"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let expected = format!("{}:2: unknown type", world.dir().join("X.md").display());
    assert!(
        text(&out.stderr).contains(&expected),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn check_names_a_failed_fetch_before_its_problems() {
    let world = World::new("check-fetch-fail");
    with_upstream(&world);
    let missing = world.root.join("missing.git");
    git(
        &world.root.join("alpha"),
        &[
            "remote",
            "set-url",
            "origin",
            missing.to_str().expect("utf8"),
        ],
    );
    world.write("X.md", &poetry());
    let out = world.programme(&["check"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    let mut lines = stderr.lines();
    assert!(
        lines.next().is_some_and(|l| l.starts_with("relais: ")),
        "{stderr}"
    );
    assert!(
        lines
            .next()
            .is_some_and(|l| l.starts_with("relais: run with --no-fetch")),
        "{stderr}"
    );
    assert!(
        lines.next().is_some_and(|l| l.contains(":2: unknown type")),
        "{stderr}"
    );
}

/// The processes of process group `pgid` whose command line matches `pattern`.
fn in_group(pgid: u32, pattern: &str) -> String {
    let out = std::process::Command::new("pgrep")
        .args(["-g", &pgid.to_string(), "-f", pattern])
        .output()
        .expect("pgrep runs");
    text(&out.stdout)
}

/// The `*.lock` files under `git_dir`'s top level and `refs/`.
fn locks_under(git_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![(git_dir.to_path_buf(), false), (git_dir.join("refs"), true)];
    while let Some((dir, recurse)) = pending.pop() {
        for path in std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
        {
            if path.is_dir() {
                if recurse {
                    pending.push((path, true));
                }
            } else if path.extension().is_some_and(|e| e == "lock") {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn a_ctrl_c_during_the_fetch_leaves_no_fetch_and_no_lock() {
    use std::os::unix::process::CommandExt;
    let world = World::new("sigint");
    with_upstream(&world);
    let alpha = world.root.join("alpha");
    // relais sets GIT_SSH_COMMAND itself, so the transport hangs here.
    git(
        &alpha,
        &[
            "config",
            "remote.origin.uploadpack",
            "sleep 60; git-upload-pack",
        ],
    );
    let root = world.root.to_str().expect("utf8");
    let mut child = common::relais(BIN)
        .env("RELAIS_STATE_DIR", &world.state)
        .args(["programme", "list", "--root", root])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("relais starts");
    let pgid = child.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while in_group(pgid, "sleep 60").is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the fetch never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !in_group(pgid, "fetch").is_empty(),
        "a git fetch runs in relais's group"
    );
    let pgid_t = libc::pid_t::try_from(pgid).expect("pid fits");
    // SAFETY: killpg only sends a signal to the group this test started.
    let signalled = std::time::Instant::now();
    assert_eq!(unsafe { libc::killpg(pgid_t, libc::SIGINT) }, 0);
    let status = child.wait().expect("relais ends");
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
    }
    assert!(
        signalled.elapsed() < std::time::Duration::from_secs(5),
        "relais stopped well before the 10 s backstop"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !in_group(pgid, ".").is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "left behind: {}",
            in_group(pgid, ".")
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(locks_under(&alpha.join(".git")), Vec::<PathBuf>::new());
}

#[test]
fn a_worktree_older_than_origins_last_change_reads_origin() {
    let world = World::new("stale");
    with_upstream(&world);
    let alpha = world.root.join("alpha");
    let linked = world.root.join("alpha-wt-old");
    git(
        &alpha,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("utf8"),
            "-b",
            "old",
        ],
    );
    std::fs::write(
        linked.join("docs/programmes/demo/B.md"),
        ticket("open", "[\"A\"]", None, "B, a week ago", None),
    )
    .expect("ticket");
    commit_all_at(&linked, unix_now() - 7 * 86_400, "old work");
    push_upstream(&world, close_d);
    let root = world.root.to_str().expect("utf8");
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    let rows = json_of(&out);
    assert_eq!(
        rows.as_array().map(Vec::len),
        Some(1),
        "{}",
        text(&out.stdout)
    );
    assert_ne!(rows[0]["source"], "working-tree");
    assert_eq!(rows[0]["closed"], 2);
    let dir = linked.join("docs/programmes/demo");
    let out = world.relais(&["programme", "frontier", dir.to_str().expect("utf8")]);
    assert_eq!(ids(&out.stdout), ["B", "C"], "{}", text(&out.stderr));
}

#[test]
fn show_names_where_it_read_from() {
    let world = World::new("show-source");
    let branch = with_upstream(&world);
    push_upstream(&world, close_d);
    let out = world.programme(&["show"]);
    assert!(
        text(&out.stdout).contains(&format!("\n\nRead from origin/{branch}\n\n")),
        "{}",
        text(&out.stdout)
    );
    let out = world.programme(&["show", "--json", "--no-fetch"]);
    assert_eq!(json_of(&out)["source"], format!("origin/{branch}"));
}

#[test]
fn local_edits_older_than_origin_are_named_when_set_aside() {
    let world = World::new("set-aside");
    let branch = with_upstream(&world);
    world.write("X.md", &poetry());
    let old = std::process::Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(world.dir().join("X.md"))
        .status()
        .expect("touch runs");
    assert!(old.success());
    push_upstream(&world, close_d);
    // Origin's copy is what is checked (its closed D is unlinked: exit 2);
    // the person's older edit is named, not silently skipped.
    let out = world.programme(&["check"]);
    let expected = format!(
        "relais: {}: local work older than origin/{branch}'s last change; reading origin/{branch}\n",
        world.dir().display()
    );
    assert!(
        text(&out.stderr).contains(&expected),
        "{}",
        text(&out.stderr)
    );
}
