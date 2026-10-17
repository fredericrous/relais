//! A verification profile proposed from what a repository already says,
//! for `relais init --detect`.
//!
//! Only TEXT is read. Nothing here runs `make -n`, `npm`, or any other
//! program of the repository's: GNU make evaluates `$(shell …)` while it
//! parses a Makefile, so even asking make which targets exist would run
//! repository code before a person agreed to anything. The one program run
//! is the person's own `amont`, from PATH, asked what it declares
//! (`amont list --json`, which reads its configuration and runs no check):
//! amont owns its configuration's grammar, so relais does not parse it
//! (decisions ADR-0037). A proposal is a
//! suggestion; what the person accepts is written to `relais.toml` and
//! hashed into the trust grant like any hand-written policy (SPEC §5).

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::{lockfiles, Ecosystem};
use crate::policy::INIT_TEMPLATE;

/// The profile every proposal fills.
pub const DETECTED_PROFILE: &str = "default";

/// One argv relais would declare, and where it was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposedCommand {
    pub argv: Vec<String>,
    /// What in the repository this came from: `Makefile: target check`,
    /// `package.json scripts.test`, `pnpm-lock.yaml`, `--command`.
    pub source: String,
}

/// Something seen in the repository and deliberately not proposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub what: String,
    pub reason: String,
}

/// The `[integrations]` a proposal declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposedIntegrations {
    pub aval: &'static str,
    pub amont: &'static str,
    pub amont_agent: &'static str,
}

/// What `relais init --detect` would write, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proposal {
    pub profile: &'static str,
    pub setup: Vec<ProposedCommand>,
    pub commands: Vec<ProposedCommand>,
    pub skipped: Vec<Skipped>,
    pub integrations: ProposedIntegrations,
    /// The `relais.toml` this proposal writes. Kept in step with the
    /// fields above: every constructor renders it.
    pub toml: String,
    /// What detection itself wants a person to know, one line each: a
    /// degraded way of reading the repository, say. Diagnostics, not part
    /// of the proposal; absent from JSON when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Proposal {
    /// No command was found. A profile without one verifies nothing, so
    /// this is "nothing detected" whatever setup or skips it carries.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// The same proposal with its commands replaced by the one a person
    /// typed. Setup, skips and integrations are kept.
    pub fn with_command(mut self, argv: Vec<String>, source: &str) -> Self {
        self.commands = vec![ProposedCommand {
            argv,
            source: source.to_string(),
        }];
        self.toml = render_toml(&self.setup, &self.commands, &self.integrations);
        self
    }
}

/// Read the repository at `root` and propose a policy. Pure text
/// parsing: no file is written and no program is run.
pub fn detect_policy(root: &Path) -> Proposal {
    detect_policy_with(root, crate::tooling::which("amont").as_deref())
}

/// [`detect_policy`], with the `amont` it asks named (`None`: not on PATH).
fn detect_policy_with(root: &Path, amont: Option<&Path>) -> Proposal {
    let mut skipped = Vec::new();
    let mut notes = Vec::new();
    let commands = detect_commands(root, &mut skipped);
    let setup = detect_setup(root, &mut skipped);
    skipped.extend(amont_gates(root, amont, &mut notes));
    let integrations = ProposedIntegrations {
        amont: required_if(root.join("amont.conf").is_file()),
        aval: required_if(root.join(".adr.yaml").is_file()),
        amont_agent: "optional",
    };
    let toml = render_toml(&setup, &commands, &integrations);
    Proposal {
        profile: DETECTED_PROFILE,
        setup,
        commands,
        skipped,
        integrations,
        toml,
        notes,
    }
}

fn required_if(present: bool) -> &'static str {
    if present {
        "required"
    } else {
        "optional"
    }
}

fn command(argv: &[&str], source: impl Into<String>) -> ProposedCommand {
    ProposedCommand {
        argv: argv.iter().map(|arg| arg.to_string()).collect(),
        source: source.into(),
    }
}

