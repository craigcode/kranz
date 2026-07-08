# Estimate calibration — refit the pre-mission cost model (roadmap M1)

The pre-mission cost estimate exists and is calibrated from recorded actuals
(`cost::estimate` + `cost::calibrate` + `cost::apply_shape`, wired into every
plan-approval path). The roadmap frames the open work as *"estimates ran ~10×
**above** actuals — derive `EstimateParams` from recorded costs."* Both halves
are now stale: the derivation already ships, and the bias runs the **opposite**
direction. This doc measures the real gap across all 39 completed missions in
this repo and scopes the refit.

## The finding: the estimate is systematically ~3× LOW, not high

Every completed mission records `**Cost:** $actual vs $low–$high estimated
(expected $exp)` in its `report.md`. Harvested across all 39:

| Metric | Value |
|---|---|
| Missions measured | 39 (all `Complete`) |
| actual / expected — min | **1.39×** (no mission ever came in at or under "expected") |
| actual / expected — median | **2.57×** |
| actual / expected — mean | 3.41× |
| actual / expected — max | 8.92× (`m-d341a7`, doc-heavy) |
| ratio p10 / p25 / p75 / p90 | 1.89 / 2.28 / 4.31 / 5.86 |
| Corpus aggregate | $2,335 actual vs $618 expected → **3.78× under** |
| Actual **exceeds** the `high` bound (2.5× exp) | **22/39 = 56%** |
| Actual within `[low, high]` | 17/39 = 44% |
| Actual **below** the `low` bound (0.5× exp) | **0/39 = 0%** |

Two facts do the work here:

1. **The bias is universal, not noisy.** All 39 missions overran "expected";
   the minimum overrun was 1.39×. "Expected" is not a central guess — it is a
   floor the real number never touches.
2. **The range is miscalibrated in both directions.** The `low` bound
   (0.5×) has never once bound in 39 missions — it is pure dead slack. The
   `high` bound (2.5×) is exceeded by the majority (56%). The empirical ratio
   distribution wants roughly a `1.4×`–`5.9×` band around the current
   "expected", i.e. the whole range should shift up and widen.

The roadmap's "~10× over" almost certainly described an early, pre-calibration
era or a since-fixed unit bug; on the current corpus it is simply wrong. Refit
the model to the data, don't invert a wrong premise.

## Current model (what already ships)

`cost::estimate` (plan §5 Phase-1 formula):

```text
worker_runs    = features·(1+r) + milestones·x·f·(1+r)
validator_runs = 2·milestones·(1+x)              (minus skipped validators)
expected       = worker_runs·avg_worker + validator_runs·avg_validator
               + features·orchestrator_overhead
low = 0.5·expected,  high = 2.5·expected
```

- `cost::calibrate(repo)` folds every `Complete` mission, turns each into one
  `EstimateParams` via `mission_actuals` (mean per-run cost by role, respawn /
  fix rates), and returns their **simple mean** as the live params, plus
  `missions_used`. Defaults apply only at zero missions.
- `cost::apply_shape` widens `high` to `15·expected` and drops confidence to
  `Low` **only** for `DocHeavy` plans when the corpus has zero doc-heavy
  examples. Every other shape is a no-op.

So calibration runs — yet the corpus it calibrates *from* still overruns 3.78×.
That means the miss is structural, not a stale-defaults problem.

## Why calibration converges to a low number

- **The run-count formula undercounts expensive turns.** Cost is dominated by
  orchestrator/judgement turns over a *growing* diff and by respawn/fix loops,
  not by feature count. `m-d341a7`: 2 `AgentJudgement` assertions drove 21.5M
  cache-read tokens across 17 judgement turns. The model prices orchestrator
  work as a flat `$0.25/feature` — a term that cannot scale with mission length
  or diff size, which is where the money actually goes.
- **Per-run means wash out the fat tail.** `mission_actuals` records the *mean*
  worker/validator run cost, then `calibrate` takes the *mean across missions*.
  Two layers of averaging flatten a heavy-right-tailed distribution: a mission
  whose cost is one $40 judgement marathon contributes a modest per-run mean.
- **The multipliers are fixed constants, not fit to residuals.** `0.5×`/`2.5×`
  were plan-time guesses. The data says `low` never bind and `high` is too
  tight for 56% of runs.
- **Blast radius is unmodeled.** Wide cross-crate engine missions (e.g.
  `m-079c36` $124.70, `m-db35a6` $140.09) have ordinary feature counts but
  outsized diffs; nothing in the plan-time inputs distinguishes them from a
  small mission with the same counts.

## Decisions

**D-A — Recenter "expected" to the corpus, by fitting totals not per-run
means.** Replace the two-layer per-run averaging with a fit that makes
`Σ predicted ≈ Σ actual` across the corpus (e.g. scale the calibrated params so
aggregate predicted matches aggregate actual, or least-squares the per-mission
`expected` against `actual`). *Recommendation: yes — this is the single biggest
win; it removes the 3.78× systematic bias directly.*

**D-B — Derive the range from the empirical ratio distribution, not fixed
0.5×/2.5×.** After recentering, set `low`/`high` to the residual p10/p90 of
actual÷predicted (recomputed each calibration). On today's data that is roughly
`0.75×`–`1.8×` of a corrected center. *Recommendation: yes — a range that
brackets ~80% of outcomes is the honest contract; the current one brackets 44%.*

**D-C — Add a length/turn-driven orchestrator term.** Replace flat
`$/feature` overhead with one that scales with a plan-time turn proxy
(`milestones·(1+fix_cycles)` and/or `DocHeavy` shape). This is the structural
fix for the long-mission tail that recentering alone will over-smooth.
*Recommendation: yes, but as slice 2 — recentering (D-A/B) captures most of the
gain; this earns the tail.*

**D-D — Bucket calibration by shape/size instead of one global mean.** Extend
the existing `DocHeavy` special-case into first-class buckets (e.g.
code-change-small / code-change-wide / doc-heavy), each with its own params, so
the wide-engine tail stops being averaged against cheap missions. Blast-radius
proxy: `touch_set` size or crate span. *Recommendation: yes, slice 3 — highest
complexity, do it once D-A/B prove out.*

**D-E — Label the number as the floor it is until refit lands.**
`render_cost_estimate` already prints "based on N missions"; add the historical
overrun ("actuals have run ~2.6× expected across N missions") so a user reading
today's estimate isn't misled before the model is corrected. *Recommendation:
yes — one-line honesty fix, ship immediately, independent of the refit.*

## Build slices

1. **Recenter + empirical range (D-A, D-B, D-E).** Pure refit of existing
   params/multipliers + one display line. Cheapest, removes the systematic
   bias, and is fully testable against the 39-mission corpus as a fixture.
2. **Length/turn orchestrator term (D-C).** New model term + recalibration.
3. **Shape/size-bucketed calibration (D-D).** Structural; needs a blast-radius
   input and per-bucket corpora.

## Done when

The roadmap bar is *"a fresh mission's estimate lands within 2× of its
actual."* Concretely, against a held-out replay of the corpus after slice 1:
median |actual÷expected| within ~1.3×, p90 within 2×, and the shown `[low,high]`
brackets ≥80% of actuals (today: 44%). The estimate stops being a floor and
becomes a range.

## Data source

`.kranz/missions/m-*/report.md` `**Cost:**` lines (39 missions, harvested
2026-07-08). The full model lives in `crates/engine/src/cost.rs`; display in
`crates/cli/src/output.rs::render_cost_estimate`. Related: [worker cost
recording] `WorkerCompleted.cost_usd` / `usage_cost_usd`.
