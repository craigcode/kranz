//! Token pricing and pre-mission cost estimation (plan §5 Phase 1).
//!
//! The estimate produced here is exactly that — an ESTIMATE, presented as a
//! wide range before the mission starts. Live token usage reported by the
//! CLI (and folded into `MissionState.total_cost_usd`) is always
//! authoritative; nothing in this module gates or bills anything.

use crate::event_log::EventLog;
use crate::paths::MissionPaths;
use crate::reducer;
use crate::types::{
    AssertionCheck, BackendKind, FeatureOrigin, MissionConfig, MissionState, MissionStatus, Plan,
    PlanFeature, PlanMilestone, Role, TokenUsage, WorkerRun,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

const TOKENS_PER_MTOK: f64 = 1_000_000.0;

/// Default model id for the Codex backend, importable engine-wide.
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5-codex";

/// Whether `model` names a codex-family model (same substring match
/// [`pricing_for_model`] uses to select codex pricing).
pub fn is_codex_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("codex") || m.contains("gpt")
}

/// Default model id for the Droid backend (Fireworks-hosted GLM 5.2),
/// importable engine-wide.
pub const DEFAULT_DROID_MODEL: &str = "accounts/fireworks/models/glm-5p2";

/// Whether `model` names a droid-family (Fireworks GLM) model (same
/// substring match [`pricing_for_model`] uses to select droid pricing).
pub fn is_droid_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("glm") || m.contains("fireworks")
}

/// Default model id for the Kimi backend (Kimi Code CLI's flagship alias),
/// importable engine-wide.
pub const DEFAULT_KIMI_MODEL: &str = "kimi-code/k3";

/// Whether `model` names a kimi-family model (same substring match
/// [`pricing_for_model`] uses to select kimi pricing).
pub fn is_kimi_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("k3") || m.contains("kimi")
}

/// Per-model token pricing in USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl Pricing {
    /// Cache reads are billed at 10% of the input rate.
    pub fn cache_read_per_mtok(&self) -> f64 {
        0.1 * self.input_per_mtok
    }

    /// Cache writes are billed at 125% of the input rate.
    pub fn cache_write_per_mtok(&self) -> f64 {
        1.25 * self.input_per_mtok
    }
}

/// Pricing for a model alias or full id, by case-insensitive substring
/// match on the family name. Unknown models fall back to opus-tier pricing
/// (deliberately conservative for estimates).
pub fn pricing_for_model(model: &str) -> Pricing {
    let m = model.to_ascii_lowercase();
    if m.contains("fable") {
        Pricing {
            input_per_mtok: 10.0,
            output_per_mtok: 50.0,
        }
    } else if m.contains("codex") || m.contains("gpt") {
        Pricing {
            input_per_mtok: 1.25,
            output_per_mtok: 10.0,
        }
    } else if m.contains("glm") || m.contains("fireworks") {
        // TODO(pricing): confirm Fireworks GLM 5.2 $/Mtok before ship
        Pricing {
            input_per_mtok: 0.55,
            output_per_mtok: 2.19,
        }
    } else if m.contains("k3") || m.contains("kimi") {
        // TODO(pricing): confirm kimi $/Mtok before ship
        Pricing {
            input_per_mtok: 0.60,
            output_per_mtok: 2.50,
        }
    } else if m.contains("opus") {
        Pricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
        }
    } else if m.contains("sonnet") {
        Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        }
    } else if m.contains("haiku") {
        Pricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
        }
    } else {
        Pricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
        }
    }
}

/// Dollar cost of a run's token usage under the model's pricing. Used as a
/// fallback when the CLI result message does not report `cost_usd`.
pub fn usage_cost_usd(usage: &TokenUsage, model: &str) -> f64 {
    let p = pricing_for_model(model);
    (usage.input as f64 / TOKENS_PER_MTOK) * p.input_per_mtok
        + (usage.output as f64 / TOKENS_PER_MTOK) * p.output_per_mtok
        + (usage.cache_read as f64 / TOKENS_PER_MTOK) * p.cache_read_per_mtok()
        + (usage.cache_write as f64 / TOKENS_PER_MTOK) * p.cache_write_per_mtok()
}

/// [`usage_cost_usd`] fallback that is aware of the local backend: local
/// model ids are free-form (scoping-doc addendum §5) and cannot be priced by
/// [`pricing_for_model`]'s family match, so a local run always falls back to
/// $0 marginal cost rather than the conservative opus-tier default. Every
/// other backend prices exactly as [`usage_cost_usd`] does.
pub fn usage_cost_usd_for_backend(usage: &TokenUsage, model: &str, backend: BackendKind) -> f64 {
    if backend == BackendKind::Local {
        0.0
    } else {
        usage_cost_usd(usage, model)
    }
}

/// Tunable assumptions behind [`estimate`]. The defaults encode the plan's
/// calibration; callers may override any of them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EstimateParams {
    /// Fraction of runs expected to need a respawn (multiplies run counts).
    pub respawn_allowance: f64,
    /// Expected validation rounds that produce findings, per milestone.
    pub fix_cycles_per_milestone: f64,
    /// Expected fix-features created per fix cycle.
    pub fix_features_per_cycle: f64,
    pub avg_worker_run_usd: f64,
    pub avg_validator_run_usd: f64,
    pub orchestrator_overhead_usd_per_feature: f64,
}

