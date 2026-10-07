//! `relais native router-state`'s document (docs/router-protocol.md),
//! assembled from values the caller read: machine settings, the
//! repository policy, the environment, the ledger's
//! token totals and the agent definitions. Pure.

use std::collections::BTreeMap;

use serde::Serialize;

use super::holdout;
use super::mode::{resolve_mode, Mode};
use super::outcome::TaskOutcome;
use super::stats::median;
use super::table::{default_table, CapabilityTable};
use crate::orchestration::PriceTable;
use crate::policy::{MachineSettings, RepoPolicy, RoutingEnvelope, Tier};

/// The fixed prior per difficulty 1–5 (plan §4), in tokens.
pub const TASK_TOKENS_BY_DIFFICULTY: [u64; 5] = [20_000, 60_000, 150_000, 400_000, 800_000];

/// Below this many completed tasks of one (kind, difficulty), the fixed
/// prior stands in for the observed median.
pub const MIN_TASKS_FOR_MEDIAN: usize = 10;

/// Argv prefixes that count as verification everywhere (the contract's
/// built-in list).
pub const BUILT_IN_CHECKS: &[&[&str]] = &[
    &["cargo", "test"],
    &["cargo", "nextest"],
    &["npm", "test"],
    &["pnpm", "test"],
    &["yarn", "test"],
    &["bun", "test"],
    &["pytest"],
    &["uv", "run", "pytest"],
    &["go", "test"],
    &["make", "check"],
    &["make", "test"],
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TierModel {
    pub model: String,
    pub effort: Option<String>,
}

/// Micro-USD per million tokens; `cache_write` is the 1-hour rate (S0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rates {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Priors {
    pub task_tokens_by_difficulty: [u64; 5],
    /// `"<kind>:<difficulty>"` → the median total tokens of completed
    /// tasks of that class, only where there are at least
    /// [`MIN_TASKS_FOR_MEDIAN`].
    pub task_tokens: BTreeMap<String, u64>,
}

/// The whole document `router-state` prints.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouterState {
    pub schema: u32,
    pub mode: Mode,
    pub mode_reason: String,
    pub envelope: Option<RoutingEnvelope>,
    pub holdout: bool,
    pub seed: String,
    pub epsilon: f64,
    pub tiers: BTreeMap<&'static str, TierModel>,
    pub capability_table: CapabilityTable,
    pub rates: BTreeMap<String, Option<Rates>>,
    pub priors: Priors,
    pub pins: BTreeMap<String, String>,
    pub excluded_models: Vec<String>,
    pub checks: Vec<Vec<String>>,
    pub adjustments: Vec<serde_json::Value>,
}

/// What [`build`] is assembled from.
pub struct Inputs<'a> {
    pub session: &'a str,
    pub machine: &'a MachineSettings,
    /// The repository's policy, when the cwd is in a repository with a
    /// valid `relais.toml`.
    pub repo: Option<&'a RepoPolicy>,
    /// `RELAIS_SESSION_ROUTING`, as read.
    pub env_mode: Option<&'a str>,
    /// `(kind, difficulty, total tokens)` of completed tasks.
    pub completed_task_tokens: Vec<(String, u8, u64)>,
    /// Pins from the agent definitions, user scope first so the project's
    /// win on a shared name.
    pub definition_pins: Vec<(String, String)>,
}

/// What a machine-pinned agent with no definition pin is served as: the
/// plugin never overrides a pinned type, and `inherit` is Claude Code's
/// own word for "leave the model where it would be".
pub const MACHINE_PIN: &str = "inherit";

/// The default model per tier, when the repository names none.
fn default_tier_model(tier: Tier) -> &'static str {
    match tier {
        Tier::Research => "haiku",
        Tier::Implementation => "sonnet",
        Tier::Escalation => "opus",
    }
}

