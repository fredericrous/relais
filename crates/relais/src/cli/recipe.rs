//! `relais recipe`: list, show, diff, evaluate, promote, rollback.

use super::*;

/// `kind` as `list`/`show` print it — never `Debug`'s capitalised
/// spelling, and named `unset` rather than empty when the recipe leaves
/// it unspecified.
pub(crate) fn kind_label(kind: Option<relais::contract::Kind>) -> &'static str {
    match kind {
        None => "unset",
        Some(relais::contract::Kind::Change) => "change",
        Some(relais::contract::Kind::Inspect) => "inspect",
    }
}

pub(crate) fn scope_label(scope_within: &[String]) -> String {
    if scope_within.is_empty() {
        "**".to_string()
    } else {
        scope_within.join(",")
    }
}

/// List every recipe `relais.toml` declares, one per line, in
/// declaration order (SPEC §26). Read-only: opens nothing but the
/// repository's own policy file.
pub(crate) fn recipe_list_command() -> Result<CliOutcome, CliError> {
    let (_root, policy) = load_repo_policy()?;
    if policy.recipes.is_empty() {
        println!("relais.toml declares no recipes");
        return Ok(CliOutcome::Accepted);
    }
    for recipe in &policy.recipes {
        println!(
            "{name}\trevision={revision}\tenabled={enabled}\ttier={tier}\tkind={kind}\t\
             scope={scope}\trecipe_id={id}",
            name = recipe.name,
            revision = recipe.revision,
            enabled = recipe.enabled,
            tier = recipe.tier.as_str(),
            kind = kind_label(recipe.kind),
            scope = scope_label(&recipe.scope_within),
            id = recipe.recipe_id(),
        );
    }
    Ok(CliOutcome::Accepted)
}

/// Show every revision of one recipe, oldest first (SPEC §26). An
/// unknown name is refused, naming the recipes that do exist, rather
/// than printing nothing.
pub(crate) fn recipe_show_command(name: &str) -> Result<CliOutcome, CliError> {
    let (_root, policy) = load_repo_policy()?;
    let mut revisions: Vec<&RecipeSpec> =
        policy.recipes.iter().filter(|r| r.name == name).collect();
    if revisions.is_empty() {
        let known: Vec<&str> = policy.recipes.iter().map(|r| r.name.as_str()).collect();
        let detail = if known.is_empty() {
            format!("relais recipe show: no recipe named `{name}`; relais.toml declares no recipes")
        } else {
            format!(
                "relais recipe show: no recipe named `{name}`; declared recipes: {}",
                known.join(", ")
            )
        };
        return Err(CliError::Usage { detail });
    }
    revisions.sort_by_key(|r| r.revision);
    for recipe in revisions {
        println!(
            "{name} revision {revision} (recipe_id={id}, enabled={enabled}, tier={tier}, \
             kind={kind}, scope={scope})",
            name = recipe.name,
            revision = recipe.revision,
            id = recipe.recipe_id(),
            enabled = recipe.enabled,
            tier = recipe.tier.as_str(),
            kind = kind_label(recipe.kind),
            scope = scope_label(&recipe.scope_within),
        );
        // `models` is read by routing. `execution`, `context` and `review`
        // are DECLARED AND HASHED, NOT YET READ by routing: the exact
        // caveat `RecipeSpec::execution`'s field doc carries, repeated
        // here rather than left implicit, so this command cannot be read
        // as saying those blocks take effect.
        if let Some(models) = &recipe.models {
            println!(
                "  models: {}",
                serde_json::to_string(models).unwrap_or_default()
            );
        }
        for (label, json) in [
            (
                "execution",
                recipe
                    .execution
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
            ),
            (
                "context",
                recipe
                    .context
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
            ),
            (
                "review",
                recipe
                    .review
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default()),
            ),
        ] {
            if let Some(json) = json {
                println!("  {label}: {json} — DECLARED AND HASHED, NOT YET READ by routing");
            }
        }
    }
    Ok(CliOutcome::Accepted)
}

