//! Reading a programme from disk and its branches from git. The one
//! module of this crate area that does I/O; it never writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use std::time::Duration;

use super::analysis;
use super::model::{MapDoc, Problem, Ticket, MAP_FILE};
use super::parse::{parse_map, parse_ticket};

/// Why a programme could not be read at all.
#[derive(Debug)]
pub enum ProgrammeError {
    Read {
        path: PathBuf,
        cause: std::io::Error,
    },
    /// The directory is not `<repository>/docs/programmes/<slug>`.
    Layout {
        dir: PathBuf,
    },
    Git {
        repo: PathBuf,
        detail: String,
    },
}

impl std::fmt::Display for ProgrammeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, cause } => write!(f, "cannot read {}: {cause}", path.display()),
            Self::Layout { dir } => write!(
                f,
                "{} is not a programme directory: expected <repository>/docs/programmes/<slug>",
                dir.display()
            ),
            Self::Git { repo, detail } => {
                write!(f, "git failed in {}: {detail}", repo.display())
            }
        }
    }
}

impl std::error::Error for ProgrammeError {}

fn read_error(path: &Path) -> impl FnOnce(std::io::Error) -> ProgrammeError + '_ {
    |cause| ProgrammeError::Read {
        path: path.to_path_buf(),
        cause,
    }
}

/// Listing refs reads local files; a git that has not answered by now is stuck.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// A programme as its files state it.
#[derive(Debug)]
pub struct Programme {
    /// The directory as the caller named it, for messages.
    pub dir: PathBuf,
    /// The directory's name, which names the branches.
    pub slug: String,
    /// The main checkout of the repository that holds the map, even when
    /// the map is read from a linked worktree.
    pub repo_dir: PathBuf,
    /// The parent of the map's repository: where `repos` entries live.
    pub root: PathBuf,
    pub map: MapDoc,
    pub tickets: BTreeMap<String, Ticket>,
    /// Ticket files that did not parse into a ticket.
    pub unreadable: BTreeSet<String>,
    file_problems: Vec<Problem>,
}

