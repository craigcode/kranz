# Shared ACP client and Sgian terminal integration

The operator approved the staged approach on 2026-09-24: settle ownership,
prove a minimal second consumer, then extract without widening the worker's
capabilities. Terminal mediation and Sgian integration remain subsequent slices
with their own containment and authority proofs. No provider trial is authorized
by this scope. The human review-effort pilot has separate completion criteria.

The first implementation supplies `kranz-acp`, the engine adapter, shared
synthetic fixtures and a provider-free threaded consumer example. This is a
source dependency until a future release, not a newly published registry crate.
Review and verification are recorded in
[the extraction review](../reviews/2026-09-24-shared-acp-client.md).

## Grounding

Baseline: Kranz v0.4.1 (`4de9c56c9e1c0e68813b79548ff0a6bc21db4e59`). Before extraction:

- `crates/engine/src/backend_acp.rs` currently combines protocol framing,
  session state, permission handling, mission-facing events and process cleanup.
  It advertises no filesystem or terminal service and refuses resume.
- `acp_worker.rs` and `acp_container.rs` own qualified startup, credentials and
  descendant containment. These remain security boundaries during extraction.
- `orchestrator/live_permissions.rs` owns durable consent and action bindings.
- `sgian.rs` is already a best-effort per-run credential/revocation lane.
  It does not provide terminal RPCs or expose a host socket to a contained worker.
  See [current coordination](../sgian-coordination.md).

