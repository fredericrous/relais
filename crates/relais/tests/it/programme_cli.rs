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
        "+++\ntype = \"task\"\nmode = \"afk\"\nstatus = \"{status}\"\nblocked_by = {blocked_by}\n{repos}+++\n# {name}\n\n## Question\n\nWhich one?\n{resolution}"
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
        "B\ttask/afk\talpha\tName of B\nD\ttask/afk\talpha\tName of D\tin-flight prog/demo/D\n"
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
         open 3 (frontier 1, blocked 1, in flight 1), closed 1, out of scope 1\n\n\
         Blocked\n  Name of C (C): blocked by D\n\n\
         In flight\n  Name of D (D): prog/demo/D\n"
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
    assert_eq!(text(&out.stdout), "{\"problems\":[]}\n");
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
            "{root}/alpha/docs/programmes/demo/map.md\tShip the demo.\topen 3 frontier 2 closed 1\n\
             {root}/other/docs/programmes/second/map.md\tSecond goal.\topen 1 frontier 1 closed 1\n"
        )
    );
    let out = world.relais(&["programme", "list", "--root", root, "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(rows.as_array().map(Vec::len), Some(2));
    assert_eq!(rows[1]["frontier"], 1);
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
