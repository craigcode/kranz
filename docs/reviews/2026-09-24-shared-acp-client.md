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
matched the design. Domain lint and knowledge freshness passed. A post-commit
freshness checks identified dependent standing notes (skill capture, lessons
and the vault index); each was re-reviewed against the changed roadmap,
invariants and citations before the final check. These tickets
do not create a recorded Complete mission or admit dependent work automatically.

Local execution is on macOS. Docker is stopped: daemon-dependent tests record
capability skips, and explicitly opt-in containment probes remain unexercised
locally. Existing Linux Docker and Windows/macOS CI lanes remain required. No
live model/provider calls, credentials or Keychain access were used. The spike
does not qualify Sgian lifecycle handling, a real adapter, terminal mediation,
PTY support or visibility of commands an adapter executes internally.

## Desktop CI correction

The first PR head passed the Linux/macOS Rust suites and Linux container proofs,
but both desktop compile jobs rejected the standalone Tauri lockfile under
`--locked`: it lacked the extracted crate. The correction adds only `kranz-acp`
and the engine dependency edge. A parsed before/after comparison confirms that
all existing registry package versions, checksums and dependency edges remain
unchanged. The root workspace and production Rust source are unchanged.

The corrected macOS desktop `cargo check --locked` passed. Dashboard typecheck,
259 tests, build, embedded sync/check and lint passed; the embedded bundle has
no diff. Desktop `cargo audit --deny unsound` passed when run from its CI working
directory using the existing advisory policy. An earlier root-directory audit
invocation did not load that policy and its failed log is retained; no audit
exception was added or weakened. The corrected head then needed its CI results
and PR review before merge.

## Independent review and merge

Three fresh reviewers examined exact head `d4106c5` against `4de9c56`: protocol
and bounded I/O, engine permission/lifetime behavior, and packaging/dependency
boundaries and scope. None found an actionable issue. The isolated packaged
crate proof was repeated successfully. One reviewer's adapter test build ran
out of local disk before any tests executed; after clearing an obsolete release
build cache, the focused workspace adapter target passed all 27 tests.

The corrected Linux CI job failed three required Docker availability probes.
The same Rust source had passed the preceding Linux jobs. The failed job was
rerun without weakening capability requirements and passed, including the full
workspace suite. The original failure and the successful rerun are retained in
the operator's evidence directory. All required checks were green before merge.

[PR #82](https://github.com/craigcode/kranz/pull/82) merged at
`7943324aa7accbf81661ae278cf7f42034f84be7` on 2026-09-25 UTC. The extraction is
on main; the latest published release remains v0.4.1. The terminal-provider
implementation, Sgian integration and adapter qualification remain separate,
unfinished tickets.
