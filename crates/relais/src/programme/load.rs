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
        let (map, mut file_problems) = parse_map(&map_text);

        let mut tickets = BTreeMap::new();
        let mut unreadable = BTreeSet::new();
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
        for path in files {
            let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let text = std::fs::read_to_string(&path).map_err(read_error(&path))?;
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
        let repo_dir = main_checkout(repo_dir)?;
        let root = repo_dir.parent().unwrap_or(&repo_dir).to_path_buf();
        Ok(Self {
            dir: dir.to_path_buf(),
            slug,
            repo_dir,
            root,
            map,
            tickets,
            unreadable,
            file_problems,
        })
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
        let working = branch_ids(&self.repo_dir, &self.slug)?;
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
/// `prog/<slug>/<id>` works.
fn branch_ids(repo: &Path, slug: &str) -> Result<BTreeSet<String>, ProgrammeError> {
    let local = format!("refs/heads/prog/{slug}/");
    let remote = format!("refs/remotes/origin/prog/{slug}/");
    let printed = git_stdout(
        repo,
        &["for-each-ref", "--format=%(refname)", &local, &remote],
    )?;
    Ok(printed
        .lines()
        .filter_map(|name| {
            name.strip_prefix(&local)
                .or_else(|| name.strip_prefix(&remote))
        })
        .map(str::to_string)
        .collect())
}

/// Every `<root>/*/docs/programmes/*` directory holding a `map.md`, sorted.
pub fn find_programmes(root: &Path) -> Result<Vec<PathBuf>, ProgrammeError> {
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
        for programme in children(&repo.join("docs").join("programmes"))? {
            if programme.join(MAP_FILE).is_file() {
                found.push(programme);
            }
        }
    }
    Ok(found)
}
