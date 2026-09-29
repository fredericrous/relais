//! The effort catalog: which efforts a dispatch may ask for, resolved per
//! (harness version, model id) from three independent facts.
//!
//! Effort is data ([`EffortId`]), not a compiled list of levels. Nothing
//! here names `low` or `max` outside tests: what the CLI accepts is read
//! from its `--help`, what a model supports and in what order come from
//! machine.toml, and a fact nobody stated is [`Fact::Unknown`], never a
//! guessed set. Pure: the help text and the settings are handed in.
//!
//! 1. CLI-accepted — [`parse_cli_efforts`] over the harness `--help`.
//!    Membership only; the order the CLI lists them in is never used.
//! 2. Model support — `[[efforts.models]] supported`. `[]` is a model with
//!    no effort control.
//! 3. Order — the model's own `order`, else `[efforts] order`, lowest
//!    first. There is no built-in default.
//!
//! The admissible set is CLI-accepted ∩ model-supported ∩ authorized, in
//! the configured order. Authorized is `[routing] max_effort` read as a
//! position in that order — spend authority, never capability evidence.

use serde::{Deserialize, Serialize};

use crate::policy::{EffortId, EffortSettings};

/// One thing a source may or may not be able to tell us.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fact<T> {
    Known(T),
    /// Established absent: the flag does not exist, or the model has no
    /// effort control.
    Unsupported,
    /// Nobody stated it. Distinct from `Unsupported`: it is never read as
    /// a "no" and never filled in.
    #[default]
    Unknown,
}

/// A set of efforts as a fact. The `Vec` is a membership list; only
/// [`EffortCatalog`]'s order says which is above which.
pub type EffortSet = Fact<Vec<EffortId>>;

/// What the CLI fact says about one requested effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    /// The CLI has no `--effort`, or lists efforts without this one.
    Refused,
    /// The CLI has the flag but its help lists nothing to check against.
    Unverifiable,
}

impl EffortSet {
    pub fn admission_of(&self, effort: &EffortId) -> Admission {
        match self {
            Fact::Known(set) if set.contains(effort) => Admission::Accepted,
            Fact::Known(_) | Fact::Unsupported => Admission::Refused,
            Fact::Unknown => Admission::Unverifiable,
        }
    }
}

/// The CLI-accepted fact from a harness's `--help`: `Unsupported` when
/// there is no `--effort` flag, `Unknown` when the flag is there but no
/// list of levels can be read on its own option line, otherwise the listed
/// members. Only that option's own description is read: another option
/// that mentions `--effort`, or a list that belongs to the next option, is
/// never the effort set. Never a fallback set.
pub fn parse_cli_efforts(help: &str) -> EffortSet {
    let lines: Vec<&str> = help.lines().collect();
    let Some((first, rest)) = lines
        .iter()
        .enumerate()
        .find_map(|(at, line)| option_tail(line).map(|tail| (at, tail.to_string())))
    else {
        // No option line: a mention elsewhere (a usage string) shows the
        // flag exists but states no list.
        return if lines.iter().any(|line| mentions_flag(line)) {
            Fact::Unknown
        } else {
            Fact::Unsupported
        };
    };
    // The option's own description, which the CLI wraps onto the lines that
    // follow it until the next option or a blank line.
    let mut region = rest;
    for line in lines[first + 1..].iter().map(|line| line.trim()) {
        if line.is_empty() || line.starts_with('-') {
            break;
        }
        region.push(' ');
        region.push_str(line);
    }
    region
        .split('(')
        .skip(1)
        .filter_map(|chunk| chunk.split_once(')').map(|(inside, _)| inside))
        .find_map(listed_efforts)
        .map_or(Fact::Unknown, Fact::Known)
}

const EFFORT_FLAG: &str = "--effort";

/// Whether `tail` (the text after `--effort`) ends the flag's name, so
/// `--effort-x` is another flag.
fn ends_flag_name(tail: &str) -> bool {
    !tail
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The text after `--effort` on the option line that declares it: the flag
/// is the first thing on the line after its indentation.
fn option_tail(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix(EFFORT_FLAG)
        .filter(|tail| ends_flag_name(tail))
}

/// Whether a line names `--effort` anywhere (usage strings, other
/// options' descriptions).
fn mentions_flag(line: &str) -> bool {
    line.match_indices(EFFORT_FLAG)
        .any(|(at, _)| ends_flag_name(&line[at + EFFORT_FLAG.len()..]))
}

/// `low, medium, high` read as identifiers, or `None` when it is not a
/// list of at least two (`(default: high)` and `(experimental)` are prose).
fn listed_efforts(inside: &str) -> Option<Vec<EffortId>> {
    let mut found: Vec<EffortId> = Vec::new();
    let mut items = 0;
    for item in inside.split(',') {
        let id = EffortId::parse(item.trim()).ok()?;
        items += 1;
        if !found.contains(&id) {
            found.push(id);
        }
    }
    (items >= 2).then_some(found)
}

/// Which of the three facts is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactName {
    CliAccepted,
    ModelSupport,
    Order,
}