/// Mirrors `RecipeSpec` field for field, but with no `skip_serializing_if`:
/// every field always appears in the JSON, holding the recipe's EFFECTIVE
/// value, never omitted just because it equals a default. `RecipeSpec`'s
/// own `Serialize` skips default-valued fields (so a `relais.toml` written
/// before a field existed keeps hashing the same); comparing that compact
/// form directly would print a defaulted field as `null` instead of the
/// value it actually resolves to.
///
/// Built by destructuring `RecipeSpec` with no `..`, so this fails to
/// compile — not silently drops the new field — the day `RecipeSpec`
/// gains one.
#[derive(Serialize)]
pub(crate) struct RecipeSpecFull {
    name: String,
    kind: Option<relais::contract::Kind>,
    scope_within: Vec<String>,
    tier: relais::policy::Tier,
    revision: u32,
    enabled: bool,
    models: Option<std::collections::BTreeMap<relais::policy::Tier, relais::policy::ModelProfile>>,
    execution: Option<relais::policy::ExecutionPolicy>,
    context: Option<relais::policy::ContextPolicy>,
    review: Option<relais::contract::Review>,
    task_type: Vec<relais::contract::TaskType>,
    requires_design: bool,
}

impl From<&RecipeSpec> for RecipeSpecFull {
    fn from(spec: &RecipeSpec) -> Self {
        let RecipeSpec {
            name,
            kind,
            scope_within,
            tier,
            revision,
            enabled,
            models,
            execution,
            context,
            review,
            task_type,
            requires_design,
        } = spec.clone();
        Self {
            name,
            kind,
            scope_within,
            tier,
            revision,
            enabled,
            models,
            execution,
            context,
            review,
            task_type,
            requires_design,
        }
    }
}

/// Every field the two recipes' EFFECTIVE values disagree on, name and
/// JSON value on each side. Derived from [`RecipeSpecFull`]'s serialized
/// form rather than a hand-picked field list, so a field added to
/// `RecipeSpec` later appears here without anyone remembering to add it.
pub(crate) fn recipe_field_diffs(
    before: &RecipeSpec,
    after: &RecipeSpec,
) -> Vec<(String, String, String)> {
    let before_value =
        serde_json::to_value(RecipeSpecFull::from(before)).expect("RecipeSpecFull serializes");
    let after_value =
        serde_json::to_value(RecipeSpecFull::from(after)).expect("RecipeSpecFull serializes");
    let (serde_json::Value::Object(before_map), serde_json::Value::Object(after_map)) =
        (&before_value, &after_value)
    else {
        unreachable!("RecipeSpec always serializes to a JSON object")
    };
    let mut keys: std::collections::BTreeSet<&String> = before_map.keys().collect();
    keys.extend(after_map.keys());
    let mut diffs = Vec::new();
    for key in keys {
        let a = before_map
            .get(key)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let b = after_map
            .get(key)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if a != b {
            diffs.push((key.clone(), a.to_string(), b.to_string()));
        }
    }
    diffs
}