impl Programme {
    pub fn load(dir: &Path) -> Result<Self, ProgrammeError> {
        let canonical = dir.canonicalize().map_err(read_error(dir))?;
        let layout = || ProgrammeError::Layout {
            dir: dir.to_path_buf(),
        };
        let name_of = |path: &Path| path.file_name().map(|n| n.to_string_lossy().into_owned());
        let programmes = canonical.parent().ok_or_else(layout)?;
        let docs = programmes.parent().ok_or_else(layout)?;
        let repo_dir = docs.parent().ok_or_else(layout)?;
        if name_of(programmes).as_deref() != Some("programmes")
            || name_of(docs).as_deref() != Some("docs")
        {
            return Err(layout());
        }
        let slug = name_of(&canonical).ok_or_else(layout)?;

        let map_path = canonical.join(MAP_FILE);
        let map_text = std::fs::read_to_string(&map_path).map_err(read_error(&map_path))?;

        let mut files: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&canonical).map_err(read_error(&canonical))? {
            let path = entry.map_err(read_error(&canonical))?.path();
            let is_ticket = path.is_file()
                && path.extension().is_some_and(|ext| ext == "md")
                && path.file_name().is_some_and(|name| name != MAP_FILE);
            if is_ticket {
                files.push(path);
            }
        }
        files.sort();
        let mut ticket_texts = Vec::new();
        for path in files {
            let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let text = std::fs::read_to_string(&path).map_err(read_error(&path))?;
            ticket_texts.push((id, text));
        }
        let repo_dir = main_checkout(repo_dir)?;
        Ok(Self::assemble(
            dir.to_path_buf(),
            slug,
            repo_dir,
            &map_text,
            ticket_texts,
        ))
    }

    /// The programme `<repo>/docs/programmes/<slug>` as the commit `rev`
    /// holds it, read through git rather than from the working tree. `repo`
    /// is a main checkout; `dir` is still the working-tree path, for messages.
    pub fn load_at(repo: &Path, slug: &str, rev: &str) -> Result<Self, ProgrammeError> {
        let prefix = format!("docs/programmes/{slug}/");
        let names = git_stdout(repo, &["ls-tree", "-z", "--name-only", rev, "--", &prefix])?;
        let mut ids: Vec<String> = names
            .split('\0')
            .filter_map(|path| path.strip_prefix(&prefix))
            .filter(|name| *name != MAP_FILE)
            .filter_map(|name| name.strip_suffix(".md"))
            .filter(|id| !id.is_empty() && !id.contains('/'))
            .map(str::to_string)
            .collect();
        ids.sort();
        let show = |file: &str| git_stdout(repo, &["show", &format!("{rev}:{prefix}{file}")]);
        let map_text = show(MAP_FILE)?;
        let mut ticket_texts = Vec::new();
        for id in ids {
            let text = show(&format!("{id}.md"))?;
            ticket_texts.push((id, text));
        }
        let repo_dir = main_checkout(repo)?;
        Ok(Self::assemble(
            repo.join("docs").join("programmes").join(slug),
            slug.to_string(),
            repo_dir,
            &map_text,
            ticket_texts,
        ))
    }

    /// Parses a map and its tickets, given as `(id, text)` sorted by id.
    fn assemble(
        dir: PathBuf,
        slug: String,
        repo_dir: PathBuf,
        map_text: &str,
        ticket_texts: Vec<(String, String)>,
    ) -> Self {
        let (map, mut file_problems) = parse_map(map_text);
        let mut tickets = BTreeMap::new();
        let mut unreadable = BTreeSet::new();
        for (id, text) in ticket_texts {
            let (ticket, problems) = parse_ticket(&id, &text);
            file_problems.extend(problems);
            match ticket {
                Some(ticket) => {
                    tickets.insert(id, ticket);
                }
                None => {
                    unreadable.insert(id);
                }
            }
        }
        let root = repo_dir.parent().unwrap_or(&repo_dir).to_path_buf();
        Self {
            dir,
            slug,
            repo_dir,
            root,
            map,
            tickets,
            unreadable,
            file_problems,
        }
    }

    /// The name of the repository that holds the map: a ticket's repos
    /// when it does not say.
    pub fn default_repo(&self) -> String {
        self.repo_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn repos_of(&self, ticket: &Ticket) -> Vec<String> {
        ticket
            .repos
            .clone()
            .unwrap_or_else(|| vec![self.default_repo()])
    }

    /// Every problem, the map's first, then by file and line.
    pub fn problems(&self) -> Vec<Problem> {
        let root = &self.root;
        let named: BTreeSet<&String> = self
            .tickets
            .values()
            .flat_map(|t| t.repos.iter().flatten())
            .collect();
        let existing_repos: BTreeSet<String> = named
            .into_iter()
            .filter(|name| {
                let single = Path::new(name.as_str()).components().count() == 1
                    && matches!(
                        Path::new(name.as_str()).components().next(),
                        Some(std::path::Component::Normal(_))
                    );
                single && root.join(name).join(".git").exists() && root.join(name).is_dir()
            })
            .cloned()
            .collect();
        let mut problems = self.file_problems.clone();
        problems.extend(analysis::check(
            &self.tickets,
            &self.unreadable,
            &self.map,
            &existing_repos,
        ));
        problems.sort_by(|a, b| {
            (a.file != MAP_FILE, &a.file, a.line).cmp(&(b.file != MAP_FILE, &b.file, b.line))
        });
        problems
    }

    /// Where each ticket stands, reading the branches from git.
    pub fn standings(&self) -> Result<BTreeMap<String, analysis::Standing>, ProgrammeError> {
        let working = branch_tips(&self.repo_dir, &self.slug)?;
        Ok(analysis::standings(&self.tickets, &self.slug, &working))
    }
}

/// What `git <args>` prints in `repo`.
fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, ProgrammeError> {
    let mut command = crate::workspace::git_command(repo);
    command.args(args);
    let git_failed = |detail: String| ProgrammeError::Git {
        repo: repo.to_path_buf(),
        detail,
    };
    let output = crate::procs::run_with_timeout(command, GIT_TIMEOUT, None, None)
        .map_err(|e| git_failed(e.to_string()))?;
    if !output.ended.succeeded() {
        return Err(git_failed(format!(
            "{}: {}",
            output.ended.describe(),
            output.stderr.trim()
        )));
    }
    Ok(output.stdout)
}

/// The main checkout of the repository `dir` belongs to: the parent of the
/// git common directory, which a linked worktree shares with it.
pub fn main_checkout(dir: &Path) -> Result<PathBuf, ProgrammeError> {
    let printed = git_stdout(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(printed.trim());
    let is_dot_git = common.file_name().is_some_and(|name| name == ".git");
    match common.parent() {
        Some(parent) if is_dot_git => Ok(parent.to_path_buf()),
        _ => Ok(common),
    }
}

/// The ids that a local branch or an `origin` remote-tracking branch named
/// `prog/<slug>/<id>` works, each with the latest committer date (Unix
/// seconds) among its refs.
fn branch_tips(repo: &Path, slug: &str) -> Result<BTreeMap<String, i64>, ProgrammeError> {
    let local = format!("refs/heads/prog/{slug}/");
    let remote = format!("refs/remotes/origin/prog/{slug}/");
    let printed = git_stdout(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)\t%(committerdate:unix)",
            &local,
            &remote,
        ],
    )?;
    let mut tips: BTreeMap<String, i64> = BTreeMap::new();
    for line in printed.lines() {
        let Some((name, date)) = line.split_once('\t') else {
            continue;
        };
        let Some(id) = name
            .strip_prefix(&local)
            .or_else(|| name.strip_prefix(&remote))
        else {
            continue;
        };
        let committed = date
            .trim()
            .parse::<i64>()
            .map_err(|e| ProgrammeError::Git {
                repo: repo.to_path_buf(),
                detail: format!("committer date of {name}: {e}"),
            })?;
        let latest = tips.entry(id.to_string()).or_insert(committed);
        *latest = (*latest).max(committed);
    }
    Ok(tips)
}