impl FactName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CliAccepted => "cli-accepted",
            Self::ModelSupport => "model support",
            Self::Order => "order",
        }
    }
}

/// What the three facts add up to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admissible {
    /// The admissible efforts, lowest first; empty when something is
    /// established unsupported or the authorization admits nothing.
    Set(Vec<EffortId>),
    /// Not decidable: these facts are unknown.
    Undetermined(Vec<FactName>),
}

/// The three facts for one model under one harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortCatalog {
    pub model: String,
    pub cli: EffortSet,
    pub supported: EffortSet,
    /// Lowest first.
    pub order: EffortSet,
    pub max_effort: EffortId,
}

/// Resolve the catalog for `model` from what the harness accepts
/// (`cli`, see [`parse_cli_efforts`]) and machine.toml's `[efforts]`.
pub fn resolve(
    cli: &EffortSet,
    settings: &EffortSettings,
    max_effort: &EffortId,
    model: &str,
) -> EffortCatalog {
    let entry = settings.entry_for(model);
    let supported = match entry.and_then(|entry| entry.supported.as_ref()) {
        None => Fact::Unknown,
        Some(list) if list.is_empty() => Fact::Unsupported,
        Some(list) => Fact::Known(list.clone()),
    };
    let order = entry
        .and_then(|entry| entry.order.as_ref())
        .or(settings.order.as_ref())
        .filter(|order| !order.is_empty())
        .map_or(Fact::Unknown, |order| Fact::Known(order.clone()));
    EffortCatalog {
        model: model.to_string(),
        cli: cli.clone(),
        supported,
        order,
        max_effort: max_effort.clone(),
    }
}

impl EffortCatalog {
    fn facts(&self) -> [(FactName, &EffortSet); 3] {
        [
            (FactName::CliAccepted, &self.cli),
            (FactName::ModelSupport, &self.supported),
            (FactName::Order, &self.order),
        ]
    }

    /// The facts that are `Unknown`, in a stable order.
    pub fn unknown(&self) -> Vec<FactName> {
        self.facts()
            .into_iter()
            .filter(|(_, fact)| **fact == Fact::Unknown)
            .map(|(name, _)| name)
            .collect()
    }

    /// The facts established `Unsupported`, in a stable order.
    pub fn unsupported(&self) -> Vec<FactName> {
        self.facts()
            .into_iter()
            .filter(|(_, fact)| **fact == Fact::Unsupported)
            .map(|(name, _)| name)
            .collect()
    }

    /// CLI-accepted ∩ model-supported ∩ authorized, in the configured
    /// order. An unsupported fact decides the answer (nothing is
    /// admissible) even when another is unknown.
    pub fn admissible(&self) -> Admissible {
        if !self.unsupported().is_empty() {
            return Admissible::Set(Vec::new());
        }
        let unknown = self.unknown();
        if !unknown.is_empty() {
            return Admissible::Undetermined(unknown);
        }
        let (Fact::Known(cli), Fact::Known(supported), Fact::Known(order)) =
            (&self.cli, &self.supported, &self.order)
        else {
            return Admissible::Set(Vec::new());
        };
        let authorized = order
            .iter()
            .position(|effort| *effort == self.max_effort)
            .map_or(0, |at| at + 1);
        let mut set: Vec<EffortId> = Vec::new();
        for effort in &order[..authorized] {
            if cli.contains(effort) && supported.contains(effort) && !set.contains(effort) {
                set.push(effort.clone());
            }
        }
        Admissible::Set(set)
    }

