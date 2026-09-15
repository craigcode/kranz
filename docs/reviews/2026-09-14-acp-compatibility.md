# ACP compatibility preparation — self-review

S2 implementation self-review, 2026-09-14 (local date). This is not an independent
audit, a live provider receipt, or approval to promote ACP into a contained role.

- Correctness: the actual worker runner now completes and parses a report from
  a persistent ACP adapter. Engine and peer IDs stay distinct; foreign updates
  fail. Duplicate/malformed/oversized/non-UTF-8 input cannot become a pass.
  Non-natural stop reasons and observed nonzero exits remain failures. Missing
  telemetry stays missing; cumulative cost is not counted twice across turns.
- Readability: protocol, lifecycle, identity and permission behavior are
  documented together. Released-source receipts distinguish inspected versions
  from executed providers. The runbook states credential/readiness limitations.
- Architecture: the existing backend/session seam is retained. The process
  observer is shared with command execution; the new strict line-reader option
  leaves existing tolerant readers unchanged. No persisted event/state schema,
  validator role, scheduler, prompt strategy or automatic promotion is added.
- Security: environment clearing remains in place. Permission selection is
  one-time only, action identity is required, current raw arguments supersede
  earlier announcements, and mode changes/unknown kinds fail closed. The probe
  uses an empty workspace/private HOME, records only environment key names,
  redacts the injected credential and requires a new receipt per attempt.
  Same-group cleanup does not prove containment of an escaped descendant;
  cooperative permissions do not stop actions a peer never reports.
- Performance: frame, message, retained tool state, write and cleanup limits
  bound the transport. The probe adds capture/time limits and no automatic retry.
  No new Rust dependency is introduced. The timeout test deliberately exercises
  the real 30-second handshake deadline.

Local workspace tests pass: 2,953 passed, zero failed, ten existing ignored.
The ACP-filtered set passes 33 tests, including thirteen newly named compatibility
tests. Clippy's redundant-closure finding in the example was corrected; Clippy
with warnings denied, formatting and the full workspace build pass. The synthetic probe proves that
`--check` does not spawn, unrelated environment values do not cross, the actual
report parses, and an existing receipt refuses a retry before spawning.
Actionlint and domain lint pass. The integrating PR carries remaining CI results.

S2 stays open pending separately authorized live Claude and Codex runs with
available local credentials. Neither provider has been launched. The source
receipts and mock success must not be labelled live authentication or production
readiness. Native-state authentication and enforced ACP containment are not
implemented by this patch.
