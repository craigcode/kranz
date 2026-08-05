//! Gate score distribution flags (ticket
//! `.kranz/tickets/gate-score-distribution-flags.md`, KRZ-316 — the
//! scored-gates addendum's second half): per gate, fold the score
//! distribution across missions and flag two smells — scores that never
//! approach the threshold over a meaningful sample, and near-constant scores
//! — surfaced on the outcomes report beside the rubber-stamp signal and
//! carried into the escalation ledger fold as a summary field.
//!
//! WHY this is the rubber-stamp flag's COMPLEMENT, not an alternative
//! (presented together, always): block-to-grant timing (KRZ-323) catches an
//! inattentive HUMAN — grants approved faster than anyone could have read
//! them. These flags catch a mis-specified GATE — one that passes everything
//! because its threshold is meaningless. A gate whose scores cluster far
//! from their threshold and never approach it is either genuinely safe or
//! measuring nothing, and the verdict alone cannot distinguish the two;
//! neither can the flag — it ROUTES the distribution to a human, exactly
//! like the rubber-stamp share does. A flag is never an enforcement and
//! never re-derives a verdict ([`crate::gate`]: the stated verdict is
//! authoritative; there is no path from score to verdict, so there is none
//! from distribution to verdict either).
//!
//! The two rules, with their exact constants (each gate is assessed only at
//! or above [`MIN_SAMPLE_COUNT`] scored evaluations):
//!
//! - **`never-approaches-threshold`** — the closest any score came to its
//!   threshold (min |score − threshold| over the sample) is STRICTLY beyond
//!   [`NEVER_APPROACHES_MARGIN`]. At the margin exactly is within reach and
//!   does not flag. The margin is calibrated to the conventional 0.0..=1.0
//!   score scale ([`crate::gate::GateScore`]): 0.1 is one tenth of it. A
//!   gate defining a different scale gets the same constant — documented,
//!   and acceptable for a smell: the flag says "nothing ever came near the
//!   line", which only reads falsely if the gate's scale dwarfs its
//!   threshold spacing, and the carried distribution lets the reader judge.
//! - **`near-constant`** — the population variance (÷n) of the scores is
//!   STRICTLY below [`NEAR_CONSTANT_VARIANCE_EPSILON`]: a standard deviation
//!   under 0.001 on the conventional scale. At or above the epsilon does not
//!   flag. The fold cannot see a gate's INPUT, so the rule asserts only
//!   "the scores do not move" — whether the input ever varied is the
//!   investigation the flag routes, not a claim the fold makes.
//!
//! WHY the minimum sample exists: a handful of evaluations can sit far from
//! a threshold or agree with each other by chance; ten cannot plausibly do
//! either without saying something about the gate. Below the minimum the
//! fold renders the gate's assessment ABSENT — no flags and no zero-filled
//! distribution (the house no-fabricated-numbers rule): an honest "not
//! enough evidence", never a fabricated clean bill.
//!
//! WHY distance is per-point: the (score, threshold) pair travels together
//! from each `gate.result` event (events.rs), and a gate MAY state a
//! different threshold per evaluation — so `score - threshold` is computed
//! per point, never against one assumed constant threshold. WHY the two
//! surfaces pool: the same gate id is evaluated at approval and at the
//! final gate (gate.rs); folding both into one series treats the GATE's
//! scoring behavior as the unit, and a gate that scores differently per
//! surface shows up as variance — biasing AWAY from `near-constant`, the
//! conservative direction.
//!
//! Gates that emit no score are excluded entirely: a boolean-only gate
//! produces no sample, so it can never reach the distribution or the flags
//! (KRZ-315's absence-is-the-normal-case rule carried through: a gate that
//! says nothing about confidence says NOTHING, and the flags never invent a
//! reading for it).
//!
//! Pure-fold idiom, mirroring [`crate::gate_scores`] (whose per-event
//! extraction this shares): [`score_distribution_report`] is a pure function
//! over the collected samples, and the samples are folded from the same
//! event logs by the caller — the outcomes fold collects them per mission
//! through its memoized per-mission pass ([`crate::outcomes`]), so the same
//! logs always yield an identical report. No persisted state, no clock, no
//! reads outside the logs.

