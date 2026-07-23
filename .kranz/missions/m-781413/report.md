# Mission report — m-781413

**Goal:** At the final gate, route a failing contract command assertion the orchestrator judges to be author-broken (a false negative — the underlying requirement is met but the command still fails) straight to an operator escalation (block with a precise 'assertion appears buggy, evidence attached' message) instead of spending fix cycles synthesizing fix-features for the gate itself.

Branch `kranz/mission-m-781413` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 9h 37m 05s
**Tokens:** 18146 in / 60246 out / 5995895 cache read / 326390 cache write
**Cost:** $14.19 actual vs $2.61–$13.06 estimated (expected $6.09)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-781413-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Preflight:** preflight: clear — no advisory issues recorded

## What shipped

### Milestone 1 — Command-broken contract assertions escalate to the operator instead of burning fix cycles ✅

- ✅ **Escalate author-broken final-gate command assertions to the operator** — 1 run
  - `65ed625` [f-1-1] escalate author-broken final-gate command assertions to the operator

## Validation history

### ms-1 round 1 — Command-broken contract assertions escalate to the operator instead of burning fix cycles

No findings.

## Contract outcomes

- ✅ **[a1]** A final-gate command-assertion finding the orchestrator classifies as command-broken causes the mission to BLOCK immediately with a MilestoneBlocked reason that names the failing assertion id and communicates the command appears buggy with evidence, emits NO fixfeature.created for that assertion, leaves the milestone's fix_cycles at 0, and does not COMPLETE the mission — proven by a dedicated mission_test. *(command: `cargo test --workspace command_broken_assertion_escalates_to_operator 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a2]** The escalation is honored ONLY for class=command-assertion findings: a non-command-assertion finding the orchestrator mislabels command-broken does NOT escalate — it flows to the normal fix path and bumps fix_cycles exactly as before (the escape-hatch guard), proven by a dedicated mission_test. *(command: `cargo test --workspace noncommand_finding_marked_command_broken_does_not_escalate 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a3]** The pre-existing final-gate non-waivable command-assertion behavior is unregressed: a genuinely failing command assertion the orchestrator tries to WAIVE is still refused, synthesizes a fix feature, and blocks at the fix-cycle cap. *(command: `cargo test --workspace command_assertion_at_final_gate_is_non_waivable 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a4]** The full workspace test suite is green. *(command: `cargo test --workspace`)*
- ✅ **[a5]** Clippy is clean across the workspace with warnings denied. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a6]** Code is rustfmt-clean. *(command: `cargo fmt --all --check`)*
- ✅ **[a7]** The MilestoneBlocked reason emitted for a command-broken escalation is precise and operator-actionable: it identifies the specific assertion and conveys that the assertion's command appears buggy (a false negative) with evidence attached — framing gate-repair as a human decision, not a product defect to fix-cycle. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