impl Default for EstimateParams {
    fn default() -> Self {
        EstimateParams {
            respawn_allowance: 0.2,
            fix_cycles_per_milestone: 0.5,
            fix_features_per_cycle: 2.0,
            avg_worker_run_usd: 1.50,
            avg_validator_run_usd: 0.75,
            orchestrator_overhead_usd_per_feature: 0.25,
        }
    }
}

/// A pre-mission cost estimate range. `expected_usd` is the central guess;
/// `low_usd`/`high_usd` bound it at 0.5x and 2.5x — real missions vary that
/// much. Live usage remains authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    /// Expected worker session count (fractional — it is a rate, not a plan).
    pub worker_runs: f64,
    /// Expected validator session count.
    pub validator_runs: f64,
    pub low_usd: f64,
    pub expected_usd: f64,
    pub high_usd: f64,
    /// Plan-observable shape. [`estimate`] itself is shape-neutral and always
    /// reports `Unknown` here — only [`apply_shape`] classifies it.
    pub shape: MissionShape,
    /// Whether the calibration corpus covers this shape. [`estimate`] always
    /// reports `High` — only [`apply_shape`] can lower it.
    pub confidence: Confidence,
}

/// How much the calibration corpus backs a [`CostEstimate`]'s range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    /// The calibration corpus has (or is assumed to have) comparable
    /// missions.
    High,
    /// The plan's shape has zero comparable missions in the calibration
    /// corpus — the range has been widened accordingly.
    Low,
}

/// Estimate mission cost from the approved plan (plan §5 Phase 1 formula):
///
/// ```text
/// worker_runs    = features·(1+r) + milestones·x·f·(1+r)
/// validator_runs = 2·milestones·(1+x)          (2 = scrutiny+functional pair)
/// expected       = worker_runs·avg_worker + validator_runs·avg_validator
///                + features·orchestrator_overhead
/// low = 0.5·expected, high = 2.5·expected
/// ```
///
/// where `r` is the respawn allowance, `x` fix cycles per milestone, and `f`
/// fix features per cycle. `skip_scrutiny` / `skip_functional` each remove
/// half of the validator pairs.
pub fn estimate(plan: &Plan, cfg: &MissionConfig, p: &EstimateParams) -> CostEstimate {
    let milestones = plan.milestones.len() as f64;
    let features = plan
        .milestones
        .iter()
        .map(|m| m.features.len())
        .sum::<usize>() as f64;

    let r = p.respawn_allowance;
    let x = p.fix_cycles_per_milestone;
    let f = p.fix_features_per_cycle;

    let worker_runs = features * (1.0 + r) + milestones * x * f * (1.0 + r);

    let validators_per_milestone =
        2.0 - (cfg.skip_scrutiny as u8 as f64) - (cfg.skip_functional as u8 as f64);
    let validator_runs = validators_per_milestone * milestones * (1.0 + x);

    let expected_usd = worker_runs * p.avg_worker_run_usd
        + validator_runs * p.avg_validator_run_usd
        + features * p.orchestrator_overhead_usd_per_feature;

    CostEstimate {
        worker_runs,
        validator_runs,
        low_usd: 0.5 * expected_usd,
        expected_usd,
        high_usd: 2.5 * expected_usd,
        shape: MissionShape::Unknown,
        confidence: Confidence::High,
    }
}

/// Multiplier on one worker-run cost for the cache-miss a tier switch pays:
/// the first post-escalation frontier turn re-reads the whole conversation
/// prefix UNCACHED, and cached-prefix tokens run ~10x cheaper (Cursor's
/// Router post, cursor.com/blog/router). Priced ONCE per escalation — kranz
/// escalates only at feature/milestone edges, where its fresh-context-per-
/// feature design means there is no warm cache left to lose. Kranz-native
/// number, ours to tune; not Factory's and not Cursor's gospel.
pub const CACHE_MISS_MULT: f64 = 9.0;

/// The two-path estimate for a plan routed to the local tier: what it costs
/// if it completes locally (≈$0 marginal) vs if it escalates to frontier
/// (frontier estimate + one cache-miss). Reported as a pair — a single
/// number would be wrong in both directions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwoPathEstimate {
    /// The completes-locally path: $0 marginal (fixed hardware, not per-token).
    pub local_usd: f64,
    /// The escalates-to-frontier path (frontier estimate + one cache-miss).
    pub escalated: CostEstimate,
    /// The priced cache-miss, once per escalation (see [`CACHE_MISS_MULT`]).
    pub cache_miss_usd: f64,
}