use crate::events::Event;
use crate::events::EventKind;
use serde::{Deserialize, Serialize};

/// Minimum scored evaluations before either rule assesses a gate: 10. WHY
/// ten: below it, "all far from the threshold" or "all alike" is still a
/// plausible accident of a young series; at it, the distribution itself is
/// the evidence. Boundary-tested: exactly 10 assesses, 9 does not.
pub const MIN_SAMPLE_COUNT: u64 = 10;

/// The `never-approaches-threshold` margin: 0.1 — one tenth of the
/// conventional 0.0..=1.0 score scale (see the module docs). The closest
/// approach must be STRICTLY beyond this to flag; exactly at it is within
/// reach.
pub const NEVER_APPROACHES_MARGIN: f64 = 0.1;

/// The `near-constant` variance epsilon: 1e-6 on the population variance —
/// a standard deviation under 0.001 on the conventional scale (see the
/// module docs). STRICTLY below flags; at or above does not.
pub const NEAR_CONSTANT_VARIANCE_EPSILON: f64 = 1e-6;

/// One scored evaluation of one gate: the (score, threshold) pair a
/// `gate.result` event stated, verbatim (kranz records, never normalizes —
/// the [`crate::gate_scores`] contract), keyed by the gate identity. The
/// distribution fold's atom; the replay identity (mission/seq/ts/surface/
/// verdict) the series carries is not read here, so it is not carried.
/// Fold-internal — the report structs are the wire surface.
#[derive(Debug, Clone, PartialEq)]
pub struct GateScoreSample {
    pub gate: String,
    pub score: f64,
    pub threshold: f64,
}

/// The smell a [`GateScoreFlag`] names. Serde kebab-case (the GateKind
/// idiom) so the wire form IS the ticket's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateScoreFlagKind {
    /// Over a meaningful sample, no score ever came within
    /// [`NEVER_APPROACHES_MARGIN`] of its threshold — the threshold is
    /// never exercised, so it cannot distinguish anything.
    NeverApproachesThreshold,
    /// Over a meaningful sample, the scores do not move (variance below
    /// [`NEAR_CONSTANT_VARIANCE_EPSILON`]) — the score carries no
    /// information about whatever the gate evaluated.
    NearConstant,
}

impl GateScoreFlagKind {
    /// The wire/serde form (`never-approaches-threshold`/`near-constant`)
    /// for text surfaces (the EscalationKind::as_str idiom).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NeverApproachesThreshold => "never-approaches-threshold",
            Self::NearConstant => "near-constant",
        }
    }
}

/// The score distribution of one gate over its scored evaluations: the
/// summary a flag carries so the reader can judge the smell without
/// re-walking the series. All f64 fields are computed over exactly
/// `samples` points — there is no empty distribution (a gate reaches here
/// only at or above [`MIN_SAMPLE_COUNT`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreDistribution {
    /// Scored evaluations folded (always >= [`MIN_SAMPLE_COUNT`]).
    pub samples: u64,
    pub min_score: f64,
    pub max_score: f64,
    pub mean_score: f64,
    /// Population variance (÷n) of the scores — the `near-constant` input.
    pub variance: f64,
    /// Endpoints of the signed distance-to-threshold distribution
    /// (score − threshold, per point — thresholds may vary per evaluation).
    pub min_distance: f64,
    pub max_distance: f64,
    /// min |score − threshold| over the sample — how close ANY evaluation
    /// ever came to its threshold; the `never-approaches-threshold` input.
    pub closest_approach: f64,
}