/// Where a found programme is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The checkout's files: the repository has no `origin` default branch.
    WorkingTree,
    /// A remote-tracking ref, such as `origin/main`.
    Ref(String),
}

/// A programme `find_programmes` found.
#[derive(Debug)]
pub struct Found {
    /// The main checkout that holds it.
    pub repo: PathBuf,
    pub slug: String,
    pub source: Source,
}

impl Found {
    /// The working-tree path of its directory, for messages.
    pub fn dir(&self) -> PathBuf {
        self.repo.join("docs").join("programmes").join(&self.slug)
    }

    pub fn load(&self) -> Result<Programme, ProgrammeError> {
        match &self.source {
            Source::WorkingTree => Programme::load(&self.dir()),
            Source::Ref(rev) => Programme::load_at(&self.repo, &self.slug, rev),
        }
    }
}

/// The remote-tracking ref of `origin`'s default branch: what
/// `origin/HEAD` points at, else `origin/main` when it exists.
/// Each candidate must resolve to a commit: an `origin/HEAD` left dangling by
/// a renamed upstream branch is skipped, never read.
fn origin_default(repo: &Path) -> Option<String> {
    let resolves = |rev: &str| {
        git_stdout(
            repo,
            &["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")],
        )
        .is_ok() // exit 1: the ref names no commit
    };
    let head = git_stdout(
        repo,
        &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok() // a missing origin/HEAD exits 1; fall back to origin/main
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty() && resolves(s));
    head.or_else(|| Some("origin/main".to_string()).filter(|main| resolves(main)))
}

/// The slugs of `docs/programmes/*/map.md` in the commit `rev`, sorted.
fn programmes_at(repo: &Path, rev: &str) -> Result<Vec<String>, ProgrammeError> {
    let names = git_stdout(
        repo,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            rev,
            "--",
            "docs/programmes/",
        ],
    )?;
    let suffix = format!("/{MAP_FILE}");
    let slugs: BTreeSet<String> = names
        .split('\0')
        .filter_map(|path| path.strip_prefix("docs/programmes/"))
        .filter_map(|rest| rest.strip_suffix(&suffix))
        .filter(|slug| !slug.is_empty() && !slug.contains('/'))
        .map(str::to_string)
        .collect();
    Ok(slugs.into_iter().collect())
}

/// Every programme under `<root>/*/docs/programmes/*` that holds a `map.md`,
/// sorted. Only primary checkouts (a `.git` directory) are scanned: a linked
/// worktree holds the same map as its main checkout. A repository with an
/// `origin` default branch is read from that ref, so maps and ticket
/// closures merged upstream show before the checkout pulls them; one
/// without is read from its files.
pub fn find_programmes(root: &Path) -> Result<Vec<Found>, ProgrammeError> {
    let children = |dir: &Path| -> Result<Vec<PathBuf>, ProgrammeError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(read_error(dir))? {
            paths.push(entry.map_err(read_error(dir))?.path());
        }
        paths.sort();
        Ok(paths)
    };
    let mut found = Vec::new();
    for repo in children(root)? {
        if !repo.join(".git").is_dir() {
            continue;
        }
        // A ref git cannot list falls back to the files, so one repository
        // never hides the others.
        let at_origin = origin_default(&repo)
            .and_then(|rev| programmes_at(&repo, &rev).ok().map(|slugs| (rev, slugs)));
        if let Some((rev, slugs)) = at_origin {
            for slug in slugs {
                found.push(Found {
                    repo: repo.clone(),
                    slug,
                    source: Source::Ref(rev.clone()),
                });
            }
            continue;
        }
        for programme in children(&repo.join("docs").join("programmes"))? {
            let slug = programme
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            if let (true, Some(slug)) = (programme.join(MAP_FILE).is_file(), slug) {
                found.push(Found {
                    repo: repo.clone(),
                    slug,
                    source: Source::WorkingTree,
                });
            }
        }
    }
    Ok(found)
}