/// Two-path estimate from the (shape-adjusted) frontier estimate when `cfg`
/// routes the executor to the local tier; None for frontier-routed plans
/// (their single frontier estimate is honest).
pub fn estimate_two_path(
    frontier: CostEstimate,
    cfg: &MissionConfig,
    p: &EstimateParams,
) -> Option<TwoPathEstimate> {
    if cfg.worker.backend.as_deref() != Some("local") {
        return None;
    }
    // The miss is paid once per escalation, never per turn: the first
    // post-escalation turn re-reads the full prefix uncached; later turns
    // rebuild a warm cache at the new tier.
    let cache_miss_usd = p.avg_worker_run_usd * CACHE_MISS_MULT;
    let mut escalated = frontier;
    escalated.expected_usd += cache_miss_usd;
    escalated.low_usd += cache_miss_usd;
    escalated.high_usd += cache_miss_usd;
    Some(TwoPathEstimate {
        local_usd: 0.0,
        escalated,
        cache_miss_usd,
    })
}

// ---------------------------------------------------------------------------
// Mission shape classification (observable at plan time)
// ---------------------------------------------------------------------------

/// The plan-observable shape of a mission, used to flag validation contracts
/// [`estimate`]'s calibration corpus doesn't cover (later features will widen
/// ranges / lower confidence for these; this type is inert on its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionShape {
    /// Contract gates on a build/test/lint command — the calibration corpus's
    /// typical shape.
    CodeChange,
    /// Contract leans on agent judgement with no build/test gate — expensive
    /// to validate and not represented in the calibration corpus.
    DocHeavy,
    /// Neither signal present (empty or grep-only contract): treated as
    /// neutral, never widened.
    Unknown,
}

/// Classify a plan's shape from its validation contract alone (observable
/// before any run happens).
///
/// `AgentJudgement` assertions are re-evaluated by validators and the
/// orchestrator against the *entire, growing* mission diff on every
/// validation pass — m-d341a7 had 2 judgement assertions over a 1074-line
/// doc and that alone drove 21.5M cache-read tokens across 17 judgement
/// turns. Missions that instead gate on a build/test/lint command bound that
/// cost (the command runs once, deterministically, regardless of diff size),
/// so any `Command` assertion invoking cargo/npm/pytest/go test/make wins
/// over an `AgentJudgement` signal — the code-change shape doesn't scale
/// with diff size the way a judgement-only contract does.
pub fn classify_shape(plan: &Plan) -> MissionShape {
    const BUILD_TEST_TOKENS: &[&str] = &[
        "cargo test",
        "cargo build",
        "cargo check",
        "cargo clippy",
        "npm test",
        "npm run",
        "pytest",
        "go test",
        "make ",
    ];

    let judgements = plan
        .validation_contract
        .iter()
        .filter(|a| a.check == AssertionCheck::AgentJudgement)
        .count();

    let build_test_cmd = plan.validation_contract.iter().any(|a| {
        a.check == AssertionCheck::Command
            && a.command
                .as_deref()
                .map(|c| {
                    let lower = c.to_ascii_lowercase();
                    BUILD_TEST_TOKENS.iter().any(|tok| lower.contains(tok))
                })
                .unwrap_or(false)
    });

    if build_test_cmd {
        MissionShape::CodeChange
    } else if judgements >= 1 {
        MissionShape::DocHeavy
    } else {
        MissionShape::Unknown
    }
}

// ---------------------------------------------------------------------------
// Calibration from recorded actuals (roadmap M1)
// ---------------------------------------------------------------------------

/// [`EstimateParams`] derived from the repo's completed missions, plus how
/// many missions informed them. `missions_used == 0` means the params are the
/// built-in defaults (nothing to calibrate against yet).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    pub params: EstimateParams,
    pub missions_used: usize,
    /// How many of the completed missions folded into `params` classified as
    /// [`MissionShape::DocHeavy`] (from their recorded plan). Zero means the
    /// doc-heavy shape is uncovered by this corpus — see [`apply_shape`].
    pub doc_heavy_missions_used: usize,
    /// Multiplier on the raw [`estimate`] `expected_usd` that recenters it onto
    /// the corpus (aggregate actual ÷ predicted). The calibrated per-run
    /// `params` already carry most of the correction, so this removes only the
    /// residual aggregate bias left by the count formula (≈1.08 on this repo's
    /// corpus). `1.0` below [`MIN_CALIBRATION_MISSIONS`].
    pub expected_mult: f64,
    /// Multipliers on the raw `expected_usd` for the low/high bounds: the
    /// empirical p10 / p90 of actual ÷ predicted, but clamped so the band only
    /// ever WIDENS the built-in `0.5` / `2.5` guess (in-sample quantiles from a
    /// small corpus understate a fresh mission's uncertainty). `0.5` / `2.5`
    /// below [`MIN_CALIBRATION_MISSIONS`].
    pub low_mult: f64,
    pub high_mult: f64,
    /// Completed missions EXCLUDED from the corpus for not being frontier
    /// spend ([`MissionCostClass::Local`] or [`MissionCostClass::Mixed`]) —
    /// their $0-marginal or blended actuals would distort the frontier
    /// calibration. Visibility for the corpus-size readouts (ready.rs shows
    /// `missions_used`).
    pub excluded_non_frontier: usize,
}

/// Minimum completed missions before the corpus fit ([`expected_mult`] etc.)
/// engages. Below this a repo keeps the built-in 1.0 / 0.5 / 2.5 band so cold
/// and small repos behave exactly as before the M1 calibration refit.
///
/// [`expected_mult`]: Calibration::expected_mult
pub const MIN_CALIBRATION_MISSIONS: usize = 5;

