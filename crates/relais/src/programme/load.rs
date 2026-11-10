//! Reading a programme from disk or from origin's default branch, choosing
//! between them (SPEC §31), and its branches from git. The one module of
//! this crate area that does I/O; it never writes a map, and its one write
//! is the remote-tracking refs `fetch_origin` updates.

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
        // One `cat-file --batch` for the map and every ticket: a git process
        // per file cost seconds on a programme with tens of tickets.
        let mut files = vec![MAP_FILE.to_string()];
        files.extend(ids.iter().map(|id| format!("{id}.md")));
        let input: String = files
            .iter()
            .map(|file| format!("{rev}:{prefix}{file}\n"))
            .collect();
        let printed = git_stdout_with_input(repo, &["cat-file", "--batch"], input)?;
        let texts = batch_contents(&printed);
        if texts.len() != files.len() {
            return Err(ProgrammeError::Git {
                repo: repo.to_path_buf(),
                detail: format!(
                    "cat-file printed {} objects for {} asked",
                    texts.len(),
                    files.len()
                ),
            });
        }
        let mut texts = texts.into_iter();
        let missing = |file: &str| ProgrammeError::Git {
            repo: repo.to_path_buf(),
            detail: format!("{rev}:{prefix}{file} is missing"),
        };
        let map_text = texts.next().flatten().ok_or_else(|| missing(MAP_FILE))?;
        let mut ticket_texts = Vec::new();
        for (id, text) in ids.into_iter().zip(texts) {
            let text = text.ok_or_else(|| missing(&format!("{id}.md")))?;
            ticket_texts.push((id, text));
        }
        // `repo` is already a main checkout: every caller passes one.
        let repo_dir = repo.to_path_buf();
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

/// Where a programme is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A checkout's files: it has local work on the programme, or the
    /// repository has no `origin` default branch.
    WorkingTree,
    /// A remote-tracking ref, such as `origin/main`.
    Ref(String),
}

impl Source {
    /// `working-tree` or the ref, as `list` and `--json` print it.
    pub fn label(&self) -> &str {
        match self {
            Self::WorkingTree => "working-tree",
            Self::Ref(rev) => rev,
        }
    }
}

/// A programme, and where the resolution rule (SPEC §31) reads it from.
#[derive(Debug)]
pub struct Found {
    /// The main checkout of its repository.
    pub repo: PathBuf,
    /// The checkout whose path names it: the one with local work, else the
    /// main checkout.
    pub checkout: PathBuf,
    pub slug: String,
    pub source: Source,
}

impl Found {
    /// The working-tree path of its directory, which may be absent on disk
    /// when it is read from a ref.
    pub fn dir(&self) -> PathBuf {
        self.checkout.join(programme_rel(&self.slug))
    }

    pub fn load(&self) -> Result<Programme, ProgrammeError> {
        match &self.source {
            Source::WorkingTree => Programme::load(&self.dir()),
            Source::Ref(rev) => Programme::load_at(&self.repo, &self.slug, rev),
        }
    }
}