/// The command ladder: first match wins.
fn detect_commands(root: &Path, skipped: &mut Vec<Skipped>) -> Vec<ProposedCommand> {
    if let Some((file, targets)) = makefile_targets(root) {
        for target in ["check", "test"] {
            if targets.iter().any(|t| t == target) {
                if target == "check" && targets.iter().any(|t| t == "test") {
                    skipped.push(Skipped {
                        what: format!("{file}: target test"),
                        reason: "target check was proposed first".into(),
                    });
                }
                return vec![command(
                    &["make", target],
                    format!("{file}: target {target}"),
                )];
            }
        }
    }
    if root.join("Cargo.toml").is_file() {
        return vec![command(&["cargo", "test"], "Cargo.toml")];
    }
    if root.join("go.mod").is_file() {
        return vec![command(&["go", "test", "./..."], "go.mod")];
    }
    if let Some(found) = package_json_test(root, skipped) {
        return vec![found];
    }
    if let Some(found) = pyproject_pytest(root) {
        return vec![found];
    }
    Vec::new()
}

/// The first Makefile make itself would read, and the explicit targets
/// it defines — read as text, a line at a time.
fn makefile_targets(root: &Path) -> Option<(&'static str, Vec<String>)> {
    // Names compared against the directory listing, not probed with
    // `is_file`: on a case-insensitive filesystem `makefile` would open
    // `Makefile` and the source would name a file that is not there.
    let present: Vec<String> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    for name in ["GNUmakefile", "makefile", "Makefile"] {
        if !present.iter().any(|p| p == name) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(name)) else {
            continue;
        };
        return Some((name, rule_targets(&text)));
    }
    None
}

/// The target names of every rule line: `a b: deps` and `a::` name `a`
/// and `b`. Recipe lines (a leading tab), assignments (`X := a:b`,
/// `X = a:b`), special targets (`.PHONY`) and computed names (`$(X)`)
/// name none.
fn rule_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in text.lines() {
        if line.starts_with('\t') {
            continue;
        }
        // Make's comments run from `#` to the end of the line.
        let line = line.split('#').next().unwrap_or_default();
        let Some(colon) = line.find(':') else {
            continue;
        };
        let (head, rest) = line.split_at(colon);
        if head.contains('=') || rest.trim_start_matches(':').starts_with('=') {
            continue;
        }
        for name in head.split_whitespace() {
            if name.starts_with('.') || name.contains('$') {
                continue;
            }
            targets.push(name.to_string());
        }
    }
    targets
}

/// npm's own `npm init` placeholder, which fails by design.
const NPM_PLACEHOLDER: &str = "no test specified";

fn package_json_test(root: &Path, skipped: &mut Vec<Skipped>) -> Option<ProposedCommand> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let manifest: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => {
            skipped.push(Skipped {
                what: "package.json".into(),
                reason: "not valid JSON".into(),
            });
            return None;
        }
    };
    let script = manifest.get("scripts")?.get("test")?.as_str()?;
    if script.contains(NPM_PLACEHOLDER) {
        skipped.push(Skipped {
            what: "package.json scripts.test".into(),
            reason: "npm's placeholder, not a test".into(),
        });
        return None;
    }
    let manager = lockfiles(root)
        .into_iter()
        .map(|ecosystem| ecosystem.name)
        .find(|name| matches!(*name, "npm" | "pnpm" | "yarn" | "bun"))
        .unwrap_or("npm");
    Some(command(&[manager, "test"], "package.json scripts.test"))
}

fn pyproject_pytest(root: &Path) -> Option<ProposedCommand> {
    let text = std::fs::read_to_string(root.join("pyproject.toml")).ok()?;
    if !text.contains("pytest") {
        return None;
    }
    let source = "pyproject.toml declares pytest";
    Some(if root.join("uv.lock").is_file() {
        command(&["uv", "run", "pytest"], source)
    } else {
        command(&["pytest"], source)
    })
}

