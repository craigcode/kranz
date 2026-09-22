# Mission outcome reasons review

Reviewed implementation: `c4052e98f8268fc81111a56bb13eb6a598dd13b6`,
updated from main as `3fb8a61815d97fa7542fe63a25480d4d642a2426`.
Both have tree `4e9e2d256625b5956c1302ca6e2d63d1e8b3c1fb`.
The main integration changes ancestry only. This is the implementing assistant's
five-axis review, not independent validation or a release approval.

## Findings

No actionable findings survived the review. The reviewed surfaces were the
engine reason fold and report aggregation; CLI/API/window handling; dashboard
types and rendering; evidence-bundle compatibility; tests and operator docs.

Correctness: only structured causes select a category. Legacy or ambiguous
failures stay unknown; the existing reducer supplies current status separately.
Resolution, replacement, expiry and terminal closure preserve history without
granting consent or claiming a repair. An invalid reducer state downgrades cause
attribution. Mission cohorts use the latest recorded event in the inclusive
window, retain complete histories, and count each selected mission once per
category. Categories overlap.

Readability: the mapping is isolated and documented with its limitations.
CLI/UI distinguish current state from observations and expose source sequences,
attempts and recorded actors. A reported defect remains a checker's judgment;
local capabilities are not asserted to be verified human identities.

Architecture: the implementation reuses the outcomes memo and existing export.
Optional fields preserve older report deserialization. No event or mission-state
schema, execution path, permission, retry or merge policy changes.

Security: reporting issues no command or provider call. Details use existing
scrubbing and truncation; text output removes controls, debug-rendered identities
escape controls, and React escapes displayed data. Existing mission path and
log integrity checks remain in use. Scrubbing is not a guarantee that arbitrary
evidence is safe to publish.

Performance: the existing cached fold is reused; the report adds one reason
scan and category aggregation. It includes full histories for selected missions
and still scans available mission metadata, so it is not a bounded-size or
paginated reporting API. No new throughput or large-history benchmark is claimed.

## Validation and limits

The implementation tree passed the full workspace suite: 3,106 tests, no
failures, ten ignored, including enabled synthetic ACP, evaluator and mount
container checks. Workspace formatting, Clippy with warnings denied, build,
strict Rust documentation, knowledge refresh, domain lint and public-tree audit
passed. Dashboard type checking, 259 tests, build, embedded synchronization and
comparison, and lint passed with pre-existing lint warnings.

The nine reason tests and API integration cover typed and unknown causes,
repair history, pending and denied authority, expiry, replacement, foreign
mission filtering, invalid windows, unique denominators, absent old fields,
missing logs, shared exports and unchanged source/control files. UI tests cover
escaped evidence, absent legacy reports and window selection.

GitHub's complete checks passed on `c4052e9`; the ancestry update requires a
fresh check run on `3fb8a61` before merge. No live-provider qualification, Keychain
access, release tag or version bump is part of this review. Human review effort
and the three pilot cases remain unmeasured.
