//! The `tool_use` calls of a worker transcript, and which of them write a
//! given path (SPEC §8, §10). Both scans are calculations over a
//! transcript's text: [`tool_uses`] is the one walk that holdout exposure
//! (a read-exposure test) and home-drift attribution (a write test) share.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// One `tool_use` block of a transcript, with what its record says of when
/// and where it ran.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolUse {
    pub(crate) name: String,
    pub(crate) input: serde_json::Value,
    /// The record's `cwd`, when it has one.
    pub(crate) cwd: Option<String>,
    /// The record's `timestamp`, when it has one.
    pub(crate) timestamp: Option<String>,
}

impl ToolUse {
    /// Whether this call ran at or after `since`. A record with no
    /// readable timestamp is kept: it cannot be shown to be older.
    pub(crate) fn at_or_after(&self, since: DateTime<Utc>) -> bool {
        self.timestamp
            .as_deref()
            .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
            .is_none_or(|stamp| stamp >= since)
    }
}

/// Every `tool_use` block of a transcript, in order. A line that is not
/// JSON, or holds no `tool_use` block, yields nothing.
pub(crate) fn tool_uses(text: &str) -> Vec<ToolUse> {
    let mut found = Vec::new();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let text_of = |key: &str| {
            record
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let (cwd, timestamp) = (text_of("cwd"), text_of("timestamp"));
        let content = record
            .pointer("/message/content")
            .or_else(|| record.get("content"))
            .and_then(serde_json::Value::as_array);
        for block in content.into_iter().flatten() {
            if block.get("type").and_then(serde_json::Value::as_str) != Some("tool_use") {
                continue;
            }
            found.push(ToolUse {
                name: block
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                input: block.get("input").cloned().unwrap_or_default(),
                cwd: cwd.clone(),
                timestamp: timestamp.clone(),
            });
        }
    }
    found
}

/// What a drifted path is to a write test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetKind {
    /// Written only by a command that names it.
    File,
    /// The run repository's `.git/config`, which a git command also writes
    /// without naming it.
    RepoConfig,
}

/// The path a write test looks for.
pub(crate) struct WriteTarget {
    /// The spellings a call may use for it, the absolute path first.
    spellings: Vec<String>,
    kind: TargetKind,
}

impl WriteTarget {
    pub(crate) fn new(path: &Path, home: Option<&Path>, kind: TargetKind) -> Self {
        let mut spellings = vec![separators_unified(&path.to_string_lossy())];
        if let Some(rest) = home.and_then(|home| path.strip_prefix(home).ok()) {
            let rest = separators_unified(&rest.to_string_lossy());
            spellings.push(format!("~/{rest}"));
            spellings.push(format!("$HOME/{rest}"));
            spellings.push(format!("${{HOME}}/{rest}"));
        }
        Self { spellings, kind }
    }

    fn names(&self, word: &str) -> bool {
        let word = separators_unified(word);
        self.spellings.contains(&word)
    }
}

/// `text` with Windows's `\` separators as `/`, so a path joined by the OS
/// and one a shell or tool spelled with `/` compare equal there. On unix a
/// `\` is part of a name and is kept.
fn separators_unified(text: &str) -> String {
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.to_string()
    }
}

/// What a write test needs from the world: where `~` is, and whether a
/// directory belongs to the run's repository.
pub(crate) struct WriteWorld<'a> {
    pub(crate) home: Option<&'a Path>,
    pub(crate) in_run_repo: &'a dyn Fn(&Path) -> bool,
}

const FILE_EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Whether a tool of this name can write a path at all: a file-edit tool
/// or `Bash`. Every other call is a mention at most.
pub(crate) fn is_writing_tool(name: &str) -> bool {
    FILE_EDIT_TOOLS.contains(&name) || name == "Bash"
}

/// Whether `call` writes `target`: a file-edit tool whose path is the
/// target, or a `Bash` command that names it as a write target. A mere
/// mention (a read, a search, quoted text) is not a write.
pub(crate) fn writes(call: &ToolUse, target: &WriteTarget, world: &WriteWorld<'_>) -> bool {
    if FILE_EDIT_TOOLS.contains(&call.name.as_str()) {
        return ["file_path", "notebook_path"].iter().any(|key| {
            call.input
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|given| target.names(given))
        });
    }
    if call.name != "Bash" {
        return false;
    }
    let Some(command) = call
        .input
        .get("command")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let start = call.cwd.as_deref().map(PathBuf::from);
    command_writes(command, start, target, world, 0)
}

