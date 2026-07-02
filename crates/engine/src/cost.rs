//! Token pricing and pre-mission cost estimation (plan §5 Phase 1).
//!
//! The estimate produced here is exactly that — an ESTIMATE, presented as a
//! wide range before the mission starts. Live token usage reported by the
//! CLI (and folded into `MissionState.total_cost_usd`) is always
//! authoritative; nothing in this module gates or bills anything.

use crate::types::{MissionConfig, Plan, TokenUsage};

const TOKENS_PER_MTOK: f64 = 1_000_000.0;

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
        Pricing { input_per_mtok: 10.0, output_per_mtok: 50.0 }
    } else if m.contains("opus") {
        Pricing { input_per_mtok: 5.0, output_per_mtok: 25.0 }
    } else if m.contains("sonnet") {
        Pricing { input_per_mtok: 3.0, output_per_mtok: 15.0 }
    } else if m.contains("haiku") {
        Pricing { input_per_mtok: 1.0, output_per_mtok: 5.0 }
    } else {
        Pricing { input_per_mtok: 5.0, output_per_mtok: 25.0 }
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostEstimate {
    /// Expected worker session count (fractional — it is a rate, not a plan).
    pub worker_runs: f64,
    /// Expected validator session count.
    pub validator_runs: f64,
    pub low_usd: f64,
    pub expected_usd: f64,
    pub high_usd: f64,
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
    }
}
