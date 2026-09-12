# contractChangeRequest: opt-in assertion negative controls

Status: authorized by the user's approval of the six reliability enhancements
on 2026-09-12.

`Assertion` gains optional `negativeControl`, described in
[critical assertion controls](../contract-controls.md). It is absent by default
and omitted when absent, preserving legacy plan and event serialization.
`plan.approved`, `plan.revised`, and mission snapshots already carry assertions;
their existing fields and transitions keep their meaning. Present control
definitions validate before approval side effects and revision event writes.
Replanning cannot remove or change an existing definition; legacy assertions
without controls retain their existing replay behavior.

No new event kind or gate verdict is introduced. Selected controls append
advisory deterministic `gate.result` records at approval and final validation.
Only a complete valid/defective pair can produce a passing verdict. The artifact
and explanatory detail explicitly distinguish `not-rejected` from
`inconclusive`; both are non-passing advisory evidence. Missing artifacts retain
the existing unresolved export behavior. New gate results do not change the
standards-pack split, command assertions, or consent policy.

The implementation runs reviewable fixture replacements in disposable detached
worktrees with mandatory containment and read-only checking inputs. It records
fresh input/revision identities and bounded, scrubbed evidence. Runtime files
are gitignored. The parent ticket stays open for independent read-back; this
contract change authorizes deterministic controls only.

Regression coverage includes legacy round trips, unsafe and oversized inputs,
immutable controls across revisions, full authorization checks versus a
missing-credential-only checker, zero-check/setup/timeout outcomes, fresh
receipts, protected checking inputs/source/authority, provider refusal, and
approval/final-gate evidence integration. Workspace and dashboard gates remain
required before delivery.