/// Package managers that install into one shared place in the tree: two
/// of them in one setup would fight over it.
fn family(ecosystem: &Ecosystem) -> &'static str {
    match ecosystem.name {
        "npm" | "pnpm" | "yarn" | "bun" => "node",
        "uv" | "poetry" | "pipenv" => "python",
        other => other,
    }
}

/// One setup step per ecosystem family, from the lockfile table.
fn detect_setup(root: &Path, skipped: &mut Vec<Skipped>) -> Vec<ProposedCommand> {
    let mut chosen: Vec<&Ecosystem> = Vec::new();
    let mut setup = Vec::new();
    for ecosystem in lockfiles(root) {
        if let Some(first) = chosen.iter().find(|c| family(c) == family(ecosystem)) {
            skipped.push(Skipped {
                what: ecosystem.lockfile.into(),
                reason: format!("{} was proposed first", first.lockfile),
            });
            continue;
        }
        chosen.push(ecosystem);
        setup.push(command(ecosystem.setup_argv, ecosystem.lockfile));
    }
    setup
}

/// How long `amont list --json` may take before relais reads the file
/// itself.
const AMONT_LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// amont's declared `block` checks: seen, never proposed. Their command
/// column is a shell line, and a policy names argv, never shell recipes
/// (SPEC §6). amont is asked: declared checks whose line says `block`
/// (see [`declares_block`]), so neither a machine's `amont.severity.*`
/// overrides nor whether this clone trusts its `amont.conf` changes the
/// answer. Without a usable answer the file is read directly, and a note
/// says so.
fn amont_gates(root: &Path, amont: Option<&Path>, notes: &mut Vec<String>) -> Vec<Skipped> {
    if !root.join("amont.conf").is_file() {
        return Vec::new();
    }
    let why = match amont {
        None => "amont not found".to_string(),
        Some(bin) => match amont_list(bin, root) {
            Ok(gates) => return gates,
            Err(why) => why,
        },
    };
    notes.push(format!("{why}; reading amont.conf directly (deprecated)"));
    amont_conf_gates(root)
}

/// The declared block checks, as `amont list --json` reports them.
fn amont_list(bin: &Path, root: &Path) -> Result<Vec<Skipped>, String> {
    let mut command = std::process::Command::new(bin);
    command.args(["list", "--json"]).current_dir(root);
    let end = crate::procs::run_with_timeout(command, AMONT_LIST_TIMEOUT, None, None)
        .map_err(|e| format!("amont list --json could not run: {e}"))?;
    if !end.ended.succeeded() {
        return Err(format!(
            "amont list --json ended with {}",
            end.ended.describe()
        ));
    }
    let listing: serde_json::Value = serde_json::from_str(&end.stdout)
        .map_err(|_| "amont list --json printed no JSON".to_string())?;
    if listing["format"] != "amont-list-v1" {
        return Err("amont list --json is not amont-list-v1".into());
    }
    let checks = listing["checks"]
        .as_array()
        .ok_or("amont list --json listed no checks")?;
    let mut gates: Vec<Skipped> = checks
        .iter()
        .filter(|c| c["source"] == "declared" && declares_block(c))
        .filter_map(|c| Some(gate(c["stage"].as_str()?, c["short_name"].as_str()?)))
        .collect();
    gates.sort_by(|a, b| a.what.cmp(&b.what));
    Ok(gates)
}

/// Does this listing entry's `amont.conf` line say `block`? Its
/// `manifest_severity` (amont 1.58) is the line's own word, trusted or not;
/// `declared_severity` reads `warn` for a check that cannot run, an
/// untrusted one included, and serves only an amont that predates it.
fn declares_block(check: &serde_json::Value) -> bool {
    match check.get("manifest_severity") {
        Some(severity) => severity == "block",
        None => check["declared_severity"] == "block",
    }
}

