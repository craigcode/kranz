# contractChangeRequest: opt-in baseline and candidate observations

Status: authorized by the user's approval to continue the evidence stream after
the review packet, 2026-09-21 (America/Los_Angeles).

`Assertion.negativeControl` gains optional `baselinePair`. Absent values stay
omitted, preserving legacy plans and logs. Present definitions pin a full
baseline commit, exact expected outcomes, an environment label and explicit
permission to overlay approved checker files on the baseline. Existing contract
revision equality pins every field. No event kind, transition or blocking
requirement changes.

The existing contained control runner first establishes valid/defective
discrimination, then runs the actual baseline and candidate with the same
approved checker. All four cases share the existing admission, cancellation and
300-second budget. Candidate checker bytes must be delivered; a baseline overlay
is separately identified. The original source identity is captured before any
overlay. Optional exact receipt diagnostics support declared compile-time checks;
unrelated errors, zero tests and missing receipts remain inconclusive.

Control evidence version 2 adds optional source/environment bindings and a pair;
legacy controls retain version 1. An advisory `baseline-candidate:<assertion>`
gate result seals the retained artifact's digest and length in a versioned
`baseline-candidate-v1:` JSON detail descriptor. Its narrow summary contains
approved expectations, identities and observed receipts, without command output
or worker transcripts. Export checks retained bytes against that descriptor.
Fresh stage diagnostics and the human review packet use verified artifacts;
missing or mismatched bytes remain unresolved.

Environment comparison covers the platform, effective sandbox policy and cleared
environment values, with per-case scratch paths and separately bound revisions
normalized. It does not attest mutable tool binaries, caches or remote services.
Human views therefore say source/configuration match, never infer current gate
authority from a pair. Changing source, checker or environment configuration
invalidates that match. Gate evaluators receive observations under existing
policy; these do not satisfy required command receipts.

Required verification: old-log round trips and immutable revisions; fail/pass and
pass/pass actual revisions; new checker overlay; wrong/setup/zero/timeout and
compile-time diagnostics; broken controls; stale source/configuration; missing
or tampered export; source immutability; full workspace and dashboard gates.