fn programme_rel(slug: &str) -> String {
    format!("docs/programmes/{slug}")
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
fn programmes_at(repo: &Path, rev: &str) -> Result<BTreeSet<String>, ProgrammeError> {
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
    Ok(names
        .split('\0')
        .filter_map(|path| path.strip_prefix("docs/programmes/"))
        .filter_map(|rest| rest.strip_suffix(&suffix))
        .filter(|slug| !slug.is_empty() && !slug.contains('/'))
        .map(str::to_string)
        .collect())
}

/// Origin's default ref and the programmes it holds.
struct OriginView {
    rev: String,
    slugs: BTreeSet<String>,
}

/// Origin's view of a repository: `None` when it has no default ref, an
/// error when git cannot list the ref it has.
fn origin_view(repo: &Path) -> Result<Option<OriginView>, ProgrammeError> {
    let Some(rev) = origin_default(repo) else {
        return Ok(None);
    };
    let slugs = programmes_at(repo, &rev)?;
    Ok(Some(OriginView { rev, slugs }))
}

/// What `git <args>` prints in `repo`, given `input` on stdin.
fn git_stdout_with_input(
    repo: &Path,
    args: &[&str],
    input: String,
) -> Result<String, ProgrammeError> {
    let mut command = crate::workspace::git_command(repo);
    command.args(args);
    let git_failed = |detail: String| ProgrammeError::Git {
        repo: repo.to_path_buf(),
        detail,
    };
    let output =
        crate::procs::run_with_timeout(command, GIT_TIMEOUT, Some(input.into_bytes()), None)
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

/// The contents `git cat-file --batch` printed, one per object asked, in
/// order: `None` for an object it reported missing. Each entry is a header
/// `<sha> <type> <size>`, `<size>` bytes, and a newline.
fn batch_contents(printed: &str) -> Vec<Option<String>> {
    let mut contents = Vec::new();
    let mut rest = printed;
    while let Some((header, after)) = rest.split_once('\n') {
        let size = header
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|_| !header.ends_with(" missing"));
        match size.and_then(|size| after.get(..size).map(|body| (size, body))) {
            Some((size, body)) => {
                contents.push(Some(body.to_string()));
                rest = after.get(size + 1..).unwrap_or_default();
            }
            None => {
                contents.push(None);
                rest = after;
            }
        }
    }
    contents
}

/// Every tree `docs/programmes/<slug>` had in the history of `rev`: a
/// checkout whose tree is one of them has nothing origin lacks, even when
/// its commits were squash- or rebase-merged and so are not in `rev`.
fn trees_on(repo: &Path, rev: &str, slug: &str) -> Result<BTreeSet<String>, ProgrammeError> {
    let rel = programme_rel(slug);
    let commits = git_stdout(repo, &["rev-list", "--full-history", rev, "--", &rel])?;
    let input: String = commits
        .lines()
        .filter(|c| !c.is_empty())
        .map(|c| format!("{c}:{rel}\n"))
        .collect();
    if input.is_empty() {
        return Ok(BTreeSet::new());
    }
    let printed = git_stdout_with_input(repo, &["cat-file", "--batch-check"], input)?;
    Ok(printed
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            match (fields.next(), fields.next()) {
                (Some(sha), Some("tree")) => Some(sha.to_string()),
                _ => None, // `<object> missing`: the path did not exist there
            }
        })
        .collect())
}

/// What origin's default ref says about one programme.
enum Origin<'a> {
    /// The repository has no origin default ref: the files decide.
    Absent,
    /// The ref exists and does not hold the programme.
    Lacks { rev: &'a str },
    /// The ref holds it: every tree its directory had in the ref's history,
    /// and the committer date of the newest commit touching it.
    Holds {
        rev: &'a str,
        trees: BTreeSet<String>,
        changed_at: i64,
    },
}

impl Origin<'_> {
    fn rev(&self) -> Option<&str> {
        match self {
            Self::Absent => None,
            Self::Lacks { rev } | Self::Holds { rev, .. } => Some(rev),
        }
    }
}

fn origin_for<'a>(
    repo: &Path,
    view: Option<&'a OriginView>,
    slug: &str,
) -> Result<Origin<'a>, ProgrammeError> {
    let Some(view) = view else {
        return Ok(Origin::Absent);
    };
    if !view.slugs.contains(slug) {
        return Ok(Origin::Lacks { rev: &view.rev });
    }
    let printed = git_stdout(
        repo,
        &[
            "log",
            "-1",
            "--format=%ct",
            &view.rev,
            "--",
            &programme_rel(slug),
        ],
    )?;
    let changed_at = printed
        .trim()
        .parse::<i64>()
        .map_err(|e| ProgrammeError::Git {
            repo: repo.to_path_buf(),
            detail: format!("last change of {slug} on {}: {e}", view.rev),
        })?;
    Ok(Origin::Holds {
        rev: &view.rev,
        trees: trees_on(repo, &view.rev, slug)?,
        changed_at,
    })
}

/// Seconds since the epoch of a path's last change, when it has one.
fn mtime_unix(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(secs).ok()
}

/// One file `git status` lists under a programme.
struct Dirty {
    /// Repository-relative.
    path: String,
    deleted: bool,
}

/// What one checkout holds under `docs/programmes/`, read once for all of
/// its programmes.
struct CheckoutState {
    path: PathBuf,
    /// Per slug, the files `git status --porcelain -z -uall` lists under it.
    /// `-uall` names each file of an untracked programme; `-z` keeps odd
    /// names unquoted.
    dirty: BTreeMap<String, Vec<Dirty>>,
    /// Per slug, HEAD's tree for its directory.
    head_trees: BTreeMap<String, String>,
}