/// The fallback: amont.conf's `block` lines, read as text. Deprecated —
/// the grammar is amont's — and kept for a machine without amont.
fn amont_conf_gates(root: &Path) -> Vec<Skipped> {
    let Ok(text) = std::fs::read_to_string(root.join("amont.conf")) else {
        return Vec::new();
    };
    let mut gates: Vec<Skipped> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [stage, name, _scope, "block", _command, ..] => Some(gate(stage, name)),
                _ => None,
            }
        })
        .collect();
    gates.sort_by(|a, b| a.what.cmp(&b.what));
    gates
}

/// One skipped gate, labelled the same whichever way it was read.
fn gate(stage: &str, name: &str) -> Skipped {
    Skipped {
        what: format!("amont.conf: {stage} {name}"),
        reason: "not a plain command".into(),
    }
}

/// A TOML basic string, quoted and escaped.
fn quoted(text: &str) -> String {
    toml::Value::String(text.to_string()).to_string()
}

fn argv_line(argv: &[String]) -> String {
    let items: Vec<String> = argv.iter().map(|arg| quoted(arg)).collect();
    format!("argv = [{}]", items.join(", "))
}

/// A comment line: a source is repository-derived text, so a newline in
/// it must not end the comment and start a TOML line.
fn comment(text: &str) -> String {
    format!("# {}", escape_control(text))
}

/// Control characters rendered visibly (`\n`, `\xNN`), so text read from
/// a repository can never draw a line of its own.
pub fn escape_control(text: &str) -> String {
    // One escaper for every place repository text is shown to a person:
    // the trust question uses the same one.
    crate::trust::escape_display(text)
}

/// The template up to its `[integrations]` comment: schema version, the
/// model tiers and the execution defaults, verbatim, so a detected
/// policy and a templated one never drift apart.
fn template_head() -> &'static str {
    let cut = INIT_TEMPLATE
        .find("# required blocks execution")
        .expect("INIT_TEMPLATE documents [integrations]");
    &INIT_TEMPLATE[..cut]
}

fn render_toml(
    setup: &[ProposedCommand],
    commands: &[ProposedCommand],
    integrations: &ProposedIntegrations,
) -> String {
    let mut out = String::new();
    out.push_str(template_head());
    out.push_str(
        "# Proposed by `relais init --detect` from this repository's files.\n\
         # Nothing here runs until a person grants trust for it.\n\
         # required blocks execution when missing; optional gaps are reported.\n",
    );
    out.push_str("[integrations]\n");
    out.push_str(&format!("aval = {}\n", quoted(integrations.aval)));
    out.push_str(&format!("amont = {}\n", quoted(integrations.amont)));
    out.push_str(&format!(
        "amont_agent = {}\n",
        quoted(integrations.amont_agent)
    ));
    for step in setup {
        out.push('\n');
        out.push_str(&comment(&step.source));
        out.push('\n');
        out.push_str(&format!(
            "[[verification.profiles.{DETECTED_PROFILE}.setup]]\n"
        ));
        out.push_str(&argv_line(&step.argv));
        out.push_str("\ntimeout_seconds = 600\n");
    }
    for check in commands {
        out.push('\n');
        out.push_str(&comment(&check.source));
        out.push('\n');
        out.push_str(&format!(
            "[[verification.profiles.{DETECTED_PROFILE}.commands]]\n"
        ));
        out.push_str(&argv_line(&check.argv));
        out.push_str("\ntimeout_seconds = 300\n");
    }
    out
}

/// Why a typed command was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandTextError {
    Empty,
    /// A shell operator, named. Relais runs argv, never a shell line.
    ShellCharacter(char),
    UnclosedQuote(char),
}