ACP terminal support is optional. The five operations are create, output,
wait-for-exit, kill and release. Creation carries a session, command/arguments,
optional environment and absolute working directory, plus an output bound.
Output reports truncation and available exit status. Kill preserves the handle;
release terminates remaining work and invalidates it. These operations do not
define keyboard input or guarantee a PTY. See the
[v1 terminal contract](https://agentclientprotocol.com/protocol/v1/terminals).

Protocol cancellation remains separate from host process cleanup. Session
load/resume requires advertised support; Kranz's fresh attempt policy is its own
rule. Retain the existing restrictions during extraction. See
[prompt turns](https://agentclientprotocol.com/protocol/v1/prompt-turn) and
[session setup](https://agentclientprotocol.com/protocol/v1/session-setup).

The read-only Sgian inventory was refreshed at clean source commit
`b72eabea253d8a06c9c0577af87c1b49b63ee83a` on 2026-09-24. No Sgian files were
changed and its daemon was not run. Source observations:

- [`AgentBackendKind`](https://github.com/craigcode/sgian/blob/b72eabea253d8a06c9c0577af87c1b49b63ee83a/src-tauri/src/lib.rs#L234)
  contains Claude and Droid, so ACP is new backend work.
- [`handle_run_process`](https://github.com/craigcode/sgian/blob/b72eabea253d8a06c9c0577af87c1b49b63ee83a/src-tauri/src/lib.rs#L3660)
  returns the final result of a non-PTY host invocation. It filters inherited
  environment rather than clearing it and does not execute in Kranz's namespace.
  Forwarding terminal requests to it would widen a qualified worker's authority.
- [`agent_stream.rs`](https://github.com/craigcode/sgian/blob/b72eabea253d8a06c9c0577af87c1b49b63ee83a/src-tauri/src/agent_stream.rs#L357)
  owns a writer queue, reader threads and child killer. Drop denies outstanding
  approvals and kills the CLI; Unix escalation and Windows direct-child cleanup
  are not Kranz's contained descendant lifecycle. Do not share the child between
  these supervisors or infer containment from an agent pane.
- [`handle_as`](https://github.com/craigcode/sgian/blob/b72eabea253d8a06c9c0577af87c1b49b63ee83a/src-tauri/src/lib.rs#L4986)
  checks credential revocation and scopes, binds the holder, and gates credentialed
  prompt/approval/interrupt requests on the pane lease. That is UI coordination,
  not authority to answer a Kranz consent proposal or start a contained command.
  The future integration must carry the exact Kranz run/session/proposal binding.
- `sgian-protocol` remains an intentionally empty reserved crate. Sgian has no
  direct Tokio dependency in its manifest. The isolated threaded consumer proves
  a current-thread runtime can sit behind its thread-based design without an
  engine dependency; it does not prove a daemon connection, pane or live adapter.

This is a source-level lifecycle/authority assessment, not a daemon security
audit. Restart recovery, execution inside the selected namespace, scoped terminal
credentials and output retention still require adversarial acceptance tests.

## Ownership decision

| Component | Owns | Must not acquire |
| --- | --- | --- |
| `kranz-acp` | Bounded framing, protocol/session state, request correlation, raw typed updates and callback interfaces; shared conformance fixtures | Mission state, approval authority, credentials, sandbox policy or engine dependencies |
| Kranz adapter | Mapping protocol updates into `AgentEvent`, feature/session binding, durable permission decisions, budgets and evidence | UI leases as a substitute for consent |
| Runtime supervisor | Owned process/namespace lifecycle, cleared environment, enforced paths/egress, hard cancellation and cleanup receipts | Reliance on cooperative ACP callbacks for containment |
| Sgian provider and pane | Presentation, output retention, authenticated terminal handles and operator interaction within the selected execution boundary | Unrestricted host execution on behalf of a contained worker |

Tokio is not itself the dependency problem. Keep the shared crate small and
independent of `kranz-engine`; do not move mission event semantics into it merely
to make both products share a name. Preserve raw protocol identities so each
consumer can normalize events without inventing attribution.

## Delivery slices and acceptance

1. **Boundary decision and consumer spike.** Inventory the ACP module's actual
   dependencies and finish the Sgian lifecycle/authority review. Agree who owns the child and
   who can forcibly stop it. Demonstrate a second minimal consumer without an
   engine dependency. This is the first review checkpoint.
2. **Extract with no behavior change.** Move framing and session protocol into
   `kranz-acp`; keep mission mapping and policy in the engine. Run the existing
   malformed/fragmented input, bounded output, permission identity, cancellation,
   process-death and report tests through the new boundary. Filesystem/terminal
   capabilities and resume stay off. Compare normalized events and errors with
   the current implementation, including unknown/missing cost data.
3. **Provider contract and contained fake.** Define a `TerminalProvider` with
   the five protocol operations and explicit ownership/cancellation. Advertise
   terminal support only when a configured, admitted provider is available.
   Bind every opaque handle to its session and run. Reject cross-session use,
   traversal/symlink escapes, disallowed environment, oversized output and stale
   handles. The provider must require authority for the exact command itself;
   never assume the agent previously requested permission. Concurrent permission
   and terminal requests must not deadlock the protocol reader.
4. **Sgian integration under a new qualification.** Show terminal output in a
   pane while execution stays inside the admitted namespace. Validate output
   limits and retention, provider/daemon death, restart, cancellation, release,
   delayed exit status and orphan cleanup. A pane lease grants UI control, not
   permission to bypass Kranz's writer. A host terminal backend needs a separate
   explicit profile and proof; it is not a fallback when containment fails.
5. **One adapter, then additional adapters.** Prove whether each exact adapter
   actually uses client terminals. An adapter may still execute tools internally;
   terminal capability cannot honestly promise that every command is visible.
   Retain bypass probes and OS containment. Any live-provider trial needs a
   separately bounded approval after provider-free qualification.

Each slice is a separate ticket. Frontmatter is authoritative for priority,
one-shot scheduling and dependencies; these are backlog stages, not recurring
runs or promises of calendar dates:

- [Boundary and consumer spike](../../.kranz/tickets/shared-acp-boundary-spike.md)
- [Behavior-preserving extraction](../../.kranz/tickets/shared-acp-client-extraction.md)
- [Contained terminal-provider contract](../../.kranz/tickets/acp-contained-terminal-provider.md)
- [Sgian integration qualification](../../.kranz/tickets/acp-sgian-terminal-qualification.md)
- [Pinned adapter qualification](../../.kranz/tickets/acp-terminal-adapter-qualification.md)

As with the existing roadmap, a ticket's done label is not a recorded Complete
mission. Out-of-mission implementations require the documented, explicit
operator reconciliation before admitting dependent missions; see
[dependency admission](../tickets.md#dependencies-blocked-by).

## D1–D4: approved direction, with later proof gates

- **D1 — shared boundary:** extract protocol and raw update types first; retain
  engine-specific normalization and process policy with their existing owners.
- **D2 — execution location:** use a pane backed by the existing contained
  namespace. Do not mount a broad host control socket or run commands on the host
  as an implicit compatibility fallback.
- **D3 — human step-in:** first support observation and exact permission answers
  through existing authenticated Kranz controls. Prompt ownership transfer and
  terminal keyboard input need a separate authority/lifecycle decision.
- **D4 — rollout:** opt-in provider/profile, one pinned adapter first, shared
  synthetic conformance tests in both consumers, then explicit qualification.

No new worker pool, model routing, prompt management, factory UI or automatic
merge authority belongs in this stream. The gate contract remains the existing
decision/evidence boundary; these changes supply observable execution beneath it.

## First extraction boundary

`kranz-acp::Client` owns request numbering, initialization/version checking,
session identity, prompt correlation and cooperative cancel frames. `Frame`
keeps raw request IDs, method names and parameters; `SessionUpdate` borrows the
original update and classifies only the variants currently interpreted by Kranz.
Unknown fields and usage/cost data are retained without normalization.

`backend_acp.rs` keeps the read loop and deadline selection, bounded writes,
credential filtering before logs, pending permission state, event mapping,
message/tool accumulation limits and all process/container cleanup. A local
`Input` enum separates broker answers from wire frames. Shared code cannot
fabricate a broker answer. Permission callbacks remain raw requests handed back
to the consumer, not a blocking policy callback inside the protocol reader.

The existing bounded stream reader and duplicate-key decoder move once, including
their tests. Engine modules re-export them so other backends retain identical
behavior without another implementation. Only `io-util` is enabled by the shared
crate's normal Tokio dependency; runtimes, deadlines and test macros are chosen
by consumers. No mission types, OS process handles or `cap-std` cross this boundary.

The consumer example uses bounded in-memory pipes and a five-second deadline
inside a joined standard thread. The `conformance` feature exposes a synthetic
turn fixture used by both the example and the existing engine event assertions.
It demonstrates protocol/runtime fit only. A real Sgian bridge must add bounded
UI/control channels, stale-run rejection and its own separately reviewed
supervisor; this example does not certify Sgian process cleanup.
