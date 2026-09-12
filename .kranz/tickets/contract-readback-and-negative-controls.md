---
state: open
title: Contract read-back and negative controls for critical assertions
priority: 2
schedule: once
---

## Goal

At plan approval, help the operator establish that critical acceptance checks
test the requested property. Present an independent explanation of what each
check actually establishes, together with evidence that it rejects an
explicitly chosen defective implementation. Retain both through the existing
gate and evidence-bundle interfaces.

## Context

Implementation note (2026-09-12): the explicit valid/defective control portion
is implemented by `contract_controls.rs` with advisory approval/final-gate
evidence and dashboard fixture review. See `docs/contract-controls.md`.
This ticket remains open for independent model read-back, its input-isolation
proof, and the operator comparison/disposition surface.

Inspired by [Anthropic's FLT formalization](https://www.anthropic.com/research/formalizing-fermats-last-theorem)
and [Prove2Me's audited missions and independent read-back](https://arxiv.org/html/2608.28433v1).
Prove2Me separates approval of a statement's intended meaning from checking
its proof. Its reviewer translates the formal statement without seeing the
original request; a human compares the two. The FLT artifact also uses a
[comparator](https://github.com/leanprover/comparator) to check the target
statement and its definitions against a trusted challenge.

Kranz already has advisory contract diagnostics (`contract_gates.rs` and
`contract_lint.rs`), contained validators, and portable evidence bundles.
This extends those mechanisms to expose semantic gaps between the requested
outcome and the executable check. It belongs to the governance/evidence
surface under `docs/knowledge/decisions/positioning-governance-evidence-layer.md`.
Related tickets: `contract-validation-gates`, `contract-smoke-test-at-approval`,
`evidence-bundle-export`.

**Scope**

- Start with explicitly selected critical command assertions. Give the
  read-back reviewer the executable check and its relevant fixtures/helpers,
  excluding the original requirement, assertion description, and worker's
  explanation. Treat comments and labels in those inputs as untrusted claims.
  Record what is exercised, what is asserted, assumptions, and omissions;
  show the original requirement beside the resulting explanation for approval.
- When a command, future test, fixture, or dependency is unavailable, report
  incomplete evidence. Do not infer test behavior from a command's name or
  the intended feature. The model's read-back supports operator judgment;
  it does not certify semantic equivalence.
- Let the contract author provide a bounded, reviewable negative-control
  fixture or patch for the selected assertion, paired with a valid control.
  An intentionally broken authorization implementation is the first example.
  Preserve the approved inputs and identify their versions in the evidence;
  changes require an explicit contract revision and fresh verification.
- Run controls in disposable snapshots through the existing cleared-environment,
  sandboxed gate runner. Keep the primary checkout and mission deliverable
  unchanged. Compilation/setup failures, missing tests, and timeouts are
  inconclusive; they do not demonstrate rejection of the intended defect.
- Bind the read-back and control results to the assertion, approved check
  inputs, relevant code revision, checker identity, and execution environment.
  Export their scrubbed artifacts and unresolved results through the existing
  evidence bundle. Changed inputs invalidate reuse of the affected evidence.
- Preserve today's advisory approval policy in this first slice. Missing,
  stale, or inconclusive evidence must remain visibly unresolved. Any new
  blocking policy needs a separate operator decision. Persisted schema changes
  require a deliberate `contractChangeRequest` and additive compatibility.

**Deferred**
A general claim-dependency graph, automated mutation search, formal-proof
backends, and additional worker pools are outside this ticket. Ordinary test
results establish evidence within their recorded scope; they do not inherit
Lean's guarantee that verified subproofs compose into a proof of the whole.

## Acceptance hints

- Use the claim "an invalid credential cannot authorize a mutation" as the end-to-end fixture. A check covering only missing credentials must expose that it omits wrong nonempty credentials and verification of unchanged state.
- Verify the read-back session's actual inputs exclude the original request and assertion description. Test wiring and report persistence with a fixture backend; record a separate reviewed real-model example without treating it as a deterministic correctness guarantee.
- The complete authorization check accepts the valid implementation and rejects the deliberately defective one for the intended behavior. An unrelated setup failure and a zero-test run remain inconclusive.
- A regression invariant that already holds on the base is not confused with newly requested behavior: negative controls are explicit defects, not a blanket requirement that every check fail on the base.
- Controls cannot alter the source checkout, authority files, approved checking inputs, or delivered branch. Exercise the production containment path, including its fail-closed behavior.
- Changing an approved checking input or relevant revision makes its prior receipt stale. Missing evidence cannot render as a verified assertion.
- The exported bundle preserves the original claim, read-back, input identities, control outcomes, and operator disposition, with existing redaction and missing-artifact behavior intact. Legacy plans/logs remain readable.
- Use a unique `contract_readback_control_` test prefix (no existing matches when this ticket was authored), nonzero-test guards in drafted contract commands, and all workspace gates before implementation is declared done.