/// Compare a candidate `relais.toml`'s recipes against the repository's
/// own, per recipe and revision, naming every field that differs; then
/// state whether `route::validate_candidate` would admit the candidate
/// (SPEC §26). Read-only throughout: nothing here grants, replays,
/// evaluates or writes anything.
pub(crate) fn recipe_diff_command(candidate_path: &Path) -> Result<CliOutcome, CliError> {
    let (_root, incumbent) = load_repo_policy()?;
    let candidate_text =
        std::fs::read_to_string(candidate_path).map_err(|cause| CliError::Read {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause,
        })?;
    let candidate_policy =
        RepoPolicy::from_toml_str(&candidate_text).map_err(|cause| CliError::Invalid {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause: Box::new(cause),
        })?;

    let incumbent_by_key: std::collections::BTreeMap<(String, u32), &RecipeSpec> = incumbent
        .recipes
        .iter()
        .map(|r| ((r.name.clone(), r.revision), r))
        .collect();
    let candidate_by_key: std::collections::BTreeMap<(String, u32), &RecipeSpec> = candidate_policy
        .recipes
        .iter()
        .map(|r| ((r.name.clone(), r.revision), r))
        .collect();
    let mut keys: std::collections::BTreeSet<(String, u32)> =
        incumbent_by_key.keys().cloned().collect();
    keys.extend(candidate_by_key.keys().cloned());

    let mut any_difference = false;
    for key @ (name, revision) in &keys {
        match (incumbent_by_key.get(key), candidate_by_key.get(key)) {
            (Some(before), Some(after)) => {
                let diffs = recipe_field_diffs(before, after);
                if diffs.is_empty() {
                    continue;
                }
                any_difference = true;
                println!("{name} revision {revision}:");
                for (field, before_json, after_json) in diffs {
                    println!("  {field}: {before_json} -> {after_json}");
                }
            }
            (None, Some(_)) => {
                any_difference = true;
                println!("{name} revision {revision}: only in the candidate");
            }
            (Some(_), None) => {
                any_difference = true;
                println!("{name} revision {revision}: only in the repository");
            }
            (None, None) => unreachable!("every key comes from one side or the other"),
        }
    }
    if !any_difference {
        println!("no differences between the repository's recipes and the candidate's");
    }

    let bounds = tuning_bounds(&incumbent);
    match route::validate_candidate(
        &incumbent,
        &candidate_policy,
        &bounds,
        route::CandidateAdmission::Promote,
    ) {
        Ok(_) => println!(
            "admissible: route::validate_candidate would ADMIT this candidate — admissible is \
             not approved: it runs only under the repository's grant, or a legacy grant for \
             its exact hash, and nothing here grants, \
             replays, evaluates or writes anything"
        ),
        Err(rejection) => println!(
            "not admissible: route::validate_candidate would REJECT this candidate: \
             {rejection} — admissible is not approved either way: nothing here grants, \
             replays, evaluates or writes anything"
        ),
    }
    Ok(CliOutcome::Accepted)
}

/// Read the settled trials for a candidate recipe and print a paired
/// comparison against the incumbent (SPEC §25). This command reads and
/// reports; it never promotes, writes a policy, or issues a grant —
/// `relais promote` is the only thing in this crate that activates
/// anything, and it takes a learned artifact id, not a recipe.
pub(crate) fn recipe_evaluate_command(candidate_path: &Path) -> Result<CliOutcome, CliError> {
    let (_root, incumbent) = load_repo_policy()?;
    let candidate_text =
        std::fs::read_to_string(candidate_path).map_err(|cause| CliError::Read {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause,
        })?;
    let candidate_policy =
        RepoPolicy::from_toml_str(&candidate_text).map_err(|cause| CliError::Invalid {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause: Box::new(cause),
        })?;
    // Admitted through the same door a learner's proposal, or a replay's
    // candidate, is — never a second, looser one for evaluation.
    let bounds = tuning_bounds(&incumbent);
    let ledger = open_ledger()?;
    let gate = operational(
        typed_gate::assess(&ledger, &incumbent, &candidate_policy),
        "recipe evaluate",
    )?;
    let candidate = match route::validate_candidate(
        &incumbent,
        &candidate_policy,
        &bounds,
        route::CandidateAdmission::PromoteGated(&gate.evidence),
    ) {
        Ok(candidate) => candidate,
        Err(rejection) => {
            eprintln!("relais recipe evaluate: candidate refused: {rejection}");
            return Ok(CliOutcome::Blocked);
        }
    };

    let report = operational(
        evaluate_candidate(
            &ledger,
            &incumbent,
            candidate.policy(),
            MIN_PAIRED_TASKS,
            DEFAULT_BOOTSTRAP_RESAMPLES,
        ),
        "recipe evaluate",
    )?;
    print!("{}", report.render());
    Ok(CliOutcome::Accepted)
}

/// What `relais plan` prints for a repository whose policy has no trust
/// grant yet: the block a person reviews and pastes into machine.toml.
/// `recipe promote` and `recipe rollback` print the same block for the
/// policy they would produce, so it is one text, not two.
pub(crate) fn missing_grant_text(
    grant_key: &str,
    repo_identity: &RepoIdentity,
) -> Result<String, CliError> {
    Ok(format!(
        "trust grant: MISSING for {grant_key}. Review the commands with `relais trust show`, then \n\
         run `relais trust grant --reviewed-by <your name>` once to trust this repository. The \n\
         deprecated legacy grant for this exact policy is `relais trust grant --key {grant_key} \n\
         --reviewed-by <your name>`, or paste this into {}:\n\n\
         [trust.\"{grant_key}\"]\n\
         granted_at = \"{}\"\n\
         reviewed_by = \"<your name>\"\n\
         repo = \"{}\"\n\
         note = \"<what you reviewed>\"\n",
        paths::machine_settings_path()
            .map_err(CliError::Home)?
            .display(),
        chrono::Utc::now().format("%Y-%m-%d"),
        repo_identity.label(),
    ))
}