impl std::fmt::Display for CommandTextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "the command is empty"),
            Self::ShellCharacter('\n') => write!(
                f,
                "a newline is not allowed: relais runs one argv, not a shell script (SPEC §6)"
            ),
            Self::ShellCharacter('\r') => write!(
                f,
                "a carriage return is not allowed: relais runs one argv, not a shell script (SPEC §6)"
            ),
            Self::ShellCharacter(c) => write!(
                f,
                "`{c}` is not allowed: relais runs argv, not a shell line (SPEC §6)"
            ),
            Self::UnclosedQuote(q) => write!(f, "unclosed {q} quote"),
        }
    }
}

impl std::error::Error for CommandTextError {}

/// The characters a shell would treat as an operator or an expansion.
const SHELL_CHARACTERS: &[char] = &['|', '&', ';', '<', '>', '$', '`', '(', ')', '\n', '\r'];

/// Split a typed command into argv: whitespace separates, single and
/// double quotes group, and there are no escapes (a backslash is an
/// ordinary character). Anything a shell would interpret is refused by
/// name rather than passed on, because no shell ever sees it.
pub fn parse_command_text(text: &str) -> Result<Vec<String>, CommandTextError> {
    if let Some(c) = text.chars().find(|c| SHELL_CHARACTERS.contains(c)) {
        return Err(CommandTextError::ShellCharacter(c));
    }
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    argv.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if let Some(q) = quote {
        return Err(CommandTextError::UnclosedQuote(q));
    }
    if in_word {
        argv.push(current);
    }
    if argv.is_empty() {
        return Err(CommandTextError::Empty);
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::RepoPolicy;

    fn repo(label: &str, files: &[(&str, &str)]) -> crate::test_support::TempDir {
        let dir = crate::test_support::temp_dir(label);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for (name, text) in files {
            std::fs::write(dir.join(name), text).expect("fixture");
        }
        dir
    }

    fn argvs(found: &[ProposedCommand]) -> Vec<Vec<&str>> {
        found
            .iter()
            .map(|c| c.argv.iter().map(String::as_str).collect())
            .collect()
    }

    fn parses(proposal: &Proposal) -> RepoPolicy {
        RepoPolicy::from_toml_str(&proposal.toml)
            .unwrap_or_else(|e| panic!("the proposed policy must parse: {e}\n{}", proposal.toml))
    }

    #[test]
    fn a_makefile_check_target_is_proposed_with_its_source() {
        let dir = repo(
            "detect-make-check",
            &[
                (
                    "Makefile",
                    "CARGO := cargo\n.PHONY: check test\n\ncheck: lint test\n\t$(CARGO) fmt\n\ntest:\n\tcargo test\n",
                ),
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ],
        );
        let proposal = detect_policy(&dir);
        assert_eq!(argvs(&proposal.commands), [["make", "check"]]);
        assert_eq!(proposal.commands[0].source, "Makefile: target check");
        assert!(
            proposal
                .skipped
                .iter()
                .any(|s| s.what == "Makefile: target test"),
            "{:?}",
            proposal.skipped
        );
        let policy = parses(&proposal);
        let profile = &policy.verification.profiles["default"];
        assert_eq!(profile.commands[0].argv, ["make", "check"]);
    }

    #[test]
    fn a_makefile_test_target_is_the_second_rung() {
        let dir = repo(
            "detect-make-test",
            &[("Makefile", "VAR = a:b\nall:\n\techo\ntest: all\n\techo\n")],
        );
        let proposal = detect_policy(&dir);
        assert_eq!(argvs(&proposal.commands), [["make", "test"]]);
        assert_eq!(proposal.commands[0].source, "Makefile: target test");
    }

    /// GNU make evaluates `$(shell …)` while it PARSES, so detection
    /// reading the file through make would run it.
    #[test]
    fn detection_never_runs_the_makefile() {
        let dir = repo("detect-make-shell", &[]);
        let marker = dir.join("X");
        std::fs::write(
            dir.join("Makefile"),
            format!(
                "OUT := $(shell touch {})\ncheck:\n\ttrue\n",
                marker.display()
            ),
        )
        .expect("makefile");
        let proposal = detect_policy(&dir);
        assert_eq!(argvs(&proposal.commands), [["make", "check"]]);
        assert!(!marker.exists(), "detection ran the Makefile");
    }

    #[test]
    fn a_cargo_only_repository_proposes_cargo_test() {
        let dir = repo(
            "detect-cargo",
            &[
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
                ("Cargo.lock", ""),
            ],
        );
        let proposal = detect_policy(&dir);
        assert_eq!(argvs(&proposal.commands), [["cargo", "test"]]);
        assert!(proposal.setup.is_empty(), "Cargo.lock calls for no setup");
        assert_eq!(proposal.integrations.amont, "optional");
        assert_eq!(proposal.integrations.aval, "optional");
        parses(&proposal);
    }

    #[test]
    fn a_go_module_proposes_go_test() {
        let dir = repo("detect-go", &[("go.mod", "module x\n")]);
        assert_eq!(
            argvs(&detect_policy(&dir).commands),
            [["go", "test", "./..."]]
        );
    }

    #[test]
    fn a_pnpm_repository_proposes_its_install_and_its_test() {
        let dir = repo(
            "detect-pnpm",
            &[
                ("package.json", r#"{"scripts":{"test":"vitest run"}}"#),
                ("pnpm-lock.yaml", ""),
            ],
        );
        let proposal = detect_policy(&dir);
        assert_eq!(
            argvs(&proposal.setup),
            [["pnpm", "install", "--frozen-lockfile"]]
        );
        assert_eq!(proposal.setup[0].source, "pnpm-lock.yaml");
        assert_eq!(argvs(&proposal.commands), [["pnpm", "test"]]);
        assert_eq!(proposal.commands[0].source, "package.json scripts.test");
        let policy = parses(&proposal);
        let profile = &policy.verification.profiles["default"];
        assert_eq!(
            profile.setup[0].argv,
            ["pnpm", "install", "--frozen-lockfile"]
        );
        assert_eq!(profile.commands[0].argv, ["pnpm", "test"]);
    }

    #[test]
    fn npms_placeholder_test_script_is_not_proposed() {
        let dir = repo(
            "detect-npm-placeholder",
            &[(
                "package.json",
                r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
            )],
        );
        let proposal = detect_policy(&dir);
        assert!(proposal.is_empty(), "{:?}", proposal.commands);
        assert!(proposal
            .skipped
            .iter()
            .any(|s| s.what == "package.json scripts.test"));
    }

    #[test]
    fn pytest_follows_uv_when_uv_locks_the_tree() {
        let dir = repo(
            "detect-pytest",
            &[(
                "pyproject.toml",
                "[project]\nname = \"x\"\n[dependency-groups]\ndev = [\"pytest>=8\"]\n",
            )],
        );
        assert_eq!(argvs(&detect_policy(&dir).commands), [["pytest"]]);
        std::fs::write(dir.join("uv.lock"), "").expect("uv.lock");
        let proposal = detect_policy(&dir);
        assert_eq!(argvs(&proposal.commands), [["uv", "run", "pytest"]]);
        assert_eq!(argvs(&proposal.setup), [["uv", "sync", "--frozen"]]);
    }

    const AMONT_CONF: &str = "# a comment block cargo test\npre-commit  cargo-test  *.rs  block  cargo test\npre-push  lint  *  warn  make lint\n";

    fn amont_repo(label: &str) -> crate::test_support::TempDir {
        repo(
            label,
            &[
                ("amont.conf", AMONT_CONF),
                (".adr.yaml", "areas: []\n"),
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ],
        )
    }

    /// A stand-in `amont` that prints `stdout` and exits with `code`.
    #[cfg(unix)]
    fn fake_amont(dir: &Path, stdout: &str, code: i32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("fake-amont");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\ncat <<'EOF'\n{stdout}\nEOF\nexit {code}\n"),
        )
        .expect("fake amont");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        bin
    }

    /// What `amont list --json` says of [`AMONT_CONF`], with a builtin and
    /// a machine override that must change nothing.
    const LISTING: &str = r#"{"format":"amont-list-v1","checks":[
        {"id":"pre-commit-ban-terms","short_name":"ban-terms","stage":"pre-commit","source":"builtin","declared_severity":"block","effective_severity":"block"},
        {"id":"pre-push-lint","short_name":"lint","stage":"pre-push","source":"declared","declared_severity":"warn","effective_severity":"block"},
        {"id":"pre-commit-cargo-test","short_name":"cargo-test","stage":"pre-commit","source":"declared","declared_severity":"block","manifest_severity":"block","effective_severity":"warn"}]}"#;

    /// The same repository in a clone that has not trusted its `amont.conf`:
    /// every declared check reads `warn`, and only `manifest_severity` keeps
    /// the line's word.
    const UNTRUSTED_LISTING: &str = r#"{"format":"amont-list-v1","checks":[
        {"id":"pre-push-lint","short_name":"lint","stage":"pre-push","source":"declared","declared_severity":"warn","manifest_severity":"warn","status":"unusable"},
        {"id":"pre-commit-cargo-test","short_name":"cargo-test","stage":"pre-commit","source":"declared","declared_severity":"warn","manifest_severity":"block","status":"unusable"}]}"#;

    /// An amont before 1.58, which has no `manifest_severity`.
    const OLD_LISTING: &str = r#"{"format":"amont-list-v1","checks":[
        {"id":"pre-push-lint","short_name":"lint","stage":"pre-push","source":"declared","declared_severity":"warn"},
        {"id":"pre-commit-cargo-test","short_name":"cargo-test","stage":"pre-commit","source":"declared","declared_severity":"block"}]}"#;

    fn the_cargo_test_gate() -> Vec<Skipped> {
        vec![Skipped {
            what: "amont.conf: pre-commit cargo-test".into(),
            reason: "not a plain command".into(),
        }]
    }

    #[cfg(unix)]
    #[test]
    fn amont_is_asked_for_its_declared_block_checks() {
        let dir = amont_repo("detect-amont-asked");
        let bin = fake_amont(&dir, LISTING, 0);
        let proposal = detect_policy_with(&dir, Some(&bin));
        // Declared severity, not effective: a machine's override is not the
        // repository's declaration. Builtins are not the repository's.
        assert_eq!(proposal.skipped, the_cargo_test_gate());
        assert_eq!(proposal.notes, Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn an_untrusted_clone_and_an_older_amont_give_the_same_answer() {
        for (label, listing) in [
            ("detect-amont-untrusted", UNTRUSTED_LISTING),
            ("detect-amont-old", OLD_LISTING),
        ] {
            let dir = amont_repo(label);
            let bin = fake_amont(&dir, listing, 0);
            let proposal = detect_policy_with(&dir, Some(&bin));
            assert_eq!(proposal.skipped, the_cargo_test_gate(), "{label}");
            assert_eq!(proposal.notes, Vec::<String>::new(), "{label}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_amont_that_cannot_answer_falls_back_to_the_file_and_says_why() {
        for (label, stdout, code, why) in [
            (
                "detect-amont-fails",
                LISTING,
                1,
                "amont list --json ended with exit 1",
            ),
            (
                "detect-amont-garbage",
                "not json",
                0,
                "amont list --json printed no JSON",
            ),
            (
                "detect-amont-format",
                r#"{"format":"amont-list-v2","checks":[]}"#,
                0,
                "amont list --json is not amont-list-v1",
            ),
        ] {
            let dir = amont_repo(label);
            let bin = fake_amont(&dir, stdout, code);
            let proposal = detect_policy_with(&dir, Some(&bin));
            assert_eq!(proposal.skipped, the_cargo_test_gate(), "{label}");
            assert_eq!(
                proposal.notes,
                [format!("{why}; reading amont.conf directly (deprecated)")],
                "{label}"
            );
        }
    }

    #[test]
    fn amont_block_gates_are_seen_and_not_proposed() {
        let dir = amont_repo("detect-amont");
        let proposal = detect_policy_with(&dir, None);
        assert_eq!(
            proposal.notes,
            ["amont not found; reading amont.conf directly (deprecated)"]
        );
        assert_eq!(
            proposal.skipped,
            [Skipped {
                what: "amont.conf: pre-commit cargo-test".into(),
                reason: "not a plain command".into(),
            }]
        );
        assert_eq!(argvs(&proposal.commands), [["cargo", "test"]]);
        assert_eq!(proposal.integrations.amont, "required");
        assert_eq!(proposal.integrations.aval, "required");
        assert_eq!(proposal.integrations.amont_agent, "optional");
        parses(&proposal);
    }

    #[test]
    fn an_empty_repository_proposes_nothing() {
        let dir = repo("detect-empty", &[("README.md", "# x\n")]);
        let proposal = detect_policy(&dir);
        assert!(proposal.is_empty());
        assert!(proposal.setup.is_empty());
    }

    #[test]
    fn a_typed_command_replaces_the_detected_ones_and_still_parses() {
        let dir = repo("detect-typed", &[("pnpm-lock.yaml", "")]);
        let proposal = detect_policy(&dir);
        assert!(proposal.is_empty());
        let argv = parse_command_text("make test").expect("plain");
        let proposal = proposal.with_command(argv, "--command");
        assert_eq!(argvs(&proposal.commands), [["make", "test"]]);
        let policy = parses(&proposal);
        let profile = &policy.verification.profiles["default"];
        assert_eq!(profile.commands[0].argv, ["make", "test"]);
        assert_eq!(profile.setup.len(), 1, "setup is kept");
    }

    #[test]
    fn command_text_splits_on_whitespace_and_quotes() {
        assert_eq!(
            parse_command_text("make test").expect("plain"),
            ["make", "test"]
        );
        assert_eq!(
            parse_command_text("pytest -k 'a b'").expect("single"),
            ["pytest", "-k", "a b"]
        );
        assert_eq!(
            parse_command_text("  go  test \"./...\"  ").expect("double"),
            ["go", "test", "./..."]
        );
        assert_eq!(
            parse_command_text(r"echo a\b").expect("no escapes"),
            ["echo", r"a\b"]
        );
    }

    #[test]
    fn command_text_refuses_shell_operators_by_name() {
        let refused = parse_command_text("make test | tee x").expect_err("a pipe");
        assert_eq!(refused, CommandTextError::ShellCharacter('|'));
        assert!(refused.to_string().contains('|'), "{refused}");
        for (text, c) in [
            ("a && b", '&'),
            ("a; b", ';'),
            ("a > x", '>'),
            ("a < x", '<'),
            ("echo $HOME", '$'),
            ("echo `id`", '`'),
            ("(a)", '('),
            ("make\ntest", '\n'),
        ] {
            assert_eq!(
                parse_command_text(text),
                Err(CommandTextError::ShellCharacter(c)),
                "{text:?}"
            );
        }
        assert_eq!(parse_command_text("   "), Err(CommandTextError::Empty));
        assert_eq!(parse_command_text(""), Err(CommandTextError::Empty));
        assert_eq!(
            parse_command_text("a 'b"),
            Err(CommandTextError::UnclosedQuote('\''))
        );
    }

    #[test]
    fn control_characters_are_rendered_visibly() {
        assert_eq!(escape_control("a\nAllow\x1b"), "a\\nAllow\\x1b");
        assert_eq!(escape_control("rm\u{202e}"), "rm\\u{202e}");
    }
}