fn tiers(inputs: &Inputs<'_>) -> BTreeMap<&'static str, TierModel> {
    let routing = &inputs.machine.session_routing;
    [Tier::Research, Tier::Implementation, Tier::Escalation]
        .into_iter()
        .map(|tier| {
            let profile = inputs.repo.and_then(|repo| repo.models.get(&tier));
            let model = profile.map_or(default_tier_model(tier), |p| p.id.as_str());
            (
                tier.as_str(),
                TierModel {
                    model: routing.full_model_id(model),
                    effort: profile
                        .and_then(|p| p.effort.as_ref())
                        .map(|e| e.as_str().to_string()),
                },
            )
        })
        .collect()
}

fn rates(
    pricing: &PriceTable,
    models: impl IntoIterator<Item = String>,
) -> BTreeMap<String, Option<Rates>> {
    models
        .into_iter()
        .map(|model| {
            let rate = pricing.rate(&model).map(|price| Rates {
                input: price.input,
                output: price.output,
                cache_read: price.cache_read,
                cache_write: price.cache_write_1h,
            });
            (model, rate)
        })
        .collect()
}

/// The per-class medians, only where a class has enough tasks.
pub fn task_token_priors(totals: &[(String, u8, u64)]) -> BTreeMap<String, u64> {
    let mut by_class: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (kind, difficulty, tokens) in totals {
        by_class
            .entry(format!("{kind}:{difficulty}"))
            .or_default()
            .push(*tokens);
    }
    by_class
        .into_iter()
        .filter(|(_, tokens)| tokens.len() >= MIN_TASKS_FOR_MEDIAN)
        .filter_map(|(class, mut tokens)| median(&mut tokens).map(|m| (class, m)))
        .collect()
}

/// The repository's check argv, then the built-in list, each once.
pub fn checks(repo: Option<&RepoPolicy>) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let repo_commands = repo
        .into_iter()
        .flat_map(|repo| repo.verification.profiles.values())
        .flat_map(|profile| profile.commands.iter())
        .map(|command| command.argv.clone());
    let built_in = BUILT_IN_CHECKS
        .iter()
        .map(|argv| argv.iter().map(|word| word.to_string()).collect());
    for argv in repo_commands.chain(built_in) {
        if !argv.is_empty() && !out.contains(&argv) {
            out.push(argv);
        }
    }
    out
}

/// The outcomes whose tasks feed the token priors.
pub const COMPLETED: [&str; 2] = [
    TaskOutcome::CompletedVerified.as_str(),
    TaskOutcome::CompletedAccepted.as_str(),
];