/// Whether an amendment is shown or appended. A parameter of its own
/// rather than a `bool`, so a call site says which it means.
#[derive(Clone, Copy)]
pub(crate) enum AmendMode {
    Print,
    Write,
}

impl AmendMode {
    pub(crate) fn from_flag(write: bool) -> Self {
        if write {
            Self::Write
        } else {
            Self::Print
        }
    }
}

/// Show an amendment: the fragment(s) it appends, then the trust block
/// for the policy that would result; with [`AmendMode::Write`], append
/// the fragment(s) to relais.toml as text. Never rewrites the file,
/// never touches machine.toml, never issues a grant (SPEC §27).
pub(crate) fn present_amendment(
    root: &Path,
    incumbent: &RepoPolicy,
    amendment: &Amendment,
    mode: AmendMode,
) -> Result<CliOutcome, CliError> {
    let path = root.join("relais.toml");
    let subject = path.display().to_string();
    let fragment = amendment.fragment(&subject).map_err(CliError::Amend)?;
    println!("recipe revision(s) to append to {subject}:\n\n{fragment}");
    let resulting = amendment.resulting_policy(incumbent);
    let authority_hash = resulting.authority_hash();
    let repo_identity = relais::repo::identity(root);
    println!(
        "policy hash after this change: {authority_hash}\n{}",
        missing_grant_text(&grant_key(&authority_hash, &repo_identity), &repo_identity)?
    );
    match mode {
        AmendMode::Print => {
            println!("nothing was written; re-run with --write to append the fragment");
        }
        AmendMode::Write => {
            let existing = std::fs::read_to_string(&path).map_err(|cause| CliError::Read {
                what: "the repository policy",
                path: path.clone(),
                cause,
            })?;
            let appendix = amendment
                .appendix(&existing, &subject)
                .map_err(CliError::Amend)?;
            let append_failed = |cause| CliError::AppendPolicy {
                operation: amendment.operation(),
                path: path.clone(),
                cause,
            };
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .map_err(append_failed)?;
            std::io::Write::write_all(&mut file, appendix.as_bytes()).map_err(append_failed)?;
            println!(
                "appended to {subject}; no trust grant was issued, so the next `relais plan` \
                 reports missing_trust_grant until the block above is pasted into machine.toml"
            );
        }
    }
    Ok(CliOutcome::Accepted)
}