/// Whether a completed mission's spend is frontier-priced, local-marginal, or
/// a blend — the calibration corpus is frontier-only, because a $0-marginal
/// local mission next to $30–160 frontier missions would drag the corpus mean
/// toward zero and pollute every future estimate (the
/// local-inference-cost-accounting ticket).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionCostClass {
    /// Every run priced per-token at a frontier backend.
    Frontier,
    /// Routed to the local tier for the entire mission (`worker.backend ==
    /// "local"`, never escalated): ~$0 marginal (fixed hardware +
    /// electricity, not per-token).
    Local,
    /// Started local and escalated to frontier mid-mission: actuals blend
    /// both tiers and are representative of neither corpus.
    Mixed,
}

/// Classify a folded mission. After a `tier.escalated` fold the reducer
/// resets `config.worker.backend` to None, so `still_local` reads true only
/// for never-escalated local missions; the (still_local, escalated) cell is
/// unreachable today and classified Mixed defensively.
pub fn mission_cost_class(state: &MissionState) -> MissionCostClass {
    let still_local = state.config.worker.backend.as_deref() == Some("local");
    let escalated = state.escalated_milestones > 0;
    match (still_local, escalated) {
        (true, false) => MissionCostClass::Local,
        (false, false) => MissionCostClass::Frontier,
        _ => MissionCostClass::Mixed,
    }
}

/// Derive [`EstimateParams`] from the actuals recorded in this repo's
/// COMPLETED missions (live estimates ran ~10x above actuals on the built-in
/// defaults — real per-run costs are the fix).
///
/// Every mission under `.kranz/missions` is folded from its event log;
/// unreadable or corrupt logs are skipped, as is any mission whose final
/// status is not `Complete` (an in-flight or failed mission's actuals are not
/// representative). Each surviving mission yields one set of per-mission
/// actuals (see [`mission_actuals`]); the calibration is their simple mean.
///
/// With zero usable missions the built-in [`EstimateParams::default`] is
/// returned with `missions_used == 0`. Derived costs are floored at $0.01 and
/// rates at 0.0 so one weird mission (e.g. all-zero reported costs) cannot
/// zero out future estimates.
pub fn calibrate(repo_root: &Path) -> Calibration {
    let mut per_mission: Vec<EstimateParams> = Vec::new();
    let mut doc_heavy_missions_used = 0usize;
    let mut excluded_non_frontier = 0usize;
    // (milestones, planned features, config, actual total cost) per completed
    // mission — the inputs needed to re-predict each mission and measure the
    // estimate's bias (see `fit_estimate_to_corpus`).
    let mut corpus: Vec<(usize, usize, MissionConfig, f64)> = Vec::new();
    for mission_id in MissionPaths::list_missions(repo_root) {
        let paths = MissionPaths::new(repo_root, &mission_id);
        let Ok(events) = EventLog::read_events(&paths.events_file()) else {
            continue; // missing or unreadable log: not calibration data
        };
        let Ok(state) = reducer::fold(&events) else {
            continue; // corrupt / empty log: skip, never fail the estimate
        };
        if state.mission.status != MissionStatus::Complete {
            continue;
        }
        // Frontier-only corpus (local-inference-cost-accounting): a
        // $0-marginal local run next to $30–160 frontier missions would drag
        // the corpus mean toward zero; a mixed (escalated) mission is
        // representative of neither tier.
        if mission_cost_class(&state) != MissionCostClass::Frontier {
            excluded_non_frontier += 1;
            continue;
        }
        if classify_shape(&mission_plan(&state)) == MissionShape::DocHeavy {
            doc_heavy_missions_used += 1;
        }
        per_mission.push(mission_actuals(&state));
        let milestones = state.mission.milestones.len();
        let planned_features = state
            .mission
            .milestones
            .iter()
            .flat_map(|m| m.features.iter())
            .filter(|f| f.origin == FeatureOrigin::Plan)
            .count();
        corpus.push((
            milestones,
            planned_features,
            state.config.clone(),
            mission_total_cost(&state),
        ));
    }

    if per_mission.is_empty() {
        return Calibration {
            params: EstimateParams::default(),
            missions_used: 0,
            doc_heavy_missions_used: 0,
            expected_mult: 1.0,
            low_mult: 0.5,
            high_mult: 2.5,
            excluded_non_frontier,
        };
    }

    let n = per_mission.len() as f64;
    let mean = |get: fn(&EstimateParams) -> f64| per_mission.iter().map(get).sum::<f64>() / n;
    let params = EstimateParams {
        respawn_allowance: mean(|p| p.respawn_allowance).max(0.0),
        fix_cycles_per_milestone: mean(|p| p.fix_cycles_per_milestone).max(0.0),
        fix_features_per_cycle: mean(|p| p.fix_features_per_cycle).max(0.0),
        avg_worker_run_usd: mean(|p| p.avg_worker_run_usd).max(0.01),
        avg_validator_run_usd: mean(|p| p.avg_validator_run_usd).max(0.01),
        orchestrator_overhead_usd_per_feature: mean(|p| p.orchestrator_overhead_usd_per_feature)
            .max(0.01),
    };
    let (expected_mult, low_mult, high_mult) = fit_estimate_to_corpus(&params, &corpus);
    Calibration {
        params,
        missions_used: per_mission.len(),
        doc_heavy_missions_used,
        expected_mult,
        low_mult,
        high_mult,
        excluded_non_frontier,
    }
}

