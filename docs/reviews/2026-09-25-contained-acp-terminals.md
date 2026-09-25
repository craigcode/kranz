# Contained ACP terminal contract review — 2026-09-25

Base: PR #89, `0507ea63`.
Scope: [terminal contract](../scoping/acp-terminal-provider.md) and
[implementation ticket](../../.kranz/tickets/acp-contained-terminal-provider.md).
This is an implementer review against the five repository axes. It is not an
independent model review, a live adapter qualification or the review-effort pilot.

## Boundary

`kranz-acp` adds wire types, the asynchronous five-operation provider interface
and bounded UTF-8 output retention. The engine owns admission, consent, scoped
handles, asynchronous routing, receipts and containment. The library gains no
process, mission, Docker or credential dependency.

The provider accepts only the engine-owned container ID and the pinned synthetic
Python image. A read-only guest driver is a non-dumpable subreaper; pidfds and
kernel child ownership cover descendants that detach with setsid/double-fork.
A missing cleanup receipt fails the session and removes its namespace. No host
execution fallback, daemon socket in the guest or ambient environment is added.

Only a private test constructor activates this path. Released profiles continue
to advertise `terminal: false`. Sgian transport, pane output, persistent terminal
transcripts, real run-binding admission, adapter terminal routing, PTY support
and a new release remain outside this slice.

## Five-axis review

- **Correctness:** creation requires consent even without a preceding ACP
  permission request. The action binds effective argv/cwd/environment, output
  limit, run/session/generation and container identity. Consumed, expired or
  changed authority refuses before spawn. Kill preserves output/status;
  release invalidates lookup before waiting for confirmed reaping and drain.
  Prompt completion with pending consent or work fails honestly. Start, exit,
  response delivery and namespace cleanup have separate receipts.
- **Readability:** protocol values live in the shared crate; broker scheduling
  and permission handling live beside the ACP adapter; the contained provider
  and guest supervisor have separate modules. Narrow fixture policies and
  deadlines are documented rather than presented as general adapter support.
- **Architecture:** no persisted event/state schema change or new dependency.
  Existing `Other` receipts and permission events carry the evidence. The
  reader remains active during waits and consent. The existing runner continues
  draining receipts after completion and cancellation.
- **Security:** request IDs, payloads, handles, pending operations and output
  retention are bounded. Scope is consumer-bound and authority is not
  deserializable. Guest cwd components disallow symlink traversal; executable
  fds refer to the immutable image. Environment keys are allowlisted and the
  terminal home is private. Failure closes the namespace, including when a peer
  kills the terminal driver. Production admission remains unavailable.
- **Performance:** eight live handles, 32 creations, 16 pending operations and
  1,024 bounded request IDs cap retained state. Output uses a bounded byte deque
  and is not appended to evidence on every chunk. The fixture has explicit
  launch, command, drain and cleanup deadlines. No throughput claim is made.

Review tightened two lifetime/resource edges: rejected attempts consume their
one-use authority, and a releasing handle remains owned until its driver has
exited. Late creation checks the still-live scope again before returning a handle.
Retained request IDs are bounded before insertion into the replay set.

## Verification

The six `acp_terminal_v1` live-container tests cover default capability refusal,
workspace/environment denials, outstanding consent alongside wait/output/kill,
multibyte output flooding, kill/release semantics, forged and released handles,
peer/provider loss, session drop, engine SIGKILL and late consent/completion.
The engine-death proof kills a separate test process; no Rust destructor can
supply its cleanup. The CI lane requires every named proof and rejects skips.
Pure authority tests cover changed inputs, expiry, replay, foreign scope and
previous provider generations. Shared tests cover malformed schema, duplicate
keys, capability opt-in and character-safe byte limits.

Local logs are retained under `kranz-contained-terminals-evidence/20260925`.
The first attempts correctly refused an unshared macOS temporary path and then
a scratch path inside the masked `.kranz` authority tree. The successful proofs
use an ordinary shared scratch directory without weakening either boundary.
No provider credentials, paid model trials or Keychain access were used.

The full local workspace suite passed 3,134 tests, with zero failures and ten
ignored. Workspace Clippy with warnings denied, formatting, locked build and
strict API documentation passed. The packaged shared crate passed 19 tests,
its threaded example, Clippy and strict docs outside workspace feature
unification. Cargo-deny and domain lint passed. The final cleanup removed a
redundant schema predicate without changing policy; the exact PR revision is
rechecked locally and by required CI before merge. The shared crate remains source-only until a
separate release.