/// Add a candidate's new recipe revision(s) to the repository's policy,
/// only when the recomputed comparison clears every gate (SPEC §27).
pub(crate) fn recipe_promote_command(
    candidate_path: &Path,
    mode: AmendMode,
) -> Result<CliOutcome, CliError> {
    let (root, incumbent) = load_repo_policy()?;
    let candidate_text =
        std::fs::read_to_string(candidate_path).map_err(|cause| CliError::Read {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause,
        })?;
    let candidate_policy =
        RepoPolicy::from_toml_str(&candidate_text).map_err(|cause| CliError::Invalid {
            what: "the candidate recipe",
            path: candidate_path.to_path_buf(),
            cause: Box::new(cause),
        })?;
    let bounds = tuning_bounds(&incumbent);
    // Recomputed here, from the ledger's settled trials — never read from
    // a verdict an earlier `recipe evaluate` printed. The typed-research
    // gate comes first: a research-tier change is admitted only on its
    // count, and the generic gates below cannot stand in for it.
    let ledger = open_ledger()?;
    let gate = operational(
        typed_gate::assess(&ledger, &incumbent, &candidate_policy),
        "recipe promote",
    )?;
    let candidate = match route::validate_candidate(
        &incumbent,
        &candidate_policy,
        &bounds,
        route::CandidateAdmission::PromoteGated(&gate.evidence),
    ) {
        Ok(candidate) => candidate,
        Err(rejection) => {
            eprintln!("relais recipe promote: candidate refused: {rejection}");
            return Ok(CliOutcome::Blocked);
        }
    };
    let report = operational(
        evaluate_candidate(
            &ledger,
            &incumbent,
            candidate.policy(),
            MIN_PAIRED_TASKS,
            DEFAULT_BOOTSTRAP_RESAMPLES,
        ),
        "recipe promote",
    )?;
    let subject = candidate_path.display().to_string();
    let proof = match admit(&report, &subject) {
        Ok(proof) => proof,
        Err(refused) => {
            eprintln!("{refused}");
            eprint!("{}", report.render_evidence());
            eprintln!("nothing was written");
            return Ok(CliOutcome::PromotionRefused);
        }
    };
    let amendment = Amendment::for_promotion(
        proof,
        &gate.evidence,
        &incumbent,
        candidate.policy(),
        &subject,
    )
    .map_err(CliError::Amend)?;
    println!("evaluation this rests on:");
    print!("{}", report.render_evidence());
    println!();
    let outcome = present_amendment(&root, &incumbent, &amendment, mode)?;
    match mode {
        AmendMode::Print => {}
        AmendMode::Write => {
            operational(typed_gate::record(&ledger, &gate), "recipe promote")?;
            for authorisation in &gate.authorisations {
                println!(
                    "recorded in the ledger: {} revision {} ({}) was authorised by {} trials",
                    authorisation.name,
                    authorisation.revision,
                    authorisation.task_type.as_str(),
                    authorisation.trial_ids.len()
                );
            }
        }
    }
    Ok(outcome)
}