/// Total actual cost of a completed mission: the CLI-reported `cost_usd` per
/// run, falling back to [`usage_cost_usd`], summed over every recorded run.
fn mission_total_cost(state: &MissionState) -> f64 {
    state
        .runs
        .values()
        .map(|r| {
            r.cost_usd.unwrap_or_else(|| {
                usage_cost_usd_for_backend(&r.tokens, &r.model, state.config.backend_kind(r.role))
            })
        })
        .sum()
}

/// Fit the count-based [`estimate`] to the corpus of completed missions
/// (roadmap M1). Returns `(expected_mult, low_mult, high_mult)`, each a
/// multiplier on the raw `estimate().expected_usd`:
///
/// - `expected_mult` = aggregate `Σ actual ÷ Σ predicted` — recenters the
///   estimate onto reality, correcting the count formula's structural
///   under-prediction (cost is driven by turns and diff size, not feature
///   count);
/// - `low_mult` / `high_mult` = the p10 / p90 of per-mission `actual ÷
///   predicted` — the range that actually brackets past outcomes.
///
/// Below [`MIN_CALIBRATION_MISSIONS`] usable points the corpus can't be fit, so
/// the built-in `1.0 / 0.5 / 2.5` is returned (an exact no-op in
/// [`apply_shape`]). Values are clamped so one pathological mission can't blow
/// the estimate up or collapse it.
fn fit_estimate_to_corpus(
    params: &EstimateParams,
    corpus: &[(usize, usize, MissionConfig, f64)],
) -> (f64, f64, f64) {
    const DEFAULT: (f64, f64, f64) = (1.0, 0.5, 2.5);
    if corpus.len() < MIN_CALIBRATION_MISSIONS {
        return DEFAULT;
    }
    let mut sum_pred = 0.0;
    let mut sum_actual = 0.0;
    let mut ratios: Vec<f64> = Vec::new();
    for (milestones, features, cfg, actual) in corpus {
        let pred = estimate(&counts_plan(*milestones, *features), cfg, params).expected_usd;
        if pred > 0.0 && *actual > 0.0 {
            sum_pred += pred;
            sum_actual += *actual;
            ratios.push(*actual / pred);
        }
    }
    if ratios.len() < MIN_CALIBRATION_MISSIONS || sum_pred <= 0.0 {
        return DEFAULT;
    }
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let center = (sum_actual / sum_pred).clamp(0.1, 20.0);
    // The band only ever WIDENS the built-in 0.5×/2.5× guess, never narrows it:
    // these are in-sample quantiles from a small corpus, and they understate a
    // fresh mission's true uncertainty — a repo whose past missions happen to
    // cluster tightly must not yield an overconfident range. It also stays
    // ordered around the recentered middle.
    let low = percentile(&ratios, 0.10).min(0.5).min(center).max(0.02);
    let high = percentile(&ratios, 0.90)
        .max(2.5)
        .max(center)
        .min(center.max(1.0) * 8.0);
    (center, low, high)
}

/// A synthetic [`Plan`] carrying only the counts [`estimate`] reads (milestone
/// count and total feature count); every other field is inert. Used to
/// re-predict a completed mission from its recorded shape.
fn counts_plan(milestones: usize, features: usize) -> Plan {
    let milestones = milestones.max(1);
    let mut ms = Vec::with_capacity(milestones);
    for i in 0..milestones {
        let n = if i == 0 { features } else { 0 };
        ms.push(PlanMilestone {
            title: String::new(),
            features: (0..n)
                .map(|_| PlanFeature {
                    title: String::new(),
                    spec: String::new(),
                    validation_criteria: Vec::new(),
                })
                .collect(),
        });
    }
    Plan {
        goal: String::new(),
        validation_contract: Vec::new(),
        milestones: ms,
        considered_alternatives: None,
        command_grants: Vec::new(),
        touch_set: Vec::new(),
    }
}