/// The slug a repository-relative path under `docs/programmes/<slug>/` is in.
fn slug_of(path: &str) -> Option<&str> {
    let (slug, _) = path.strip_prefix("docs/programmes/")?.split_once('/')?;
    Some(slug).filter(|s| !s.is_empty())
}

fn checkout_state(checkout: &Path) -> Result<CheckoutState, ProgrammeError> {
    let printed = git_stdout(
        checkout,
        &[
            "status",
            "--porcelain",
            "-z",
            "-uall",
            "--",
            "docs/programmes/",
        ],
    )?;
    let mut dirty: BTreeMap<String, Vec<Dirty>> = BTreeMap::new();
    let mut tokens = printed.split('\0').filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        let (Some(code), Some(path)) = (token.get(..2), token.get(3..)) else {
            continue;
        };
        if code.contains('R') || code.contains('C') {
            tokens.next(); // a rename or copy is followed by its origin path
        }
        if let Some(slug) = slug_of(path) {
            dirty.entry(slug.to_string()).or_default().push(Dirty {
                path: path.to_string(),
                deleted: code.contains('D'),
            });
        }
    }
    let born = git_stdout(checkout, &["rev-parse", "-q", "--verify", "HEAD"]).is_ok(); // exit 1: an unborn HEAD, which holds nothing committed
    let listed = if born {
        git_stdout(checkout, &["ls-tree", "-z", "HEAD", "docs/programmes/"])?
    } else {
        String::new()
    };
    let head_trees = listed
        .split('\0')
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            let mut fields = meta.split(' ');
            let (_, kind, sha) = (fields.next()?, fields.next()?, fields.next()?);
            let slug = path.strip_prefix("docs/programmes/")?;
            (kind == "tree" && !slug.is_empty()).then(|| (slug.to_string(), sha.to_string()))
        })
        .collect();
    Ok(CheckoutState {
        path: checkout.to_path_buf(),
        dirty,
        head_trees,
    })
}

/// When a changed file last changed: its own mtime, or for a deleted file
/// the mtime of the nearest directory still on disk, which the deletion
/// touched.
fn changed_at(checkout: &Path, entry: &Dirty) -> Option<i64> {
    let path = checkout.join(&entry.path);
    if !entry.deleted {
        return mtime_unix(&path);
    }
    path.ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(checkout))
        .find(|dir| dir.is_dir())
        .and_then(mtime_unix)
}

/// The recency (Unix seconds) of the local work a checkout has on the
/// programme `slug`, or `None` when it has none (SPEC §31).
///
/// Uncommitted changes count, and so does a HEAD whose tree for the
/// directory origin never held. Without origin, a checkout that has the
/// files counts. The recency is the newest change among the listed files
/// and the newest local commit touching the directory.
// holds-until: a person reports a copy chosen or set aside because a
// checkout or a rebase reset its files' mtimes; then a content-based
// recency (the commit or stash that last touched each file) replaces it.
fn local_work(
    state: &CheckoutState,
    slug: &str,
    origin: &Origin<'_>,
) -> Result<Option<i64>, ProgrammeError> {
    let rel = programme_rel(slug);
    let dirty = state.dirty.get(slug).map_or(&[][..], Vec::as_slice);
    let head_tree = state.head_trees.get(slug);
    let committed = match origin {
        Origin::Absent => state.path.join(&rel).join(MAP_FILE).is_file(),
        Origin::Lacks { .. } => head_tree.is_some(),
        Origin::Holds { trees, .. } => head_tree.is_some_and(|tree| !trees.contains(tree)),
    };
    if dirty.is_empty() && !committed {
        return Ok(None);
    }
    let range = origin
        .rev()
        .map_or_else(|| "HEAD".to_string(), |rev| format!("{rev}..HEAD"));
    let committed_at = if committed {
        git_stdout(
            &state.path,
            &["log", "-1", "--format=%ct", &range, "--", &rel],
        )?
        .trim()
        .parse::<i64>()
        .unwrap_or(0) // no commit in the range touches it: the files decide
    } else {
        0
    };
    let files_at = dirty
        .iter()
        .filter_map(|entry| changed_at(&state.path, entry))
        .max()
        .unwrap_or(0);
    Ok(Some(committed_at.max(files_at)))
}

