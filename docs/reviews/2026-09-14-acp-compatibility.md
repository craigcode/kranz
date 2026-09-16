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

## Native-login follow-up (2026-09-16 UTC)

The operator authorized reusing existing CLI login state. Codex passed the bounded
live probe on macOS arm64 using only a private `auth.json` copy. The reviewed
receipt removes account display data, preserves the protocol/report evidence and
hashes the private source. A complete npm lock and runtime installation receipt
pin the actual providers. No ordinary-mission auth defaults changed.

Claude's first live attempt returned `Authentication required`. A local Keychain
experiment prompted macOS approval; the operator directed that Keychain access
stop. The experiment was removed. At this point the probe neither queried nor
linked Keychain state and refused absent file-based Claude credentials before
launch. S2 remained open pending Claude live proof; the Codex result is not a claim about
Claude authentication, contained execution or production readiness.

The added auth-source validation rejects ambiguous API-key/native inputs. Fake
credential tests verify that only credential files cross, with no settings,
hooks or history. These checks require no real login and no model calls.

Validation after the follow-up: all four Rust workspace gates pass (2,953 tests,
zero failures, ten existing ignored), plus both native-file seed tests and the
synthetic probe driver. The native change is isolated to the explicit example;
no runtime auth policy or ordinary mission configuration changed.

## Authorized Claude native-login proof (2026-09-16 UTC)

The operator subsequently explicitly authorized the Keychain-backed Claude test.
The bounded probe passed one prompt in 5,657 ms through Claude ACP 0.77.0 and
Agent SDK 0.3.270 on macOS arm64. The native CLI login worked without fresh browser
OAuth. The retained receipt shows the distinct engine/peer IDs, peer default model
and Manual mode, `end_turn`, the exact partial WorkerReport, no observed tool
events and clean completion. The probe also verified that its empty workspace
remained empty. Reported USD cost telemetry (0.07431) is not billing evidence.

- Correctness: `allowKeychain` is false by default, requires a Claude native login
  on macOS and cannot mix with an explicit credential environment channel. Missing
  native state fails before adapter launch. The selected auth mode and consent
  are recorded; diagnostics do not launch a CLI or authenticate.
- Readability: the runbook separates the earlier authentication failure from this
  authorized pass, and distinguishes the live receipt from source inspection.
- Architecture: only the opt-in example changes. It reuses the native Claude
  backend's scratch-HOME seeding; ordinary ACP mission authentication and backend
  selection remain unchanged. No new dependencies or persisted schema changes.
- Security: the child keeps a disposable HOME and cleared environment. Explicit
  consent allows the existing minimal credential recipe and Keychain link;
  settings, hooks and history are not copied. The Keychain path leaves
  `CLAUDE_CONFIG_DIR` unset so it does not redirect the CLI's native lookup.
  Public evidence removes account display data and operator/install paths, and
  hashes the private source, executed probe and pinned installation lock.
- Performance: one fixed prompt, existing time/capture limits and no retries or
  additional provider calls. New auth tests use fake files and a fake Keychain
  directory, never real account access.

Both providers now have basic native-login text/report proof. This does not prove
live permission grants, client terminal routing, containment or full governed
missions; those remain separate S4/S5/S6/S7 requirements. S2's implementation
review is a self-review, not an independent audit or merge approval.

Validation for this follow-up: all four Rust workspace gates pass (2,953 tests,
zero failures, ten existing ignored), four example auth tests pass, and the
synthetic probe confirms no-spawn diagnostics, report parsing, environment
filtering and refusal to reuse a receipt. Knowledge refresh and staged secret
scans pass. The first full suite found two container tests could not reach the
stopped local Docker runtime; the complete suite passed after starting it. No
provider call was repeated for these regression checks.