/// A shell word: its text with quotes removed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    /// `>`, `>>`, `>|`, `&>`, `n>`, `n>>`: the next word is written.
    Redirect,
    /// `;`, `&&`, `||`, `|`, `&` or a newline.
    Separator,
}

fn tokens(command: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    let flush = |out: &mut Vec<Token>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            out.push(Token::Word(std::mem::take(word)));
            *in_word = false;
        }
    };
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                in_word = true;
                for inner in chars.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                    word.push(inner);
                }
            }
            '"' => {
                in_word = true;
                while let Some(inner) = chars.next() {
                    match inner {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                            word.extend(chars.next());
                        }
                        other => word.push(other),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.extend(chars.next());
            }
            ' ' | '\t' => flush(&mut out, &mut word, &mut in_word),
            '\n' | ';' | '|' => {
                flush(&mut out, &mut word, &mut in_word);
                if ch == '|' && matches!(chars.peek(), Some('|' | '&')) {
                    chars.next();
                }
                out.push(Token::Separator);
            }
            '&' if chars.peek() == Some(&'>') => {
                flush(&mut out, &mut word, &mut in_word);
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                out.push(Token::Redirect);
            }
            '&' => {
                flush(&mut out, &mut word, &mut in_word);
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
                out.push(Token::Separator);
            }
            '>' => {
                // `n>` and `n>>` carry a file descriptor; `>&n` duplicates one.
                if in_word && word.chars().all(|digit| digit.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                } else {
                    flush(&mut out, &mut word, &mut in_word);
                }
                if matches!(chars.peek(), Some('>' | '|')) {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
                out.push(Token::Redirect);
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    flush(&mut out, &mut word, &mut in_word);
    out
}

const SHELLS: [&str; 4] = ["sh", "bash", "zsh", "dash"];
const PREFIX_COMMANDS: [&str; 4] = ["sudo", "env", "command", "exec"];
/// The nesting of `sh -c '…'` followed.
const MAX_SHELL_DEPTH: u8 = 2;

fn command_writes(
    command: &str,
    start: Option<PathBuf>,
    target: &WriteTarget,
    world: &WriteWorld<'_>,
    depth: u8,
) -> bool {
    let mut cwd = start;
    for simple in tokens(command).split(|token| *token == Token::Separator) {
        let step = simple_writes(simple, cwd.as_deref(), target, world, depth);
        if step.writes {
            return true;
        }
        if let Some(next) = step.cwd_after {
            cwd = next;
        }
    }
    false
}

/// What one simple command did: whether it wrote the target, and, for a
/// `cd`, the directory the next command runs in (`Some(None)` when it
/// cannot be known).
struct Step {
    writes: bool,
    cwd_after: Option<Option<PathBuf>>,
}

impl Step {
    fn writes(writes: bool) -> Self {
        Self {
            writes,
            cwd_after: None,
        }
    }
}

// holds-until: a worker's write to `.git/config` through a form this test
// does not read (a subshell `( … )`, `xargs`, `make`, a script) is
// counted as the known writer's. Only direct shell words are read: `cd`,
// redirections, the commands listed here and `sh -c` two levels deep. It
// ends when a pilot or a recorded drift shows a worker write counted as a
// known writer; then the subshell and `xargs` forms are read too.
fn simple_writes(
    simple: &[Token],
    cwd: Option<&Path>,
    target: &WriteTarget,
    world: &WriteWorld<'_>,
    depth: u8,
) -> Step {
    let mut words: Vec<&str> = Vec::new();
    let mut after_redirect = false;
    for token in simple {
        match token {
            Token::Redirect => after_redirect = true,
            Token::Word(word) if after_redirect => {
                if target.names(word) {
                    return Step::writes(true);
                }
                after_redirect = false;
            }
            Token::Word(word) => words.push(word),
            Token::Separator => {}
        }
    }
    let mut rest = words.as_slice();
    while let [first, tail @ ..] = rest {
        let is_assignment = first
            .split_once('=')
            .is_some_and(|(name, _)| !name.is_empty() && name.chars().all(is_name_char));
        if is_assignment || PREFIX_COMMANDS.contains(first) {
            rest = tail;
        } else {
            break;
        }
    }
    let Some((name, args)) = rest.split_first() else {
        return Step::writes(false);
    };
    let name = name.rsplit('/').next().unwrap_or(name);
    let writes = match name {
        "cd" => {
            return Step {
                writes: false,
                cwd_after: args.first().map(|dir| resolve(cwd, dir, world.home)),
            };
        }
        "tee" | "rm" | "chmod" | "touch" => args.iter().any(|arg| target.names(arg)),
        "sed" => {
            args.iter().any(|arg| is_sed_in_place(arg)) && args.iter().any(|arg| target.names(arg))
        }
        "cp" | "mv" | "ln" | "install" => args.last().is_some_and(|last| target.names(last)),
        "git" => match target.kind {
            TargetKind::RepoConfig => git_writes_config(args, cwd, world),
            TargetKind::File => false,
        },
        shell if SHELLS.contains(&shell) && depth < MAX_SHELL_DEPTH => {
            match args.iter().position(|arg| *arg == "-c") {
                Some(at) => args.get(at + 1).is_some_and(|script| {
                    command_writes(script, cwd.map(Path::to_path_buf), target, world, depth + 1)
                }),
                None => false,
            }
        }
        _ => false,
    };
    Step::writes(writes)
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn is_sed_in_place(arg: &str) -> bool {
    if arg.starts_with("--") {
        return arg == "--in-place" || arg.starts_with("--in-place=");
    }
    arg.strip_prefix('-').is_some_and(|flags| {
        flags
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .any(|flag| flag == 'i')
    })
}

/// Where `dir` is, from `cwd`. `None` for a relative `dir` with no known
/// `cwd`: resolving it against relais's own directory would guess.
fn resolve(cwd: Option<&Path>, dir: &str, home: Option<&Path>) -> Option<PathBuf> {
    if let (Some(home), Some(rest)) = (home, dir.strip_prefix("~/")) {
        return Some(home.join(rest));
    }
    if Path::new(dir).is_absolute() {
        return Some(PathBuf::from(dir));
    }
    cwd.map(|base| base.join(dir))
}

/// Whether a `git` command, run from `cwd` (after its own `-C`), writes the
/// run repository's `.git/config`. Read-only forms never do.
fn git_writes_config(args: &[&str], cwd: Option<&Path>, world: &WriteWorld<'_>) -> bool {
    let mut dir = cwd.map(Path::to_path_buf);
    let mut rest = args;
    while let [first, tail @ ..] = rest {
        match *first {
            "-C" => {
                let Some((given, after)) = tail.split_first() else {
                    return false;
                };
                let Some(resolved) = resolve(dir.as_deref(), given, world.home) else {
                    return false;
                };
                dir = Some(resolved);
                rest = after;
            }
            "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                rest = tail.get(1..).unwrap_or_default();
            }
            flag if flag.starts_with('-') => rest = tail,
            _ => break,
        }
    }
    let Some((subcommand, options)) = rest.split_first() else {
        return false;
    };
    let Some(dir) = dir else { return false };
    git_subcommand_writes(subcommand, options) && (world.in_run_repo)(&dir)
}

/// `flag` among `options`, as a long option or inside a short bundle.
fn has_flag(options: &[&str], long: &[&str], short: &[char]) -> bool {
    options.iter().any(|option| {
        let option = option.split('=').next().unwrap_or(option);
        long.contains(&option)
            || (!option.starts_with("--")
                && option
                    .strip_prefix('-')
                    .is_some_and(|flags| flags.chars().any(|flag| short.contains(&flag))))
    })
}

fn positionals<'a>(options: &[&'a str]) -> Vec<&'a str> {
    options
        .iter()
        .copied()
        .filter(|option| !option.starts_with('-'))
        .collect()
}