/// The linked worktrees of the repository whose main checkout is `repo`,
/// read from `.git/worktrees/*/gitdir` without running git: each that
/// exists on disk, by path.
fn linked_worktrees(repo: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(repo.join(".git").join("worktrees")) else {
        return Vec::new();
    };
    let mut linked: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("gitdir")).ok())
        .filter_map(|gitdir| Path::new(gitdir.trim()).parent().map(Path::to_path_buf))
        .filter(|path| path.join(".git").is_file())
        .collect();
    linked.sort();
    linked
}

/// The slugs whose `map.md` a checkout's files hold.
fn local_slugs(checkout: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(checkout.join("docs").join("programmes")) else {
        return BTreeSet::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join(MAP_FILE).is_file())
        .filter_map(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// Whether the repository whose main checkout is `repo` can hold a
/// programme: a checkout has `docs/programmes/`, or origin's default ref
/// does, as of the last fetch. One git call at most, so the many
/// repositories without a programme cost little.
// holds-until: a person reports a programme charted upstream, in a
// repository that had none, that `list` missed; then such repositories
// are fetched too, at the cost of a fetch per repository under the root.
pub fn may_hold_programmes(repo: &Path) -> bool {
    let on_disk = |checkout: &Path| checkout.join("docs").join("programmes").is_dir();
    if on_disk(repo) || linked_worktrees(repo).iter().any(|c| on_disk(c)) {
        return true;
    }
    let asked =
        "refs/remotes/origin/HEAD:docs/programmes\nrefs/remotes/origin/main:docs/programmes\n";
    git_stdout_with_input(repo, &["cat-file", "--batch-check"], asked.to_string())
        .is_ok_and(|printed| printed.lines().any(|line| line.contains(" tree ")))
}

/// Where one programme is read from, and the local copy it set aside.
struct Choice {
    found: Option<Found>,
    /// The ref read instead of a checkout whose local work was older than
    /// its last change: the person may expect their copy to be read.
    set_aside: Option<String>,
}

/// Where the programme `slug` of `repo` is read from, among the checkout
/// `states` (the main one first), per SPEC §31: the most recent checkout
/// with local work (a tie goes to the earlier one), when that work is no
/// older than origin's last change to the programme (an edit in the same
/// second is the person's); else origin when it holds the programme.
fn choose(
    repo: &Path,
    slug: &str,
    states: &[CheckoutState],
    view: Option<&OriginView>,
) -> Result<Choice, ProgrammeError> {
    let origin = origin_for(repo, view, slug)?;
    let mut best: Option<(i64, &CheckoutState)> = None;
    for state in states {
        if !state
            .path
            .join(programme_rel(slug))
            .join(MAP_FILE)
            .is_file()
        {
            continue;
        }
        if let Some(at) = local_work(state, slug, &origin)? {
            if best.is_none_or(|(newest, _)| at > newest) {
                best = Some((at, state));
            }
        }
    }
    let local = |state: &CheckoutState| Found {
        repo: repo.to_path_buf(),
        checkout: state.path.clone(),
        slug: slug.to_string(),
        source: Source::WorkingTree,
    };
    Ok(match (best, origin) {
        (Some((_, state)), Origin::Absent | Origin::Lacks { .. }) => Choice {
            found: Some(local(state)),
            set_aside: None,
        },
        (Some((at, state)), Origin::Holds { changed_at, .. }) if at >= changed_at => Choice {
            found: Some(local(state)),
            set_aside: None,
        },
        (best, Origin::Holds { rev, .. }) => Choice {
            found: Some(Found {
                repo: repo.to_path_buf(),
                checkout: repo.to_path_buf(),
                slug: slug.to_string(),
                source: Source::Ref(rev.to_string()),
            }),
            set_aside: best.map(|_| rev.to_string()),
        },
        (None, Origin::Absent | Origin::Lacks { .. }) => Choice {
            found: None,
            set_aside: None,
        },
    })
}

/// The repositories directly under `root`, sorted: main checkouts only (a
/// `.git` directory), since a linked worktree is reached through its main
/// checkout.
pub fn repositories(root: &Path) -> Result<Vec<PathBuf>, ProgrammeError> {
    let mut repos = Vec::new();
    for entry in std::fs::read_dir(root).map_err(read_error(root))? {
        let path = entry.map_err(read_error(root))?.path();
        if path.join(".git").is_dir() {
            repos.push(path);
        }
    }
    repos.sort();
    Ok(repos)
}

/// The programmes of one repository, and what was skipped on the way: each
/// note is a line for stderr, never a failure.
pub struct Listed {
    pub found: Vec<Found>,
    pub notes: Vec<String>,
}

/// Origin's view of a repository, and a note when git could not list its
/// ref: the files are read instead, so one repository never hides the
/// others.
fn origin_view_noted(repo: &Path) -> (Option<OriginView>, Option<String>) {
    match origin_view(repo) {
        Ok(view) => (view, None),
        Err(error) => (
            None,
            Some(format!(
                "{}: {error}; reading the files instead",
                repo.display()
            )),
        ),
    }
}

/// The programmes of the repository whose main checkout is `repo`, by slug,
/// once each. The candidates are origin's default branch and every
/// checkout, linked worktrees included (SPEC §31).
pub fn programmes_of(repo: &Path) -> Result<Listed, ProgrammeError> {
    let mut notes = Vec::new();
    if !may_hold_programmes(repo) {
        return Ok(Listed {
            found: Vec::new(),
            notes,
        });
    }
    let (view, note) = origin_view_noted(repo);
    notes.extend(note);
    let mut states = vec![checkout_state(repo)?];
    for linked in linked_worktrees(repo) {
        match checkout_state(&linked) {
            Ok(state) => states.push(state),
            Err(error) => notes.push(format!("{}: {error}; skipped", linked.display())),
        }
    }
    let mut slugs: BTreeSet<String> = states.iter().flat_map(|s| local_slugs(&s.path)).collect();
    if let Some(view) = &view {
        slugs.extend(view.slugs.iter().cloned());
    }
    let mut found = Vec::new();
    for slug in slugs {
        // A copy set aside here is a stale branch among many: `list` is
        // quiet about it; `frontier`, `show` and `check` on its directory
        // say it.
        if let Some(one) = choose(repo, &slug, &states, view.as_ref())?.found {
            found.push(one);
        }
    }
    Ok(Listed { found, notes })
}

/// A programme directory as a command named it: the checkout that holds
/// it, its repository's main checkout and its slug. The directory itself
/// may be absent (a programme only origin holds).
#[derive(Debug)]
pub struct Located {
    pub checkout: PathBuf,
    pub repo: PathBuf,
    pub slug: String,
    /// The directory as named, absolute.
    pub dir: PathBuf,
}

/// Reads `<checkout>/docs/programmes/<slug>` off the shape of `dir`, made
/// absolute without resolving it, so a directory absent on disk is still
/// named.
pub fn locate(dir: &Path) -> Result<Located, ProgrammeError> {
    let absolute = std::path::absolute(dir).map_err(read_error(dir))?;
    let layout = || ProgrammeError::Layout {
        dir: dir.to_path_buf(),
    };
    let name_of = |path: &Path| path.file_name().map(|n| n.to_string_lossy().into_owned());
    let slug = name_of(&absolute).ok_or_else(layout)?;
    let programmes = absolute.parent().ok_or_else(layout)?;
    let docs = programmes.parent().ok_or_else(layout)?;
    if name_of(programmes).as_deref() != Some("programmes")
        || name_of(docs).as_deref() != Some("docs")
    {
        return Err(layout());
    }
    let named_checkout = docs.parent().ok_or_else(layout)?;
    let checkout = named_checkout
        .canonicalize()
        .map_err(read_error(named_checkout))?;
    let repo = main_checkout(&checkout)?;
    Ok(Located {
        checkout,
        repo,
        slug,
        dir: absolute,
    })
}

/// A located programme and where it is read from, with lines for stderr.
pub struct Resolved {
    pub found: Found,
    pub notes: Vec<String>,
}

/// Where `frontier`, `show` and `check` read a located programme from
/// (SPEC §31): its checkout's files when they hold local work no older than
/// origin's last change, else origin's default branch when it holds the
/// programme, else the files. Local work set aside is named in a note, so
/// a `check` that passes on origin never hides the edits it did not read.
pub fn resolve(located: &Located) -> Result<Resolved, ProgrammeError> {
    let (view, note) = origin_view_noted(&located.repo);
    let mut notes: Vec<String> = note.into_iter().collect();
    let state = checkout_state(&located.checkout)?;
    let choice = choose(
        &located.repo,
        &located.slug,
        std::slice::from_ref(&state),
        view.as_ref(),
    )?;
    if let Some(rev) = &choice.set_aside {
        notes.push(format!(
            "{}: local work older than {rev}'s last change; reading {rev} (commit or edit the files to read yours)",
            located.dir.display()
        ));
    }
    let files = located
        .checkout
        .join(programme_rel(&located.slug))
        .join(MAP_FILE);
    let found = match choice.found {
        Some(found) => Found {
            checkout: located.checkout.clone(),
            ..found
        },
        None if files.is_file() => Found {
            repo: located.repo.clone(),
            checkout: located.checkout.clone(),
            slug: located.slug.clone(),
            source: Source::WorkingTree,
        },
        None => {
            return Err(ProgrammeError::Read {
                path: located.dir.join(MAP_FILE),
                cause: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
        }
    };
    Ok(Resolved { found, notes })
}

/// How long a fetch runs before relais asks it to stop; git's own ssh and
/// HTTP limits (5 s) normally end it first.
pub const FETCH_BACKSTOP: Duration = Duration::from_secs(10);
/// How long a fetch asked to stop has before it is killed.
const FETCH_GRACE: Duration = Duration::from_secs(2);

/// Why a fetch did not refresh origin's refs.
#[derive(Debug)]
pub enum FetchFailureKind {
    /// It was still running at the backstop.
    TimedOut(Duration),
    /// git ended with a failure; its most telling stderr line.
    Git(String),
    /// git could not be started or waited for.
    Process(std::io::Error),
}

impl std::fmt::Display for FetchFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut(after) => write!(f, "timed out after {} s", after.as_secs()),
            Self::Git(line) => f.write_str(line),
            Self::Process(error) => write!(f, "git: {error}"),
        }
    }
}

/// A fetch that failed, and the lock files it left that were not there
/// before it ran.
#[derive(Debug)]
pub struct FetchFailure {
    pub kind: FetchFailureKind,
    pub locks: Vec<PathBuf>,
}

impl std::fmt::Display for FetchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)?;
        if !self.locks.is_empty() {
            let named: Vec<String> = self.locks.iter().map(|l| l.display().to_string()).collect();
            write!(
                f,
                "; lock left: {} (remove it if no git is running)",
                named.join(", ")
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for FetchFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            FetchFailureKind::Process(error) => Some(error),
            FetchFailureKind::TimedOut(_) | FetchFailureKind::Git(_) => None,
        }
    }
}

