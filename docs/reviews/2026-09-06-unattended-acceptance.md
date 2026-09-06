# Unattended acceptance receipt — 2026-09-06 UTC

**Passed on `d1eafe037df0dc08dd67e94889937fa66f56f7ac`.** All 14 jobs in
[workflow 34012676218](https://github.com/craigcode/kranz/actions/runs/34012676218)
passed on attempt 1, including the
[live smoke](https://github.com/craigcode/kranz/actions/runs/34012676218/job/101431059941).
The earlier ordinary [merge-commit CI](https://github.com/craigcode/kranz/actions/runs/34006929129)
also passed; its ordinary trigger skipped smoke. These are separate receipts
from the operator-assisted rehearsal in the previous candidate review.

## What ran

The committed `scripts/acceptance-smoke.sh` created a fresh Python standard-library
fixture and invoked headless `kranz exec --max-cycles 3`. GitHub supplied the
repository's `ANTHROPIC_API_KEY` secret; this review read only secret metadata.
No local OAuth credentials were uploaded. The runner used the pinned Claude
Code 2.1.261 installation and the workflow's Linux bubblewrap setup.

Mission `m-53a35b` completed without operator guidance, unblock, restart, or
resume. The headless plan approval is the declared `exec` behavior; it is not
a human review of the generated plan.

| Observation | Result |
| --- | --- |
| Mission shape | 2 milestones, 5 planned features, all complete |
| Repairs | 10 completed repair features; 5 have recorded commits, 5 have none |
| Independent final audit | 15 tests passed, plus live HTTP, CLI, and documentation assertions |
| Immutable fixture and repair provenance | Original fixture inputs unchanged; authentication helper's first change belongs to a completed repair feature |
| Live step | About 45.1 minutes |
| Recorded model cost | $11.54 |
| Event structure | 876 contiguous events, sequences 1–876; cached state at 876 |
| Sessions | 28 recorded sessions |
| Execution override | Event 38 sets `maxFixCyclesPerMilestone=3` before milestone work |
| Delivery | Mission branch audited before fixture cleanup; CLI summary records `pushed=false` |

## Qualifications

The orchestrator waived two minor validator findings:

- Event 477: missing negative regression coverage for a non-Bearer auth scheme.
  Its rationale says the runtime behavior meets the feature specification.
- Event 866: malformed/disconnected HTTP responses can produce a traceback
  instead of the CLI's formatted error. Its rationale says the nonzero exit
  and stderr explanation meet the contract.

Both dispositions also cite an exhausted **two-cycle** budget, although the
current limit was **three** and each milestone used only two cycles. The
[investigation](2026-09-06-repair-limit-investigation.md) identifies the missing
current-limit context. The independent acceptance contract passed, but that
does not make the waiver rationale accurate or establish that the same
dispositions would have been chosen with correct budget information.

The fixture's worker sandbox setting is the shipped default `off`. All 12
validator sessions have mandatory-containment decisions, and the final audit
uses the production filesystem gate wrapper. This proves those containment
paths, not filesystem containment of default workers. The shared Cargo cache
linking warning remains in the raw log.

The sequence/count inspection is not event-HMAC verification. The runner's
signing key was not retrieved. Raw historical events and transcripts were not
rewritten to repair the budget narrative.

## Retained evidence

- [Machine-readable receipt](evidence/2026-09-06-release/acceptance-summary.json)
  and [repair-limit observations](evidence/2026-09-06-release/repair-limit-observations.json).
- [Original mission artifact](https://github.com/craigcode/kranz/actions/runs/34012676218/artifacts/9983608804),
  named `flagship-acceptance-mission`, artifact ID `9983608804`.
- Original ZIP SHA-256:
  `bab7a1cb7c96ae0c6b3d225d9fd1177693fbc75fdb0dd2f2f3ce5e868536e310`.
  The downloaded bytes independently match GitHub's reported digest.
- GitHub reports artifact expiry at `2026-12-05T04:54:41Z`. A separate private
  evidence directory retains the original ZIP, full smoke log, API receipts,
  plan, research, report, state, and transcripts beyond that retention window.
  Runtime files and credentials are not added to this source repository.

The artifact contains mission records and session transcripts, not the
generated source tree or its Git history. On success,
`scripts/acceptance-smoke.sh` copies only `.kranz/missions` before deleting
the scratch repository and its worktrees. The recorded branch name and commit
IDs therefore do not provide a retained deliverable that a reviewer can check
out or rerun. The final-audit result is retained as execution evidence; this
archive cannot independently reproduce that audit against the delivered code.

## Release status

The API-key and unattended-smoke gaps are closed for this source commit.
Owner confidentiality/licensing approval, the repair-limit correction, actual
distribution notices, protected publication, anonymous-clone verification,
and published-artifact checks remain open. Follow the
[owner review packet](2026-09-06-owner-publication-review.md) and
[public-readiness checklist](../public-readiness.md). No visibility change,
release tag, package upload, or publication was performed by this preparation.