/// The document, from its inputs.
pub fn build(inputs: Inputs<'_>) -> RouterState {
    let routing = &inputs.machine.session_routing;
    let (mode, mode_reason) = resolve_mode(
        routing.envelope.is_some(),
        routing.off.is_some(),
        inputs.env_mode,
    );
    let draw = holdout::draw(inputs.session, routing.holdout_rate);
    let tiers = tiers(&inputs);
    let pricing = inputs
        .machine
        .pricing
        .clone()
        .unwrap_or_else(PriceTable::empty)
        .with_built_in_defaults();
    let mut models: Vec<String> = tiers.values().map(|tier| tier.model.clone()).collect();
    models.extend(routing.resolved_model_ids().into_values());
    let mut pins: BTreeMap<String, String> = BTreeMap::new();
    for agent in &routing.pinned_agents {
        pins.insert(agent.clone(), MACHINE_PIN.to_string());
    }
    for (agent, model) in &inputs.definition_pins {
        pins.insert(agent.clone(), model.clone());
    }
    RouterState {
        schema: 1,
        mode,
        mode_reason,
        envelope: routing.envelope.clone(),
        holdout: draw.holdout,
        seed: draw.seed,
        epsilon: routing.effective_epsilon(),
        tiers,
        capability_table: default_table(),
        rates: rates(&pricing, models),
        priors: Priors {
            task_tokens_by_difficulty: TASK_TOKENS_BY_DIFFICULTY,
            task_tokens: task_token_priors(&inputs.completed_task_tokens),
        },
        pins,
        excluded_models: routing.excluded_models.clone(),
        checks: checks(inputs.repo),
        adjustments: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(extra: &str) -> MachineSettings {
        MachineSettings::from_toml_str(&format!("schema_version = 1\n{extra}")).unwrap()
    }

    fn inputs<'a>(machine: &'a MachineSettings, repo: Option<&'a RepoPolicy>) -> Inputs<'a> {
        Inputs {
            session: "s0",
            machine,
            repo,
            env_mode: None,
            completed_task_tokens: vec![],
            definition_pins: vec![],
        }
    }

    #[test]
    fn with_no_repository_the_tiers_are_the_defaults_as_full_ids() {
        let machine = machine("");
        let state = build(inputs(&machine, None));
        assert_eq!(state.tiers["research"].model, "claude-haiku-5-5");
        assert_eq!(state.tiers["implementation"].model, "claude-sonnet-5-5");
        assert_eq!(state.tiers["escalation"].model, "claude-opus-5-5");
        assert_eq!(state.mode, Mode::Shadow);
        assert_eq!(state.epsilon, 0.0);
        assert!(state.adjustments.is_empty());
        assert_eq!(state.checks[0], vec!["cargo", "test"]);
    }

    #[test]
    fn repository_tiers_resolve_aliases_and_keep_their_effort_and_checks() {
        let repo = RepoPolicy::from_toml_str(
            "schema_version = 1\n[models.research]\nid = \"haiku\"\n\
             [models.implementation]\nid = \"claude-sonnet-9\"\neffort = \"medium\"\n\
             [models.escalation]\nid = \"fable\"\n\
             [[verification.profiles.default.commands]]\nargv = [\"make\", \"check\"]\n\
             [[verification.profiles.default.commands]]\nargv = [\"just\", \"lint\"]\n",
        )
        .unwrap();
        let machine = machine("");
        let state = build(inputs(&machine, Some(&repo)));
        assert_eq!(state.tiers["implementation"].model, "claude-sonnet-9");
        assert_eq!(
            state.tiers["implementation"].effort.as_deref(),
            Some("medium")
        );
        assert_eq!(state.tiers["escalation"].model, "claude-fable-5-1");
        assert_eq!(state.checks[0], vec!["make", "check"]);
        assert_eq!(state.checks[1], vec!["just", "lint"]);
        // `make check` is not listed twice.
        assert_eq!(
            state
                .checks
                .iter()
                .filter(|argv| *argv == &vec!["make".to_string(), "check".into()])
                .count(),
            1
        );
    }

    #[test]
    fn rates_are_per_million_with_the_one_hour_write_and_null_when_unpriced() {
        let machine = machine(
            "[session_routing]\nmodel_ids = { opus = \"claude-mystery-9\" }\n\
             [pricing]\nversion = \"v\"\n[[pricing.models]]\nids = [\"claude-haiku-5-5\"]\n\
             input = 1000000\noutput = 5000000\ncache_read = 100000\ncache_write_5m = 1250000\n\
             cache_write_1h = 2000000\n",
        );
        let state = build(inputs(&machine, None));
        assert_eq!(
            state.rates["claude-haiku-5-5"],
            Some(Rates {
                input: 1_000_000,
                output: 5_000_000,
                cache_read: 100_000,
                cache_write: 2_000_000,
            })
        );
        // A model neither machine.toml nor the built-in list prices.
        assert_eq!(state.rates["claude-mystery-9"], None);
        let json = serde_json::to_value(&state).unwrap();
        assert!(json["rates"]["claude-mystery-9"].is_null());
    }

    #[test]
    fn haiku_is_priced_from_the_built_in_list_unless_machine_toml_prices_it() {
        let machine_without = machine("");
        let state = build(inputs(&machine_without, None));
        assert_eq!(
            state.rates["claude-haiku-5-5"],
            Some(Rates {
                input: 500_000,
                output: 2_500_000,
                cache_read: 50_000,
                cache_write: 1_000_000,
            })
        );
        // The other current models come from the built-in list too.
        assert_eq!(
            state.rates["claude-sonnet-5-5"].as_ref().map(|r| r.input),
            Some(2_000_000)
        );
    }

    #[test]
    fn priors_use_a_median_only_from_ten_completed_tasks() {
        let mut totals: Vec<(String, u8, u64)> = (0..10)
            .map(|i| ("edit".to_string(), 2, 1000 * (i + 1)))
            .collect();
        totals.extend((0..9).map(|_| ("debug".to_string(), 3, 5)));
        let priors = task_token_priors(&totals);
        assert_eq!(priors.get("edit:2"), Some(&5000));
        assert_eq!(priors.get("debug:3"), None);
    }

    #[test]
    fn pins_join_definitions_and_machine_pins() {
        let machine = machine("[session_routing]\npinned_agents = [\"keep-me\", \"both\"]\n");
        let mut inputs = inputs(&machine, None);
        inputs.definition_pins = vec![("both".into(), "opus".into())];
        let state = build(inputs);
        assert_eq!(state.pins["keep-me"], "inherit");
        assert_eq!(state.pins["both"], "opus");
    }

    #[test]
    fn the_document_has_exactly_the_contracts_keys() {
        let machine = machine("");
        let value = serde_json::to_value(build(inputs(&machine, None))).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "adjustments",
                "capability_table",
                "checks",
                "envelope",
                "epsilon",
                "excluded_models",
                "holdout",
                "mode",
                "mode_reason",
                "pins",
                "priors",
                "rates",
                "schema",
                "seed",
                "tiers",
            ]
        );
    }
}