impl ScoreDistribution {
    /// Fold one gate's samples into its distribution. Order-independent in
    /// value up to float summation order; the caller's sample order is fixed
    /// by the logs, so the report is a pure function of the logs.
    fn from_samples(samples: &[&GateScoreSample]) -> Self {
        let n = samples.len() as f64;
        let mut min_score = f64::INFINITY;
        let mut max_score = f64::NEG_INFINITY;
        let mut sum = 0.0;
        let mut min_distance = f64::INFINITY;
        let mut max_distance = f64::NEG_INFINITY;
        let mut closest_approach = f64::INFINITY;
        for sample in samples {
            min_score = min_score.min(sample.score);
            max_score = max_score.max(sample.score);
            sum += sample.score;
            let distance = sample.score - sample.threshold;
            min_distance = min_distance.min(distance);
            max_distance = max_distance.max(distance);
            closest_approach = closest_approach.min(distance.abs());
        }
        let mean_score = sum / n;
        let variance = samples
            .iter()
            .map(|s| (s.score - mean_score).powi(2))
            .sum::<f64>()
            / n;
        Self {
            samples: samples.len() as u64,
            min_score,
            max_score,
            mean_score,
            variance,
            min_distance,
            max_distance,
            closest_approach,
        }
    }
}

/// One flag against one gate: the gate identity, the smell kind, and the
/// distribution summary that triggered it (the ticket's contract — a flag
/// names the gate and carries its evidence).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateScoreFlag {
    pub gate: String,
    pub kind: GateScoreFlagKind,
    pub distribution: ScoreDistribution,
}

/// The gate score distribution flag report (KRZ-316), folded across all
/// missions and surfaced beside the rubber-stamp signal on the outcomes
/// report. The rule constants ride along in effect (the RubberStampReport
/// idiom: the wire self-describes what judged it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GateScoreFlagsReport {
    /// The minimum sample in effect ([`MIN_SAMPLE_COUNT`]).
    pub min_samples: u64,
    /// The margin in effect ([`NEVER_APPROACHES_MARGIN`]).
    pub never_approaches_margin: f64,
    /// The variance epsilon in effect ([`NEAR_CONSTANT_VARIANCE_EPSILON`]).
    pub near_constant_variance_epsilon: f64,
    /// Gates with at least one scored evaluation (the population the
    /// assessment draws from — includes gates below the minimum sample).
    pub scored_gates: u64,
    /// Gates at or above the minimum sample — the ones either rule could
    /// flag. Below the minimum a gate is unassessed, never flagged.
    pub assessed_gates: u64,
    /// Every flag that fired, grouped by gate (a gate's kinds adjacent —
    /// `never-approaches-threshold` before `near-constant`), gates ordered
    /// by identity (BTreeMap: the report's order is a function of the log,
    /// never of hash iteration). Empty when nothing smells — never a
    /// zero-filled row for an unassessed or unscored gate.
    pub flags: Vec<GateScoreFlag>,
}

impl Default for GateScoreFlagsReport {
    /// The serde-backfill / empty-history default carries the DOCUMENTED
    /// rule constants, never zeros that would misstate what judged the
    /// report (the RubberStampReport::default discipline).
    fn default() -> Self {
        Self {
            min_samples: MIN_SAMPLE_COUNT,
            never_approaches_margin: NEVER_APPROACHES_MARGIN,
            near_constant_variance_epsilon: NEAR_CONSTANT_VARIANCE_EPSILON,
            scored_gates: 0,
            assessed_gates: 0,
            flags: Vec::new(),
        }
    }
}

/// The `never-approaches-threshold` rule as one named predicate, so the
/// strictness lives in exactly one place: STRICTLY beyond the margin flags;
/// at the margin is within reach.
fn never_approaches(closest_approach: f64) -> bool {
    closest_approach > NEVER_APPROACHES_MARGIN
}

/// The `near-constant` rule as one named predicate: STRICTLY below the
/// epsilon flags; at or above it does not.
fn near_constant(variance: f64) -> bool {
    variance < NEAR_CONSTANT_VARIANCE_EPSILON
}