/// Restore the revision below the effective one by appending a new
/// revision that repeats it (SPEC §27). Removing a change needs no
/// evaluation.
pub(crate) fn recipe_rollback_command(name: &str, mode: AmendMode) -> Result<CliOutcome, CliError> {
    let (root, incumbent) = load_repo_policy()?;
    let amendment = Amendment::for_rollback(&incumbent, name).map_err(CliError::Amend)?;
    present_amendment(&root, &incumbent, &amendment, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_recipe_spec() -> RecipeSpec {
        RecipeSpec {
            name: "probe".to_string(),
            kind: None,
            scope_within: Vec::new(),
            tier: relais::policy::Tier::Implementation,
            revision: 0,
            enabled: true,
            models: None,
            execution: None,
            context: None,
            review: None,
            task_type: Vec::new(),
            requires_design: false,
        }
    }

    /// Changes exactly one field of `RecipeSpec` at a time, on the same
    /// `(name, revision)`, and asserts `recipe_field_diffs` names that
    /// field and nothing else. The destructure (no `..`) fails to compile
    /// when `RecipeSpec` gains a field; the key-set assertion then fails
    /// the test until that field also has a case, because a `new: _`
    /// binding alone would otherwise let it pass uncovered.
    ///
    /// FALSIFIED (key set): the `review` case was removed and the key-set
    /// assertion turned red before it was restored.
    ///
    /// FALSIFIED: `recipe_field_diffs` was changed to build its diff set
    /// from a struct literal that left `review` out of `RecipeSpecFull`,
    /// and the `review` case below turned red (`diffs.is_empty()`) before
    /// the omission was restored.
    #[test]
    fn recipe_field_diffs_names_exactly_the_changed_field() {
        let base = probe_recipe_spec();
        let RecipeSpec {
            name: _,
            kind: base_kind,
            scope_within: base_scope_within,
            tier: base_tier,
            revision: _,
            enabled: base_enabled,
            models: base_models,
            execution: base_execution,
            context: base_context,
            review: base_review,
            task_type: base_task_type,
            requires_design: base_requires_design,
        } = base.clone();

        let cases: Vec<(&str, RecipeSpec)> = vec![
            (
                "kind",
                RecipeSpec {
                    kind: Some(relais::contract::Kind::Change),
                    ..base.clone()
                },
            ),
            (
                "scope_within",
                RecipeSpec {
                    scope_within: vec!["src/**".to_string()],
                    ..base.clone()
                },
            ),
            (
                "tier",
                RecipeSpec {
                    tier: relais::policy::Tier::Escalation,
                    ..base.clone()
                },
            ),
            (
                "enabled",
                RecipeSpec {
                    enabled: !base_enabled,
                    ..base.clone()
                },
            ),
            (
                "models",
                RecipeSpec {
                    models: Some(std::collections::BTreeMap::from([(
                        relais::policy::Tier::Implementation,
                        relais::policy::ModelProfile {
                            id: "sonnet".to_string(),
                            effort: None,
                            max_effort: None,
                        },
                    )])),
                    ..base.clone()
                },
            ),
            (
                "execution",
                RecipeSpec {
                    execution: Some(relais::policy::ExecutionPolicy::default()),
                    ..base.clone()
                },
            ),
            (
                "context",
                RecipeSpec {
                    context: Some(relais::policy::ContextPolicy::default()),
                    ..base.clone()
                },
            ),
            (
                "review",
                RecipeSpec {
                    review: Some(relais::contract::Review::Required),
                    ..base.clone()
                },
            ),
            (
                "task_type",
                RecipeSpec {
                    task_type: vec![relais::contract::TaskType::Feature],
                    ..base.clone()
                },
            ),
            (
                "requires_design",
                RecipeSpec {
                    requires_design: true,
                    ..base.clone()
                },
            ),
        ];
        // The destructure above stands for the whole field list; assert
        // against it so the base used in every case really is the base.
        assert_eq!(base_kind, None);
        assert_eq!(base_scope_within, Vec::<String>::new());
        assert_eq!(base_tier, relais::policy::Tier::Implementation);
        assert!(base_enabled);
        assert_eq!(base_models, None);
        assert_eq!(base_execution, None);
        assert_eq!(base_context, None);
        assert_eq!(base_review, None);
        assert!(base_task_type.is_empty());
        assert!(!base_requires_design);

        // The case list must name every field the diff compares. The
        // destructure above only forces a binding for a new field, not a
        // case, so tie the two together through the diff's own key set.
        let compared: std::collections::BTreeSet<String> =
            match serde_json::to_value(RecipeSpecFull::from(&base)) {
                Ok(serde_json::Value::Object(map)) => map
                    .keys()
                    .filter(|key| !matches!(key.as_str(), "name" | "revision"))
                    .cloned()
                    .collect(),
                other => panic!("RecipeSpecFull must serialize to an object, got {other:?}"),
            };
        let covered: std::collections::BTreeSet<String> = cases
            .iter()
            .map(|(field, _)| (*field).to_string())
            .collect();
        assert_eq!(
            covered, compared,
            "every field recipe_field_diffs compares needs a case here"
        );

        for (field, changed) in cases {
            assert_ne!(changed, base, "{field} case must actually change the spec");
            let diffs = recipe_field_diffs(&base, &changed);
            assert_eq!(
                diffs.len(),
                1,
                "{field}: expected exactly one changed field, got {diffs:?}"
            );
            assert_eq!(diffs[0].0, field, "{field}: {diffs:?}");
        }
    }

    /// `enabled`/`revision` skip serialization when they hold their
    /// default, so a naive diff over the two RAW serialized forms reads a
    /// defaulted field on one side as JSON `null` — never the value it
    /// actually resolves to. `recipe_field_diffs` must compare EFFECTIVE
    /// values instead: revision 1 `enabled = false` against a candidate
    /// that omits `enabled` (so it defaults to `true`) prints
    /// `enabled: false -> true`, never `-> null`.
    #[test]
    fn recipe_field_diffs_compares_effective_values_not_omitted_defaults() {
        let before = RecipeSpec {
            enabled: false,
            ..probe_recipe_spec()
        };
        let after = probe_recipe_spec();
        assert!(after.enabled, "the candidate leaves enabled at its default");

        let diffs = recipe_field_diffs(&before, &after);
        assert_eq!(diffs.len(), 1, "{diffs:?}");
        assert_eq!(diffs[0].0, "enabled");
        assert_eq!(diffs[0].1, "false");
        assert_eq!(
            diffs[0].2, "true",
            "the omitted default must compare as its effective value, never null: {diffs:?}"
        );
    }
}
