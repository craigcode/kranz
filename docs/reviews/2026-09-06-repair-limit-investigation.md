# Repair-limit investigation — 2026-09-06 UTC

**Confirmed: a config change reaches the engine but is absent from the
orchestrator's durable-state digest.** This investigation concerns source
`d1eafe037df0dc08dd67e94889937fa66f56f7ac` and mission `m-53a35b` in
[the successful unattended run](2026-09-06-unattended-acceptance.md).
It does not change runtime behavior or rewrite that run's evidence.

## Follow-up implementation

The repair-budget fix adds the effective cap, used cycles and saturating
remaining allowance to the existing execution digest. It explicitly makes
current policy authoritative over planning/research observations and states
that exhaustion does not justify a waiver or establish contract completion.
The CLI override stays on its existing audited control-inbox path after
approval, including enqueue-only operation; moving it into planning is
unnecessary for execution correctness and would change those semantics.

The mock regression starts planning at two, consumes two repair rounds,
accepts a control change to three, and captures the next actual findings
message with one round remaining. It emits that third repair, refuses a
fourth, lowers the cap to one without resetting usage, and verifies replay,
streaming reseed and single-shot execution context. Existing local-tier
escalation and non-waivable final-gate checks remain unchanged. The historical
probe below must still be run against its pinned source; it deliberately
asserts the old omission and is expected to fail against the fixed source.

## Evidence chain

1. Event 1 creates the mission with the default
   `maxFixCyclesPerMilestone=2`.
2. During planning, transcript record 41 reads `state.json`; record 42 returns
   the value 2. The planning response at record 114 repeats it, and the research
   response at record 125 records two cycles as an engine fact. `research.md`
   retains that statement.
3. `cmd_exec_with_backend` queues the `--max-cycles 3` override **after**
   planning and approval (`crates/cli/src/exec.rs`, override block). The run
   loop drains it through the normal control path. Event 38 records the
   accepted value 3; final persisted config still contains 3.
4. All 24 injected turns in `runs/orch-1.jsonl` omit
   `maxFixCyclesPerMilestone`. Execution digests expose `fixCycles` used per
   milestone, but no current cap or remaining allowance. Config changes do
   not inject a separate correction to the old research statement.
5. Decisions 333, 376, 430, 773, and 810 describe the second cycle as the
   final one. Waivers 477 and 866 both cite the exhausted two-cycle allowance.
   At both waivers, two cycles had been used and a third remained configured.

The [structured observations](evidence/2026-09-06-release/repair-limit-observations.json)
pin transcript records, event sequences, and evidence digests. The old
configuration was actually read; this is stronger evidence than assuming an
unsupported number was invented. Whether corrected context would change the
model's disposition requires a new evaluation; the historical run cannot
answer that counterfactual.

A [synthetic probe](evidence/2026-09-06-release/repair-budget-probe.rs) also
reproduces the omission without a model or live credentials. Linked against
the workspace build of the pinned source, it folds accepted config changes
from two to three and then to one: the config updates, but the digest stays
byte-identical; the reseed context also stays identical after the increase.
The probe exits zero when it reproduces this defect. It is investigation
evidence, not a regression test asserting the desired fixed behavior.

To reproduce on the pinned source after `cargo build --workspace`:

```sh
rustc --edition=2021 docs/reviews/evidence/2026-09-06-release/repair-budget-probe.rs --extern kranz_engine=target/debug/libkranz_engine.rlib -L dependency=target/debug/deps -o /tmp/kranz-repair-budget-probe
/tmp/kranz-repair-budget-probe
```

## Enforcement versus decision context

`MissionEngine::fix_cycle_exhausted` in `crates/engine/src/findings.rs` compares
the next round against **current** `self.state.config.max_fix_cycles_per_milestone`.
At two used and cap three, that check is false. No engine cap-exhaustion block
occurred in this run. Findings conversion happens before the cap check so a
waiver can complete a milestone without requesting another repair round.

`digest::render` in `crates/engine/src/digest.rs` never reads the config cap.
`orch_turn`, single-shot execution turns, and `render_reseed` all use that
digest. The Claude planning session persists into execution, retaining its
earlier observation. Thus the engine enforces three while the model continues
to reason from two. This is a governance-evidence defect, not evidence that
the CLI override failed or that the engine enforced a hard-coded two-cycle cap.

The existing final-gate protection for command assertions and enforced Flight
Rules is separate. Nothing in this investigation authorizes widening the
ordinary waiver surface or treating exhausted budget as a valid reason to
ignore a contract violation.

## Recommended correction and verification

Tracked as [orchestrator-current-repair-budget](../../.kranz/tickets/orchestrator-current-repair-budget.md).
Supply the current cap, cycles used, and remaining rounds from folded state
at every decision boundary, including recovery/reseed and config reductions.
Explain that current state supersedes planning observations; a zero remaining
budget bounds repairs and does not itself justify a waiver. Avoid injecting
the entire configuration or adding a new prompt-management system.

Use deterministic regression evidence: fold an accepted two-to-three patch
after planning, then capture the next findings-conversion input and require
two used / three allowed / one remaining. Repeat with a lower cap and a
reseed. Require the engine to allow the third requested repair and refuse a
fourth; keep a failing non-waivable command assertion blocked. Existing static
digest snapshot tests do not cover changing the cap.

This task completes the investigation and prepares the correction. The fix
ticket remains open, and release-candidate closure should retain it as an
open issue until the correction and its workspace/platform checks pass.