    /// The next admissible effort after `effort` in the configured order,
    /// stepping over gaps; `None` at the top, for an effort the order does
    /// not name, or when the admissible set is not decidable.
    pub fn next(&self, effort: &EffortId) -> Option<EffortId> {
        let Admissible::Set(set) = self.admissible() else {
            return None;
        };
        let Fact::Known(order) = &self.order else {
            return None;
        };
        let at = order.iter().position(|entry| entry == effort)?;
        order[at + 1..]
            .iter()
            .find(|entry| set.contains(entry))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::EffortModelEntry;

    const HELP_2_1_284: &str = include_str!("../tests/fixtures/help/claude-2.1.284.txt");
    const HELP_NO_LIST: &str = include_str!("../tests/fixtures/help/claude-effort-no-list.txt");
    const HELP_NO_FLAG: &str = include_str!("../tests/fixtures/help/claude-no-effort.txt");

    fn id(text: &str) -> EffortId {
        EffortId::parse(text).expect("a valid identifier")
    }

    fn ids(texts: &[&str]) -> Vec<EffortId> {
        texts.iter().map(|text| id(text)).collect()
    }

    fn settings(order: &[&str], models: Vec<EffortModelEntry>) -> EffortSettings {
        EffortSettings {
            order: Some(ids(order)),
            models,
        }
    }

    fn entry(model: &str, supported: &[&str]) -> EffortModelEntry {
        EffortModelEntry {
            ids: vec![model.into()],
            supported: Some(ids(supported)),
            order: None,
        }
    }

    fn sorted(mut set: Vec<EffortId>) -> Vec<String> {
        let mut names: Vec<String> = set.drain(..).map(|e| e.as_str().to_string()).collect();
        names.sort();
        names
    }

    #[test]
    fn the_2_1_284_help_parses_to_exactly_its_five_levels() {
        let Fact::Known(set) = parse_cli_efforts(HELP_2_1_284) else {
            panic!("the real 2.1.284 help lists its levels");
        };
        assert_eq!(
            sorted(set),
            ["high", "low", "max", "medium", "xhigh"],
            "membership only: the wrapped list is read, its order is not"
        );
    }

    /// The help names `--effort` and lists nothing. The answer is
    /// `Unknown`, never a guessed set.
    ///
    /// FALSIFIED: with `parse_cli_efforts` changed to return
    /// `Known({low, medium, high})` for a flag with no list, this test
    /// failed on the first `Fact::Unknown` assertion (re-verified after the
    /// parser was narrowed to the option line); the fallback was removed.
    #[test]
    fn a_flag_with_no_list_is_unknown_never_a_guessed_set() {
        assert_eq!(parse_cli_efforts(HELP_NO_LIST), Fact::Unknown);
        assert_eq!(
            parse_cli_efforts("usage: claude --effort <level> --model <m>"),
            Fact::Unknown,
            "a one-line usage names the flag and no levels"
        );
        assert_eq!(
            parse_cli_efforts("  --effort <level>  (default: high)\n"),
            Fact::Unknown,
            "prose in parentheses is not a list"
        );
    }

    /// Another option's description names `--effort` and carries a
    /// parenthesised list of its own; the effort option's list still wins.
    #[test]
    fn another_options_mention_and_list_are_not_the_effort_set() {
        let help = "\
Options:
  --permission-mode <mode>  Mode; with --effort unset it is (default, plan)
  --effort <level>          Effort level for the current session
                            (low, medium, high, xhigh, max)
  --model <model>           Model for the session
";
        let Fact::Known(set) = parse_cli_efforts(help) else {
            panic!("the effort option lists its levels");
        };
        assert_eq!(sorted(set), ["high", "low", "max", "medium", "xhigh"]);
    }

    /// The effort option line is there, but the only list after it
    /// belongs to the next option.
    #[test]
    fn a_list_belonging_to_the_next_option_is_unknown() {
        let help = "\
Options:
  --effort <level>          Effort level for the current session
  --permission-mode <mode>  Permission mode (default, plan)
";
        assert_eq!(parse_cli_efforts(help), Fact::Unknown);
    }

    #[test]
    fn a_help_without_the_flag_is_unsupported() {
        assert_eq!(parse_cli_efforts(HELP_NO_FLAG), Fact::Unsupported);
        assert_eq!(
            parse_cli_efforts("  --effort-x <a>  (low, high)\n"),
            Fact::Unsupported,
            "a different flag that starts the same way is not `--effort`"
        );
    }

    #[test]
    fn a_new_identifier_needs_no_code_change() {
        let cli = Fact::Known(ids(&["low", "medium", "high", "ultra", "max"]));
        let machine = settings(
            &["low", "medium", "high", "ultra", "max"],
            vec![entry("sonnet", &["low", "high", "ultra", "max"])],
        );
        let catalog = resolve(&cli, &machine, &id("max"), "sonnet");
        assert_eq!(
            catalog.admissible(),
            Admissible::Set(ids(&["low", "high", "ultra", "max"]))
        );
        assert_eq!(catalog.next(&id("high")), Some(id("ultra")));
        assert_eq!(catalog.next(&id("max")), None, "nothing is above the top");
        assert_eq!(
            catalog.next(&id("medium")),
            Some(id("high")),
            "an effort that is itself not admissible still has a next"
        );
    }

    #[test]
    fn a_sparse_set_steps_over_gaps() {
        let cli = Fact::Known(ids(&["low", "medium", "high"]));
        let machine = settings(
            &["low", "medium", "high"],
            vec![entry("sonnet", &["low", "high"])],
        );
        let catalog = resolve(&cli, &machine, &id("high"), "sonnet");
        assert_eq!(catalog.next(&id("low")), Some(id("high")));
    }

    #[test]
    fn the_intersection_is_ordered_by_the_configuration_not_the_listing() {
        let cli = Fact::Known(ids(&["max", "low", "high"]));
        let machine = settings(
            &["low", "high", "max"],
            vec![entry("sonnet", &["max", "high", "low"])],
        );
        let catalog = resolve(&cli, &machine, &id("max"), "sonnet");
        assert_eq!(
            catalog.admissible(),
            Admissible::Set(ids(&["low", "high", "max"]))
        );
    }

    #[test]
    fn authorization_is_a_position_in_the_order_and_never_evidence() {
        let cli = Fact::Known(ids(&["low", "medium", "high", "max"]));
        let machine = settings(
            &["low", "medium", "high", "max"],
            vec![entry("sonnet", &["low", "medium", "high", "max"])],
        );
        assert_eq!(
            resolve(&cli, &machine, &id("medium"), "sonnet").admissible(),
            Admissible::Set(ids(&["low", "medium"])),
            "everything up to and including max_effort"
        );
        assert_eq!(
            resolve(&cli, &machine, &id("ultra"), "sonnet").admissible(),
            Admissible::Set(vec![]),
            "a max_effort the order does not name authorizes nothing"
        );
    }

    #[test]
    fn a_model_that_supports_nothing_has_an_empty_admissible_set() {
        let cli = Fact::Known(ids(&["low", "high"]));
        let machine = settings(&["low", "high"], vec![entry("haiku", &[])]);
        let catalog = resolve(&cli, &machine, &id("high"), "haiku");
        assert_eq!(catalog.supported, Fact::Unsupported);
        assert_eq!(catalog.admissible(), Admissible::Set(vec![]));
        assert_eq!(catalog.unsupported(), vec![FactName::ModelSupport]);
    }

    #[test]
    fn each_missing_fact_is_reported_as_what_it_is() {
        let cli = Fact::Known(ids(&["low", "high"]));
        let machine = settings(&["low", "high"], vec![entry("sonnet", &["low"])]);

        let no_entry = resolve(&cli, &machine, &id("high"), "fable");
        assert_eq!(no_entry.supported, Fact::Unknown);
        assert_eq!(
            no_entry.admissible(),
            Admissible::Undetermined(vec![FactName::ModelSupport])
        );

        let no_order = resolve(
            &cli,
            &EffortSettings {
                order: None,
                models: vec![entry("sonnet", &["low"])],
            },
            &id("high"),
            "sonnet",
        );
        assert_eq!(no_order.order, Fact::Unknown);
        assert_eq!(
            no_order.admissible(),
            Admissible::Undetermined(vec![FactName::Order])
        );

        let cli_unknown = resolve(&Fact::Unknown, &machine, &id("high"), "sonnet");
        assert_eq!(
            cli_unknown.admissible(),
            Admissible::Undetermined(vec![FactName::CliAccepted])
        );
        assert_eq!(cli_unknown.next(&id("low")), None);

        let cli_absent = resolve(&Fact::Unsupported, &machine, &id("high"), "fable");
        assert_eq!(
            cli_absent.admissible(),
            Admissible::Set(vec![]),
            "an established absence decides even beside an unknown"
        );
        assert_eq!(cli_absent.unsupported(), vec![FactName::CliAccepted]);
    }

    #[test]
    fn a_models_own_order_beats_the_machines() {
        let cli = Fact::Known(ids(&["a", "b"]));
        let machine = settings(
            &["a", "b"],
            vec![EffortModelEntry {
                ids: vec!["sonnet".into()],
                supported: Some(ids(&["a", "b"])),
                order: Some(ids(&["b", "a"])),
            }],
        );
        let catalog = resolve(&cli, &machine, &id("a"), "sonnet");
        assert_eq!(catalog.admissible(), Admissible::Set(ids(&["b", "a"])));
    }

    #[test]
    fn a_request_is_checked_against_the_cli_fact_alone() {
        let known = Fact::Known(ids(&["low", "high"]));
        assert_eq!(known.admission_of(&id("low")), Admission::Accepted);
        assert_eq!(known.admission_of(&id("max")), Admission::Refused);
        assert_eq!(
            EffortSet::Unsupported.admission_of(&id("low")),
            Admission::Refused
        );
        assert_eq!(
            EffortSet::Unknown.admission_of(&id("low")),
            Admission::Unverifiable
        );
    }
}
