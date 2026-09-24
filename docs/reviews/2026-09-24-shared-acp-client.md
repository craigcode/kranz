# Shared ACP client extraction review — 2026-09-24

Baseline: v0.4.1, `4de9c56c9e1c0e68813b79548ff0a6bc21db4e59`.
Scope: [shared ACP ownership and staged integration](../scoping/shared-acp-client.md).
This is an implementer review against the five repository axes, not an independent
model verdict or the human review-effort pilot.

## Result and boundary

The first two tickets supply the shared protocol crate, an isolated second
consumer spike and the Kranz adapter extraction. Process ownership, credential
filtering, durable permission decisions and `AgentEvent`/cost normalization stay
in the engine. The remaining three tickets cover a contained terminal provider,
Sgian integration and pinned-adapter qualification, in that order.

No client filesystem/terminal services or resume are enabled. No Sgian source,
qualified worker profile, provider login or release version is changed. The
shared crate is source-only until the next release; packaging checks and the
bottom-up publishing instructions now include it.

## Review

- **Correctness:** compared request shapes, handshake errors, session identity,
  prompt correlation and cancel output with the baseline. The failed-send path
  clears the new reservation, preserving the adapter's prior outstanding-turn
  state. The original normalized-event assertions consume the same synthetic
  fixture as the threaded consumer. Existing malformed input, foreign session,
  permission identity, fragmented consent, report, stop reason, unknown cost,
  streaming cost delta, process death and bounded cleanup tests remain in place.
- **Readability:** the local `Input` enum separates broker answers from wire
  frames. The shared `Client` is a small protocol state machine; callers own
  reading, deadlines and callbacks. Unknown update variants and fields stay raw.
  The example explicitly names its synthetic peer and limited proof claim.
- **Architecture:** normal dependencies are Serde, serde_json and Tokio io-util.
  The packaged-crate check builds outside the workspace, rejecting engine,
  cap-std, OS process dependencies and Tokio process/network/full-runtime
  features. The standard-thread/current-thread-runtime consumer proves a useful
  Sgian embedding shape without making Sgian depend on mission types. Actual
  daemon integration and UI/control channels remain later work.
- **Security:** strict unique-key decoding and bounded readers move without
  algorithm changes. A source comparison confirmed that only visibility and one
  documentation link changed in those implementations. Profile credential
  filtering still runs before raw-frame retention; broker answers cannot be
  constructed by peer JSON. The engine retains cleared child environments,
  permission binding, deny policy, write deadlines, owned process/container
  cleanup and cleanup receipts. Sgian `RunProcess` is explicitly rejected as an
  implicit host-execution fallback in the next-stage design.
- **Performance:** no new polling task, queue, subprocess or runtime is created
  by the library. Frame limits and cancellation-safe partial-line buffering are
  preserved. Borrowing the update avoids the previous extra JSON subtree clone;
  no benchmark or throughput improvement is claimed.

## Verification

The isolated package ran 16 tests: 10 moved stream regressions, five protocol
checks and one threaded-consumer test. Its executable exchanged five raw updates,
two prompt turns and a cancelled permission over fragmented bounded pipes,
closed its writer and joined within its five-second deadline. Isolated clippy
and strict docs passed. Its resolved test graph contained 16 packages and only
Tokio io-util plus the runtime/time/macro features needed by the example.

Workspace verification is recorded in the operator-retained
`kranz-shared-acp-evidence/20260924` logs. The first workspace test build caught a
missing test-only re-export of the truncation marker; that was fixed before the
final run. `cargo test --workspace --locked` then passed: 3,122 tests, zero
failures and 10 ignored, with runtime capability skips recorded separately.
Workspace clippy with warnings denied, formatting, locked build,
strict docs, package licenses, dependency notices and cargo-deny passed.

All five tickets were read with the real CLI parser: all 31 scoping/acceptance
bullets survived, one-shot schedules parsed and the linear dependency chain
matched the design. Domain lint and knowledge freshness passed. These tickets
do not create a recorded Complete mission or admit dependent work automatically.

Local execution is on macOS. Docker is stopped: daemon-dependent tests record
capability skips, and explicitly opt-in containment probes remain unexercised
locally. Existing Linux Docker and Windows/macOS CI lanes remain required. No
live model/provider calls, credentials or Keychain access were used. The spike
does not qualify Sgian lifecycle handling, a real adapter, terminal mediation,
PTY support or visibility of commands an adapter executes internally.