/// Nearest-rank percentile of an already-sorted slice (`q` in `[0,1]`).
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (q * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Reconstruct the [`Plan`] a completed mission's state was approved from —
/// only the fields [`classify_shape`] reads (validation contract) matter, so
/// the milestone/feature reconstruction just needs consistent counts.
fn mission_plan(state: &MissionState) -> Plan {
    Plan {
        goal: state.mission.goal.clone(),
        considered_alternatives: None,
        command_grants: state.mission.command_grants.clone(),
        touch_set: state.mission.touch_set.clone(),
        validation_contract: state.mission.validation_contract.clone(),
        milestones: state
            .mission
            .milestones
            .iter()
            .map(|m| PlanMilestone {
                title: m.title.clone(),
                features: m
                    .features
                    .iter()
                    .map(|f| PlanFeature {
                        title: f.title.clone(),
                        spec: f.spec.clone(),
                        validation_criteria: f.validation_criteria.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Widen and confidence-label a base [`estimate`] for shapes the calibration
/// corpus doesn't cover.
///
/// [`estimate`] itself stays shape-neutral (existing callers/tests see
/// identical numbers). This is the post-processing step: classify the plan,
/// and only if it is [`MissionShape::DocHeavy`] AND the corpus has zero
/// doc-heavy missions (`cal.doc_heavy_missions_used == 0`) do we act — the
/// corpus's pooled per-run costs are tuned for code-change missions, so a
/// judgement-heavy plan's `expected_usd` is a reasonable central guess (same
/// per-run rates) but its upper bound is not: a comparable mission
/// (m-d341a7) ran to $163.64 against an $18.35 base estimate, ~9x over.
/// `expected_usd` and `low_usd` are left alone (no evidence they're
/// mis-centered); `high_usd` is widened to `expected_usd *
/// LOW_CONFIDENCE_HIGH_MULT` so the range brackets that kind of overrun with
/// margin, and `confidence` drops to `Low` so callers can flag it.
///
/// Every other case (`CodeChange`, `Unknown`, or a `DocHeavy` shape the
/// corpus now has examples of) is a strict no-op beyond stamping `shape` and
/// `confidence: High`.
pub fn apply_shape(base: CostEstimate, plan: &Plan, cal: &Calibration) -> CostEstimate {
    let shape = classify_shape(plan);
    let mut est = base;
    est.shape = shape;

    // Recenter and empirically range the estimate onto the calibration corpus
    // (roadmap M1). `raw` is the count formula's central guess; the corpus fit
    // corrects its systematic under-prediction and replaces the fixed 0.5×/2.5×
    // band with the observed p10/p90. Below MIN_CALIBRATION_MISSIONS the mults
    // are 1.0 / 0.5 / 2.5, so this block is an exact no-op.
    let raw = est.expected_usd;
    est.expected_usd = raw * cal.expected_mult;
    est.low_usd = raw * cal.low_mult;
    est.high_usd = raw * cal.high_mult;

    if shape == MissionShape::DocHeavy && cal.doc_heavy_missions_used == 0 {
        est.confidence = Confidence::Low;
        est.high_usd = est
            .high_usd
            .max(est.expected_usd * LOW_CONFIDENCE_HIGH_MULT);
    } else {
        est.confidence = Confidence::High;
    }
    est
}

/// Multiplier applied to `expected_usd` to get `high_usd` when a plan's shape
/// is uncovered by the calibration corpus (see [`apply_shape`]). Chosen so
/// the widened high comfortably exceeds m-d341a7's recorded actual
/// ($163.64) from its own $18.35 base estimate: 15.0 * 18.35 = $275.25.
const LOW_CONFIDENCE_HIGH_MULT: f64 = 15.0;

/// One completed mission's actuals, expressed in [`EstimateParams`] terms so
/// [`calibrate`] can average them directly:
///
/// - avg worker / validator run cost: mean over runs of that role of the
///   CLI-reported `cost_usd`, falling back to [`usage_cost_usd`];
/// - respawn allowance: total respawns / planned (origin `Plan`) features;
/// - fix cycles per milestone: total fix cycles / milestones;
/// - fix features per cycle: origin-`Fix` features / max(total fix cycles, 1);
/// - orchestrator overhead per feature: total orchestrator run cost / total
///   features.
fn mission_actuals(state: &MissionState) -> EstimateParams {
    let run_cost = |run: &WorkerRun| {
        run.cost_usd.unwrap_or_else(|| {
            usage_cost_usd_for_backend(&run.tokens, &run.model, state.config.backend_kind(run.role))
        })
    };
    let mean_run_cost = |roles: &[Role]| -> f64 {
        let costs: Vec<f64> = state
            .runs
            .values()
            .filter(|r| roles.contains(&r.role))
            .map(run_cost)
            .collect();
        if costs.is_empty() {
            0.0
        } else {
            costs.iter().sum::<f64>() / costs.len() as f64
        }
    };

    let features = || {
        state
            .mission
            .milestones
            .iter()
            .flat_map(|m| m.features.iter())
    };
    let total_features = features().count() as f64;
    let planned_features = features()
        .filter(|f| f.origin == FeatureOrigin::Plan)
        .count() as f64;
    let fix_features = features()
        .filter(|f| f.origin == FeatureOrigin::Fix)
        .count() as f64;
    let total_respawns = features().map(|f| f.respawns as f64).sum::<f64>();
    let milestones = state.mission.milestones.len() as f64;
    let total_fix_cycles = state
        .mission
        .milestones
        .iter()
        .map(|m| m.fix_cycles as f64)
        .sum::<f64>();

    let orchestrator_total = state
        .runs
        .values()
        .filter(|r| r.role == Role::Orchestrator)
        .map(run_cost)
        .sum::<f64>();

    let safe_div = |num: f64, den: f64| if den > 0.0 { num / den } else { 0.0 };

    EstimateParams {
        respawn_allowance: safe_div(total_respawns, planned_features),
        fix_cycles_per_milestone: safe_div(total_fix_cycles, milestones),
        fix_features_per_cycle: fix_features / total_fix_cycles.max(1.0),
        avg_worker_run_usd: mean_run_cost(&[Role::Worker]),
        avg_validator_run_usd: mean_run_cost(&[Role::ValidatorScrutiny, Role::ValidatorFunctional]),
        orchestrator_overhead_usd_per_feature: safe_div(orchestrator_total, total_features),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_pricing_applied() {
        let codex = pricing_for_model(DEFAULT_CODEX_MODEL);
        assert_eq!(codex.input_per_mtok, 1.25);
        assert_eq!(codex.output_per_mtok, 10.0);

        let opus = pricing_for_model("opus");
        assert_ne!(codex, opus);

        let usage = TokenUsage {
            input: 2_000_000,
            output: 1_000_000,
            cache_read: 500_000,
            cache_write: 200_000,
        };
        let expected = 2.0 * 1.25 + 1.0 * 10.0 + 0.5 * (0.1 * 1.25) + 0.2 * (1.25 * 1.25);
        let got = usage_cost_usd(&usage, DEFAULT_CODEX_MODEL);
        assert!(
            (got - expected).abs() < 1e-9,
            "got {got}, expected {expected}"
        );
    }

    #[test]
    fn unknown_model_falls_back_to_opus_tier() {
        let unknown = pricing_for_model("some-unknown-model-xyz");
        let opus = pricing_for_model("opus");
        assert_eq!(unknown, opus);
    }

    #[test]
    fn droid_pricing_applied() {
        let glm = pricing_for_model(DEFAULT_DROID_MODEL);
        assert_eq!(glm.input_per_mtok, 0.55);
        assert_eq!(glm.output_per_mtok, 2.19);

        let opus = pricing_for_model("opus");
        let codex = pricing_for_model(DEFAULT_CODEX_MODEL);
        assert_ne!(glm, opus);
        assert_ne!(glm, codex);

        let usage = TokenUsage {
            input: 2_000_000,
            output: 1_000_000,
            cache_read: 500_000,
            cache_write: 200_000,
        };
        let expected = 2.0 * 0.55 + 1.0 * 2.19 + 0.5 * (0.1 * 0.55) + 0.2 * (1.25 * 0.55);
        let got = usage_cost_usd(&usage, DEFAULT_DROID_MODEL);
        assert!(
            (got - expected).abs() < 1e-9,
            "got {got}, expected {expected}"
        );

        let fable = pricing_for_model("claude-fable-5");
        assert_eq!(fable.input_per_mtok, 10.0);
        assert_eq!(fable.output_per_mtok, 50.0);
    }

    #[test]
    fn kimi_pricing_applied() {
        let kimi = pricing_for_model(DEFAULT_KIMI_MODEL);
        assert_eq!(kimi.input_per_mtok, 0.60);
        assert_eq!(kimi.output_per_mtok, 2.50);

        let opus = pricing_for_model("opus");
        let codex = pricing_for_model(DEFAULT_CODEX_MODEL);
        let droid = pricing_for_model(DEFAULT_DROID_MODEL);
        assert_ne!(kimi, opus);
        assert_ne!(kimi, codex);
        assert_ne!(kimi, droid);

        assert!(is_kimi_model("kimi-code/k3"));
        assert!(is_kimi_model("K3"));
        assert!(!is_kimi_model("opus"));

        let usage = TokenUsage {
            input: 2_000_000,
            output: 1_000_000,
            cache_read: 500_000,
            cache_write: 200_000,
        };
        let expected = 2.0 * 0.60 + 1.0 * 2.50 + 0.5 * (0.1 * 0.60) + 0.2 * (1.25 * 0.60);
        let got = usage_cost_usd(&usage, DEFAULT_KIMI_MODEL);
        assert!(
            (got - expected).abs() < 1e-9,
            "got {got}, expected {expected}"
        );
    }

    #[test]
    fn local_usage_cost_is_always_zero() {
        // Local model ids are free-form (e.g. "my-local-model") and never
        // match a pricing family, so the local-aware fallback must return $0
        // regardless of how much usage was reported.
        let usage = TokenUsage {
            input: 2_000_000,
            output: 1_000_000,
            cache_read: 500_000,
            cache_write: 200_000,
        };
        assert_eq!(
            usage_cost_usd_for_backend(&usage, "my-local-model", BackendKind::Local),
            0.0
        );
        assert_eq!(
            usage_cost_usd_for_backend(&usage, "anything-at-all", BackendKind::Local),
            0.0
        );
    }

    #[test]
    fn local_usage_cost_does_not_change_other_backend_pricing() {
        let usage = TokenUsage {
            input: 2_000_000,
            output: 1_000_000,
            cache_read: 500_000,
            cache_write: 200_000,
        };
        for (backend, model) in [
            (BackendKind::Claude, "sonnet"),
            (BackendKind::Codex, DEFAULT_CODEX_MODEL),
            (BackendKind::Droid, DEFAULT_DROID_MODEL),
            (BackendKind::Kimi, DEFAULT_KIMI_MODEL),
        ] {
            assert_eq!(
                usage_cost_usd_for_backend(&usage, model, backend),
                usage_cost_usd(&usage, model),
                "backend {backend:?} pricing should be unchanged"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Local-inference cost accounting: classification + calibration exclusion
    // -----------------------------------------------------------------------

    use crate::event_log::{EventLog, LockForce};
    use crate::events::{Event, EventKind};
    use crate::paths::MissionPaths;
    use std::time::Duration;

    fn seed_mission(repo_root: &Path, id: &str, kinds: Vec<EventKind>) {
        let paths = MissionPaths::new(repo_root, id);
        let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
        for kind in kinds {
            log.append(kind).unwrap();
        }
    }

    fn created_with(config: MissionConfig) -> EventKind {
        EventKind::MissionCreated {
            goal: "g".into(),
            base_branch: "main".into(),
            mission_branch: "kranz/mission-x".into(),
            config,
        }
    }

    fn local_config() -> MissionConfig {
        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("local".to_string());
        cfg
    }

    fn approved_and_completed() -> Vec<EventKind> {
        vec![
            EventKind::PlanApproved {
                plan: crate::types::Plan {
                    goal: "g".into(),
                    validation_contract: vec![],
                    // One milestone so tier.escalated has a real ms-1 to
                    // reference (the reducer rejects unknown milestones).
                    milestones: vec![crate::types::PlanMilestone {
                        title: "milestone one".into(),
                        features: vec![crate::types::PlanFeature {
                            title: "alpha".into(),
                            spec: "build alpha".into(),
                            validation_criteria: vec![],
                        }],
                    }],
                    considered_alternatives: None,
                    command_grants: vec![],
                    touch_set: vec![],
                },
                base_sha: None,
            },
            EventKind::MissionCompleted {},
        ]
    }

    #[test]
    fn mission_cost_class_maps_local_mixed_and_frontier() {
        let cases = [
            (MissionConfig::default(), false, MissionCostClass::Frontier),
            (local_config(), false, MissionCostClass::Local),
            (local_config(), true, MissionCostClass::Mixed),
        ];
        for (config, escalate, expected) in cases {
            let mut kinds = vec![created_with(config)];
            kinds.extend(approved_and_completed());
            if escalate {
                kinds.insert(
                    kinds.len() - 1,
                    EventKind::TierEscalated {
                        milestone_id: "ms-1".into(),
                        from: crate::types::ExecutorTier::Local,
                        to: crate::types::ExecutorTier::Frontier,
                        reason: "two failed local validations".into(),
                    },
                );
            }
            let events: Vec<Event> = kinds
                .into_iter()
                .enumerate()
                .map(|(i, kind)| Event {
                    seq: (i + 1) as u64,
                    ts: chrono::Utc::now(),
                    mission_id: "m-1".into(),
                    kind,
                })
                .collect();
            let state = crate::reducer::fold(&events).unwrap();
            assert_eq!(mission_cost_class(&state), expected, "escalate={escalate}");
        }
    }

    #[test]
    fn calibrate_excludes_local_and_mixed_from_the_frontier_corpus() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // One frontier mission + one local + one escalated, all Complete.
        let mut frontier = vec![created_with(MissionConfig::default())];
        frontier.extend(approved_and_completed());
        seed_mission(root, "m-frontier", frontier);

        let mut local = vec![created_with(local_config())];
        local.extend(approved_and_completed());
        seed_mission(root, "m-local", local);

        let mut mixed = vec![created_with(local_config())];
        mixed.extend(approved_and_completed());
        // Escalate before completion: started local, ended frontier.
        mixed.insert(
            mixed.len() - 1,
            EventKind::TierEscalated {
                milestone_id: "ms-1".into(),
                from: crate::types::ExecutorTier::Local,
                to: crate::types::ExecutorTier::Frontier,
                reason: "two failed local validations".into(),
            },
        );
        seed_mission(root, "m-mixed", mixed);

        let calibration = calibrate(root);
        // The pin from the ticket: a local (or mixed) run does NOT enter the
        // frontier calibration set.
        assert_eq!(calibration.missions_used, 1, "frontier missions only");
        assert_eq!(calibration.excluded_non_frontier, 2);
    }

    #[test]
    fn two_path_prices_the_miss_once_and_only_for_local_routes() {
        let p = EstimateParams::default();
        let base = CostEstimate {
            worker_runs: 4.0,
            validator_runs: 2.0,
            low_usd: 5.0,
            expected_usd: 10.0,
            high_usd: 25.0,
            shape: MissionShape::Unknown,
            confidence: Confidence::High,
        };

        // Frontier routes keep the single honest estimate.
        assert!(estimate_two_path(base, &MissionConfig::default(), &p).is_none());

        // Local routes get both paths, with the cache-miss priced ONCE —
        // never multiplied by runs or turns.
        let local_cfg = local_config();
        let two = estimate_two_path(base, &local_cfg, &p).unwrap();
        let miss = p.avg_worker_run_usd * CACHE_MISS_MULT;
        assert_eq!(two.local_usd, 0.0, "completes-locally is $0 marginal");
        assert_eq!(two.cache_miss_usd, miss);
        assert_eq!(
            two.escalated.expected_usd,
            10.0 + miss,
            "the miss is priced once per escalation, never per turn"
        );
        assert_eq!(two.escalated.low_usd, 5.0 + miss);
        assert_eq!(two.escalated.high_usd, 25.0 + miss);
    }
}