fn git_subcommand_writes(subcommand: &str, options: &[&str]) -> bool {
    let words = positionals(options);
    match subcommand {
        "config" => {
            if has_flag(
                options,
                &["--global", "--system", "--file", "--blob", "--worktree"],
                &['f'],
            ) {
                return false;
            }
            if words.first().is_some_and(|first| {
                ["set", "unset", "rename-section", "remove-section"].contains(first)
            }) {
                return true;
            }
            if has_flag(
                options,
                &[
                    "--unset",
                    "--unset-all",
                    "--add",
                    "--replace-all",
                    "--rename-section",
                    "--remove-section",
                    "--edit",
                ],
                &['e'],
            ) {
                return true;
            }
            let reads = has_flag(
                options,
                &[
                    "--get",
                    "--get-all",
                    "--get-regexp",
                    "--get-urlmatch",
                    "--list",
                ],
                &['l'],
            ) || words
                .first()
                .is_some_and(|first| ["get", "list"].contains(first));
            !reads && words.len() >= 2
        }
        "branch" => {
            if has_flag(
                options,
                &[
                    "--delete",
                    "--move",
                    "--copy",
                    "--set-upstream-to",
                    "--unset-upstream",
                    "--edit-description",
                ],
                &['d', 'D', 'm', 'M', 'c', 'C', 'u'],
            ) {
                return true;
            }
            let lists = has_flag(
                options,
                &[
                    "--show-current",
                    "--list",
                    "--contains",
                    "--no-contains",
                    "--merged",
                    "--no-merged",
                    "--points-at",
                    "--format",
                ],
                &['l', 'a', 'r', 'v'],
            );
            !lists && !words.is_empty()
        }
        "worktree" => words.first() == Some(&"add") && has_flag(options, &[], &['b', 'B']),
        "push" => has_flag(options, &["--set-upstream"], &['u']),
        "checkout" => has_flag(options, &[], &['b', 'B']),
        "switch" => has_flag(options, &["--create", "--force-create"], &['c', 'C']),
        "remote" => words
            .first()
            .is_some_and(|first| ["add", "rm", "remove", "rename", "set-url"].contains(first)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/u";
    const REPO: &str = "/work/repo";

    fn call(name: &str, input: serde_json::Value, cwd: Option<&str>) -> ToolUse {
        ToolUse {
            name: name.to_string(),
            input,
            cwd: cwd.map(str::to_string),
            timestamp: None,
        }
    }

    fn bash(command: &str) -> ToolUse {
        call(
            "Bash",
            serde_json::json!({ "command": command }),
            Some(REPO),
        )
    }

    fn writes_netrc(call: &ToolUse) -> bool {
        let path = PathBuf::from(HOME).join(".netrc");
        let target = WriteTarget::new(&path, Some(Path::new(HOME)), TargetKind::File);
        let world = WriteWorld {
            home: Some(Path::new(HOME)),
            in_run_repo: &|_| false,
        };
        writes(call, &target, &world)
    }

    fn writes_repo_config(call: &ToolUse) -> bool {
        let path = PathBuf::from(REPO).join(".git").join("config");
        let target = WriteTarget::new(&path, Some(Path::new(HOME)), TargetKind::RepoConfig);
        let world = WriteWorld {
            home: Some(Path::new(HOME)),
            in_run_repo: &|dir| dir.starts_with(REPO),
        };
        writes(call, &target, &world)
    }

    #[test]
    fn a_file_edit_tool_on_the_path_writes_it() {
        for name in ["Edit", "Write", "MultiEdit"] {
            let input = serde_json::json!({ "file_path": "/home/u/.netrc" });
            assert!(writes_netrc(&call(name, input, None)), "{name}");
        }
        let notebook = serde_json::json!({ "notebook_path": "~/.netrc" });
        assert!(writes_netrc(&call("NotebookEdit", notebook, None)));
        let read = serde_json::json!({ "file_path": "/home/u/.netrc" });
        assert!(!writes_netrc(&call("Read", read, None)));
        let other = serde_json::json!({ "file_path": "/home/u/.netrcx" });
        assert!(!writes_netrc(&call("Write", other, None)));
    }

    #[test]
    fn a_command_writes_a_path_by_redirect_or_a_writing_program() {
        let writing = [
            r#"echo x >> "$HOME/.netrc""#,
            "echo x > ~/.netrc",
            "echo x >| /home/u/.netrc",
            "echo x &> ${HOME}/.netrc",
            "echo x 2>> '/home/u/.netrc'",
            "echo x>/home/u/.netrc",
            "printf a | tee ~/.netrc",
            "sed -i 's/a/b/' ~/.netrc",
            "sed -ni 's/a/b/p' ~/.netrc",
            "rm -f ~/.netrc",
            "chmod 600 ~/.netrc",
            "touch ~/.netrc",
            "cp /tmp/x ~/.netrc",
            "mv /tmp/x $HOME/.netrc",
            "ln -s /tmp/x ~/.netrc",
            "install -m 600 /tmp/x ~/.netrc",
            "cd /tmp && FOO=1 cp x ~/.netrc",
            r#"bash -c 'echo x > ~/.netrc'"#,
        ];
        for command in writing {
            assert!(writes_netrc(&bash(command)), "{command}");
        }
    }

    #[test]
    fn a_mention_of_a_path_is_not_a_write() {
        let reading = [
            "cat ~/.netrc",
            "grep machine ~/.netrc",
            "cp ~/.netrc /tmp/copy",
            "sed -n 1p ~/.netrc",
            "ls /home/u",
            r#"echo "see ~/.netrc""#,
            "echo ~/.netrc > /tmp/out",
            "cat ~/.netrc | wc -l",
        ];
        for command in reading {
            assert!(!writes_netrc(&bash(command)), "{command}");
        }
        let prompt = serde_json::json!({ "prompt": "do not touch ~/.netrc" });
        assert!(!writes_netrc(&call("Task", prompt, None)));
    }

    #[test]
    fn a_writing_git_form_in_the_run_repository_writes_its_config() {
        let writing = [
            "git config branch.t.pushRemote x",
            "git config set user.name x",
            "git config --unset branch.t.remote",
            "git config --add remote.o.fetch x",
            "git config --local core.x y",
            "git branch t",
            "git branch -D t",
            "git branch -u origin/main",
            "git branch --set-upstream-to=origin/main",
            "git worktree add -b t ../t",
            "git push -u origin t",
            "git push --set-upstream origin t",
            "git checkout -b t",
            "git switch -c t",
            "git remote add o url",
            "git remote set-url o url",
            "git -C /work/repo branch t",
            "cd /work/repo && git branch t",
        ];
        for command in writing {
            assert!(writes_repo_config(&bash(command)), "{command}");
        }
    }

    #[test]
    fn a_read_only_git_form_or_another_repository_writes_nothing() {
        let reading = [
            "git branch",
            "git branch --show-current",
            "git branch -a",
            "git branch --list 'f*'",
            "git config --get branch.t.remote",
            "git config user.name",
            "git config --list",
            "git config --global user.name x",
            "git remote -v",
            "git remote show origin",
            "git status",
            "git -C /elsewhere branch t",
        ];
        for command in reading {
            assert!(!writes_repo_config(&bash(command)), "{command}");
        }
        let elsewhere = call(
            "Bash",
            serde_json::json!({ "command": "git branch t" }),
            Some("/elsewhere"),
        );
        assert!(!writes_repo_config(&elsewhere));
        for relative in ["git -C sub branch t", "cd sub && git branch t"] {
            let unknown_base = call("Bash", serde_json::json!({ "command": relative }), None);
            assert!(!writes_repo_config(&unknown_base), "{relative}");
        }
        let no_directory = call(
            "Bash",
            serde_json::json!({ "command": "git branch t" }),
            None,
        );
        assert!(!writes_repo_config(&no_directory));
    }

    #[test]
    fn a_call_older_than_the_snapshot_is_not_at_or_after_it() {
        let since = DateTime::parse_from_rfc3339("2026-11-26T10:00:00Z")
            .expect("time")
            .with_timezone(&Utc);
        let mut old = bash("true");
        old.timestamp = Some("2026-11-26T09:59:59Z".into());
        assert!(!old.at_or_after(since));
        old.timestamp = Some("2026-11-26T10:00:00Z".into());
        assert!(old.at_or_after(since));
        old.timestamp = None;
        assert!(old.at_or_after(since));
    }

    #[test]
    fn the_walk_reads_tool_use_blocks_with_their_record_context() {
        let line = r#"{"type":"assistant","cwd":"/w","timestamp":"2026-11-26T10:00:00Z","message":{"content":[{"type":"text","text":"x"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        let found = tool_uses(&format!("not json\n{line}"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "Bash");
        assert_eq!(found[0].cwd.as_deref(), Some("/w"));
        assert_eq!(found[0].timestamp.as_deref(), Some("2026-11-26T10:00:00Z"));
    }
}