/// Collect the scored samples from one mission's already-filtered event
/// slice (the outcomes fold hands in its per-mission `mission_events`, whose
/// same-mission and seq-order invariants match the
/// [`crate::gate_scores::gate_score_series`] discipline). Only `gate.result`
/// events carrying the score pair contribute — a boolean-only gate's events
/// yield NO sample (absence is the normal case, never a zero), so an
/// unscored gate can never reach the distribution or its flags.
pub fn collect_scored_samples(mission_events: &[&Event]) -> Vec<GateScoreSample> {
    let mut samples = Vec::new();
    for event in mission_events {
        let EventKind::GateResult {
            gate,
            score: Some(score),
            threshold: Some(threshold),
            ..
        } = &event.kind
        else {
            continue;
        };
        samples.push(GateScoreSample {
            gate: gate.clone(),
            score: *score,
            threshold: *threshold,
        });
    }
    samples
}

/// The pure fold: every scored sample grouped by gate, each sufficiently
/// sampled gate's distribution computed and judged against the two rules.
/// Gates below [`MIN_SAMPLE_COUNT`] count toward `scored_gates` only — no
/// assessment, no flags. The same samples in the same order always yield an
/// identical report (no clock, no hash iteration).
pub fn score_distribution_report(samples: &[GateScoreSample]) -> GateScoreFlagsReport {
    let mut by_gate: std::collections::BTreeMap<&str, Vec<&GateScoreSample>> =
        std::collections::BTreeMap::new();
    for sample in samples {
        by_gate
            .entry(sample.gate.as_str())
            .or_default()
            .push(sample);
    }

    let mut assessed_gates = 0;
    let mut flags = Vec::new();
    for (gate, gate_samples) in &by_gate {
        if (gate_samples.len() as u64) < MIN_SAMPLE_COUNT {
            continue;
        }
        assessed_gates += 1;
        let distribution = ScoreDistribution::from_samples(gate_samples);
        if never_approaches(distribution.closest_approach) {
            flags.push(GateScoreFlag {
                gate: gate.to_string(),
                kind: GateScoreFlagKind::NeverApproachesThreshold,
                distribution: distribution.clone(),
            });
        }
        if near_constant(distribution.variance) {
            flags.push(GateScoreFlag {
                gate: gate.to_string(),
                kind: GateScoreFlagKind::NearConstant,
                distribution,
            });
        }
    }

    GateScoreFlagsReport {
        scored_gates: by_gate.len() as u64,
        assessed_gates,
        flags,
        ..GateScoreFlagsReport::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{GateKind, GateSurface, GateVerdict};
    use chrono::DateTime;

    fn ev(seq: u64, mission_id: &str, ts_ms: i64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: DateTime::from_timestamp_millis(ts_ms).unwrap(),
            mission_id: mission_id.to_string(),
            kind,
        }
    }

    /// A `gate.result` payload; `score` is the (score, threshold) pair a
    /// scored gate reports, `None` for a boolean-only gate (the
    /// gate_scores.rs fixture idiom).
    fn gate_result(gate: &str, verdict: GateVerdict, score: Option<(f64, f64)>) -> EventKind {
        EventKind::GateResult {
            gate: gate.to_string(),
            surface: GateSurface::Approval,
            kind: GateKind::Deterministic,
            index: 0,
            verdict,
            artefact_ref: format!("contract gate {gate}"),
            artefact_detail: None,
            score: score.map(|(score, _)| score),
            threshold: score.map(|(_, threshold)| threshold),
        }
    }

    fn sample(gate: &str, score: f64, threshold: f64) -> GateScoreSample {
        GateScoreSample {
            gate: gate.to_string(),
            score,
            threshold,
        }
    }

    /// N identical samples for one gate — the unmoving-series fixture.
    fn repeated(gate: &str, n: usize, score: f64, threshold: f64) -> Vec<GateScoreSample> {
        (0..n).map(|_| sample(gate, score, threshold)).collect()
    }

    fn kinds_of(report: &GateScoreFlagsReport, gate: &str) -> Vec<GateScoreFlagKind> {
        report
            .flags
            .iter()
            .filter(|f| f.gate == gate)
            .map(|f| f.kind)
            .collect()
    }

    /// The distribution fold itself: min/max/mean score, population
    /// variance, and the per-point signed distance endpoints plus the
    /// closest approach — over values exact in f64 so the assertions are
    /// arithmetic, not approximations. Scores 0.0/0.25/0.5/0.75/1.0 twice
    /// against threshold 0.5: mean 0.5, variance 0.125 (Σ(s−mean)²/n =
    /// 1.25/10), distances -0.5..0.5, closest 0.0 — a series that both
    /// approaches its threshold and moves, so neither flag fires.
    #[test]
    fn score_distribution_flag_stats_fold_min_max_mean_variance_distances() {
        let mut samples = Vec::new();
        for score in [0.0, 0.25, 0.5, 0.75, 1.0] {
            samples.push(sample("vacuous-filter", score, 0.5));
            samples.push(sample("vacuous-filter", score, 0.5));
        }
        let report = score_distribution_report(&samples);
        assert_eq!(report.scored_gates, 1);
        assert_eq!(report.assessed_gates, 1);
        assert!(
            report.flags.is_empty(),
            "this spread approaches the threshold and moves: {report:?}"
        );
        // The distribution the report WOULD carry is observable through a
        // flag; fold the same samples directly for the stats assertions.
        let distribution = ScoreDistribution::from_samples(&samples.iter().collect::<Vec<_>>());
        assert_eq!(distribution.samples, 10);
        assert_eq!(distribution.min_score, 0.0);
        assert_eq!(distribution.max_score, 1.0);
        assert_eq!(distribution.mean_score, 0.5);
        assert_eq!(distribution.variance, 0.125);
        assert_eq!(distribution.min_distance, -0.5);
        assert_eq!(distribution.max_distance, 0.5);
        assert_eq!(distribution.closest_approach, 0.0);
    }

    /// never-approaches-threshold: ten evaluations, none closer than 0.5 to
    /// the threshold — the flag fires, names the gate, and carries the
    /// distribution that triggered it (the ticket's contract).
    #[test]
    fn score_distribution_flag_never_approaches_fires_and_carries_evidence() {
        let report = score_distribution_report(&repeated("vacuous-filter", 10, 0.5, 1.0));
        assert_eq!(
            kinds_of(&report, "vacuous-filter"),
            [
                GateScoreFlagKind::NeverApproachesThreshold,
                GateScoreFlagKind::NearConstant,
            ]
        );
        let flag = &report.flags[0];
        assert_eq!(flag.gate, "vacuous-filter");
        assert_eq!(flag.kind.as_str(), "never-approaches-threshold");
        let d = &flag.distribution;
        assert_eq!(d.samples, 10);
        assert_eq!(d.closest_approach, 0.5);
        assert_eq!(d.min_score, 0.5);
        assert_eq!(d.max_score, 0.5);
        assert_eq!(d.mean_score, 0.5);
        assert_eq!(d.variance, 0.0);
        assert_eq!(d.min_distance, -0.5);
        assert_eq!(d.max_distance, -0.5);
    }

    /// The CLOSEST score governs, not the bulk: nine evaluations far from
    /// the threshold and one within the margin means the threshold WAS
    /// approached — no flag. (0.95 vs 1.0: distance 0.05 < 0.1.)
    #[test]
    fn score_distribution_flag_never_approaches_closest_score_within_margin_clears() {
        let mut samples = repeated("vacuous-filter", 9, 0.5, 1.0);
        samples.push(sample("vacuous-filter", 0.95, 1.0));
        let report = score_distribution_report(&samples);
        assert_eq!(report.assessed_gates, 1);
        assert!(
            !kinds_of(&report, "vacuous-filter")
                .contains(&GateScoreFlagKind::NeverApproachesThreshold),
            "one approach within the margin clears the smell: {report:?}"
        );
        // And the moving scores keep near-constant off too.
        assert!(report.flags.is_empty(), "{report:?}");
    }

    /// The margin is a STRICT boundary (documented): a closest approach
    /// exactly AT the margin is within reach and does not flag; beyond it
    /// does. 0.1 − 0.0 is exactly the margin constant in f64, so the at-case
    /// is exact, not approximate.
    #[test]
    fn score_distribution_flag_never_approaches_margin_boundary_is_strict() {
        // Predicate level: at the margin does not flag, beyond does.
        assert!(!never_approaches(NEVER_APPROACHES_MARGIN));
        assert!(never_approaches(NEVER_APPROACHES_MARGIN * 2.0));

        // Fold level: every score exactly the margin away (threshold 0.0,
        // score 0.1) — no flag; every score 0.2 away — flag.
        let at = score_distribution_report(&repeated("gate-a", 10, 0.1, 0.0));
        assert!(
            !kinds_of(&at, "gate-a").contains(&GateScoreFlagKind::NeverApproachesThreshold),
            "at the margin is within reach: {at:?}"
        );
        let beyond = score_distribution_report(&repeated("gate-a", 10, 0.2, 0.0));
        assert!(
            kinds_of(&beyond, "gate-a").contains(&GateScoreFlagKind::NeverApproachesThreshold),
            "beyond the margin flags: {beyond:?}"
        );
    }

    /// near-constant: ten identical scores — the series does not move at
    /// all, whatever the input was — flag fires with variance 0.0 carried.
    #[test]
    fn score_distribution_flag_near_constant_fires_on_unmoving_scores() {
        let report = score_distribution_report(&repeated("vacuous-filter", 10, 0.75, 1.0));
        assert_eq!(
            kinds_of(&report, "vacuous-filter"),
            [
                GateScoreFlagKind::NeverApproachesThreshold,
                GateScoreFlagKind::NearConstant,
            ]
        );
        let flag = report
            .flags
            .iter()
            .find(|f| f.kind == GateScoreFlagKind::NearConstant)
            .unwrap();
        assert_eq!(flag.gate, "vacuous-filter");
        assert_eq!(flag.kind.as_str(), "near-constant");
        assert_eq!(flag.distribution.variance, 0.0);
        // A gate's two kinds land adjacent, never-approaches first — the
        // documented grouping the text surface relies on.
        assert_eq!(
            report.flags[0].kind,
            GateScoreFlagKind::NeverApproachesThreshold
        );
        assert_eq!(report.flags[1].kind, GateScoreFlagKind::NearConstant);
    }

    /// The variance epsilon is a STRICT boundary (documented): variance
    /// exactly at the epsilon does not flag; below does. Fold level: scores
    /// alternating 0.0/0.5 (variance 0.0625, far above the epsilon) stay
    /// clear of near-constant while still firing never-approaches — proving
    /// the two rules judge independently.
    #[test]
    fn score_distribution_flag_near_constant_epsilon_boundary_is_strict() {
        // Predicate level: at the epsilon does not flag, below does.
        assert!(!near_constant(NEAR_CONSTANT_VARIANCE_EPSILON));
        assert!(near_constant(NEAR_CONSTANT_VARIANCE_EPSILON / 2.0));

        // Fold level: a moving series (0.0/0.5 alternating vs threshold
        // 0.25 — distances ±0.25, variance 0.0625).
        let mut samples = Vec::new();
        for i in 0..10 {
            let score = if i % 2 == 0 { 0.0 } else { 0.5 };
            samples.push(sample("gate-b", score, 0.25));
        }
        let report = score_distribution_report(&samples);
        assert_eq!(
            kinds_of(&report, "gate-b"),
            [GateScoreFlagKind::NeverApproachesThreshold]
        );
    }

    /// The minimum sample gates assessment, boundary-tested: nine identical
    /// far-from-threshold scores fold to an UNASSESSED gate — scored but
    /// with no flags and no distribution — and the tenth identical score
    /// turns the same series into a double flag. Below the minimum the
    /// report is absent, never a zero-filled clean bill.
    #[test]
    fn score_distribution_flag_minimum_sample_boundary_at_and_under() {
        let under = score_distribution_report(&repeated(
            "vacuous-filter",
            (MIN_SAMPLE_COUNT - 1) as usize,
            0.5,
            1.0,
        ));
        assert_eq!(under.scored_gates, 1, "the gate IS counted as scored");
        assert_eq!(under.assessed_gates, 0, "but never assessed");
        assert!(under.flags.is_empty(), "no flags below the minimum");

        let at = score_distribution_report(&repeated(
            "vacuous-filter",
            MIN_SAMPLE_COUNT as usize,
            0.5,
            1.0,
        ));
        assert_eq!(at.assessed_gates, 1);
        assert_eq!(at.flags.len(), 2, "both rules fire at the minimum");
    }

    /// Gates that emit no score are excluded, never flagged: boolean-only
    /// `gate.result` events (and non-gate events) yield no samples, so the
    /// report has no population and no flags — and in a mixed log the
    /// unscored gate appears NOWHERE while the scored one is judged.
    #[test]
    fn score_distribution_flag_unscored_gates_excluded_never_flagged() {
        let events = [
            ev(
                1,
                "m-1",
                1_000,
                gate_result("env-sensitive", GateVerdict::Pass, None),
            ),
            ev(
                2,
                "m-1",
                2_000,
                gate_result("env-sensitive", GateVerdict::Fail, None),
            ),
            ev(3, "m-1", 3_000, EventKind::MissionCompleted {}),
        ];
        let refs: Vec<&Event> = events.iter().collect();
        let samples = collect_scored_samples(&refs);
        assert!(samples.is_empty(), "an unscored gate emits no sample");
        let report = score_distribution_report(&samples);
        assert_eq!(report.scored_gates, 0);
        assert_eq!(report.assessed_gates, 0);
        assert!(report.flags.is_empty());

        // Mixed: ten scored events for one gate beside the unscored one.
        let mut events = Vec::new();
        for i in 0..10 {
            events.push(ev(
                i + 1,
                "m-1",
                1_000 + i as i64,
                gate_result("vacuous-filter", GateVerdict::Pass, Some((0.5, 1.0))),
            ));
        }
        events.push(ev(
            11,
            "m-1",
            2_000,
            gate_result("env-sensitive", GateVerdict::Pass, None),
        ));
        let refs: Vec<&Event> = events.iter().collect();
        let samples = collect_scored_samples(&refs);
        assert_eq!(samples.len(), 10);
        let report = score_distribution_report(&samples);
        assert_eq!(report.scored_gates, 1);
        assert_eq!(report.assessed_gates, 1);
        assert!(
            report.flags.iter().all(|f| f.gate == "vacuous-filter"),
            "the unscored gate is never named: {report:?}"
        );
    }

    /// Pure-fold contract: the same samples fold to a byte-identical report
    /// on repetition (no clock, no hash iteration) — and the wire form
    /// carries the rule constants that judged it.
    #[test]
    fn score_distribution_flag_pure_fold_repeat_is_byte_identical() {
        let mut samples = repeated("vacuous-filter", 10, 0.5, 1.0);
        samples.extend(repeated("other-gate", 10, 0.95, 1.0));
        let first = score_distribution_report(&samples);
        let second = score_distribution_report(&samples);
        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap(),
        );
        assert_eq!(first.min_samples, MIN_SAMPLE_COUNT);
        assert_eq!(first.never_approaches_margin, NEVER_APPROACHES_MARGIN);
        assert_eq!(
            first.near_constant_variance_epsilon,
            NEAR_CONSTANT_VARIANCE_EPSILON
        );
        // Gate order on the wire is the BTreeMap (identity) order.
        let json = serde_json::to_value(&first).unwrap();
        let kinds: Vec<&str> = json["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["kind"].as_str().unwrap())
            .collect();
        assert!(
            kinds.contains(&"never-approaches-threshold"),
            "kebab-case wire kind: {kinds:?}"
        );
    }
}