/// Whether the repository names an `origin` remote.
pub fn has_origin(repo: &Path) -> bool {
    git_stdout(repo, &["remote", "get-url", "origin"]).is_ok() // exit 2: no such remote
}

/// The line of git's stderr that says why: the first `fatal:` or `error:`
/// line, else the first non-empty one (a `warning:` often comes first).
fn telling_line(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
        .or_else(|| lines.first())
        .map_or_else(|| "git fetch failed".to_string(), ToString::to_string)
}

/// `git fetch --prune -q origin` in `repo`, bounded by `backstop`
/// (SPEC §31; callers pass [`FETCH_BACKSTOP`]).
///
/// The child stays in relais's own process group, so a terminal Ctrl-C
/// reaches git and its ssh, and git removes its own lock files; the
/// backstop asks it to stop with SIGTERM, which git also cleans up after,
/// and kills it when it did not stop within the grace period. A parent
/// that signals relais's pid alone (a `timeout(1)`, a hook's limit) ends
/// relais and its backstop: git then runs on, bounded only by the ssh and
/// HTTP limits set here (about 5 s), and not at all over a local or `ext::`
/// transport.
pub fn fetch_origin(repo: &Path, backstop: Duration) -> Result<(), FetchFailure> {
    use std::io::Read;
    let locks_before = lock_files(repo);
    let fail = |kind: FetchFailureKind| {
        let mut locks = lock_files(repo);
        locks.retain(|lock| !locks_before.contains(lock));
        FetchFailure { kind, locks }
    };
    let mut command = crate::workspace::git_command(repo);
    command
        .args([
            "-c",
            "credential.interactive=never",
            "fetch",
            "--prune",
            "-q",
            "origin",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=5 -o ServerAliveInterval=5 -o ServerAliveCountMax=1",
        )
        .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
        .env("GIT_HTTP_LOW_SPEED_TIME", "5")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| fail(FetchFailureKind::Process(e)))?;
    let stderr = child.stderr.take();
    // Drained on its own thread so a chatty git never blocks on the pipe;
    // an ssh orphaned by the kill may hold it open, so it is joined only
    // when git ended on its own, and otherwise ends with that ssh.
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = stderr {
            drop(pipe.read_to_string(&mut text)); // a partial read still names the failure
        }
        text
    });
    let started = std::time::Instant::now();
    let mut asked: Option<std::time::Instant> = None;
    let mut killed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                drop(child.kill()); // already gone: the wait below returns at once
                drop(child.wait()); // reaped so no zombie outlives the error
                return Err(fail(FetchFailureKind::Process(e)));
            }
        }
        match asked {
            None if started.elapsed() >= backstop => {
                drop(crate::procs::terminate(child.id())); // already gone: the next try_wait says so
                asked = Some(std::time::Instant::now());
            }
            Some(at) if !killed && at.elapsed() >= FETCH_GRACE => {
                drop(child.kill()); // already reaped: the next try_wait says so
                killed = true;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if asked.is_some() {
        return Err(fail(FetchFailureKind::TimedOut(backstop)));
    }
    if status.success() {
        return Ok(());
    }
    let text = reader.join().unwrap_or_default();
    Err(fail(FetchFailureKind::Git(telling_line(&text))))
}

/// The `*.lock` files under the repository's common git directory: its
/// top level and `refs/`, where a fetch takes its locks.
fn lock_files(repo: &Path) -> Vec<PathBuf> {
    let Ok(printed) = git_stdout(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) else {
        return Vec::new();
    };
    let common = PathBuf::from(printed.trim());
    let mut locks = Vec::new();
    let mut pending = vec![(common.clone(), false), (common.join("refs"), true)];
    while let Some((dir, recurse)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.filter_map(Result::ok).map(|e| e.path()) {
            if path.is_dir() {
                if recurse {
                    pending.push((path, true));
                }
            } else if path.extension().is_some_and(|ext| ext == "lock") {
                locks.push(path);
            }
        }
    }
    locks.sort();
    locks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = crate::workspace::git_command(dir)
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

    #[test]
    fn a_fetch_past_the_backstop_is_stopped_and_leaves_no_lock() {
        let scratch = crate::test_support::short_temp_dir("fetch-backstop");
        let upstream = scratch.join("upstream");
        let local = scratch.join("local");
        for repo in [&upstream, &local] {
            std::fs::create_dir_all(repo).expect("dir");
            git(repo, &["init", "-q"]);
            git(repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        }
        let url = upstream.to_str().expect("utf8");
        git(&local, &["remote", "add", "origin", url]);
        git(
            &local,
            &[
                "config",
                "remote.origin.uploadpack",
                "sleep 60; git-upload-pack",
            ],
        );
        let started = std::time::Instant::now();
        let failure = fetch_origin(&local, Duration::from_secs(1)).expect_err("it hangs");
        assert!(
            matches!(failure.kind, FetchFailureKind::TimedOut(after) if after == Duration::from_secs(1)),
            "{failure}"
        );
        assert_eq!(failure.to_string(), "timed out after 1 s");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(lock_files(&local), Vec::<PathBuf>::new());
    }

    #[test]
    fn the_telling_line_is_the_fatal_one() {
        assert_eq!(
            telling_line("warning: redirecting\nfatal: repository not found\n"),
            "fatal: repository not found"
        );
        assert_eq!(
            telling_line("\n  remote: Not found.\n"),
            "remote: Not found."
        );
        assert_eq!(telling_line(""), "git fetch failed");
    }

    #[test]
    fn a_cat_file_batch_is_split_into_its_objects() {
        let printed = "a1 blob 3\nabc\nb2 missing\nc3 blob 0\n\n";
        assert_eq!(
            batch_contents(printed),
            [Some("abc".to_string()), None, Some(String::new())]
        );
    }
}
