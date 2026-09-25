# ACP terminal provider: implementation contract

This starts the third slice of the [shared ACP stream](shared-acp-client.md).
The shared client extraction is PR #82; the implementing ticket is
[acp-contained-terminal-provider](../../.kranz/tickets/acp-contained-terminal-provider.md).
The contract now has a contained synthetic implementation and adversarial tests.
Only a private test constructor enables it. Released profiles still advertise
`terminal: false`; this does not qualify Sgian or a real agent adapter.

## Wire boundary

Follow the [stable ACP v1 terminal contract](https://agentclientprotocol.com/protocol/v1/terminals).
Keep wire types and the provider interface in `kranz-acp`; keep admission,
consent, receipts and namespace ownership in the engine. A provider receives
typed requests, never a shell string assembled by joining arguments.

| Method | Request data after `sessionId` | Result | Required behavior |
| --- | --- | --- | --- |
| `terminal/create` | Command, optional arguments, environment entries, absolute cwd and output byte limit | Opaque `terminalId` | Return after authorized creation, without waiting for command exit. |
| `terminal/output` | `terminalId` | Output, truncation flag, optional exit status | Return the current bounded snapshot without waiting for exit. |
| `terminal/wait_for_exit` | `terminalId` | Optional exit code and signal | Wait asynchronously; leave the protocol reader available for other requests. |
| `terminal/kill` | `terminalId` | Empty result | Stop the command; preserve its handle, final output and eventual exit status. |
| `terminal/release` | `terminalId` | Empty result | Stop any remaining work, confirm cleanup and invalidate the handle. |

Output is bounded in bytes and truncates from the beginning at a UTF-8 character
boundary. Output retained for presentation after release is an artifact, not a
live handle. Neither these methods nor `session/cancel` imply a PTY or keyboard
input. Cooperative turn cancellation, command termination and resource release
remain separate operations.

The provider interface has these five asynchronous operations and a
consumer-defined, non-wire authority value for creation. `kranz-acp` does not
mint that authority, serialize it or treat it as a permission decision. The
engine must consume the value once at the execution boundary. Shared protocol
types must not depend on mission state, Docker, filesystem policy or Sgian.

The exported interface is in `crates/acp/src/terminal.rs`. `Scope` is trusted
consumer context, the returned string is an opaque handle, and every later
lookup requires both scope and handle. `Create` contains the approved action.

```rust
trait TerminalProvider: Send + Sync {
    type Authority: Send;
    fn create(&self, scope: Scope, action: Create, authority: Self::Authority)
        -> impl Future<Output = Result<String, Error>> + Send;
    fn output(&self, scope: Scope, id: String)
        -> impl Future<Output = Result<OutputSnapshot, Error>> + Send;
    fn wait_for_exit(&self, scope: Scope, id: String)
        -> impl Future<Output = Result<ExitStatus, Error>> + Send;
    fn kill(&self, scope: Scope, id: String)
        -> impl Future<Output = Result<CleanupReceipt, Error>> + Send;
    fn release(&self, scope: Scope, id: String)
        -> impl Future<Output = Result<CleanupReceipt, Error>> + Send;
}
```

Internal receipts carry more information than ACP's empty kill/release result.
The adapter maps only confirmed outcomes to wire success and retains the receipt
as evidence. An uncertain outcome remains an error even when the peer disconnects.

## Admission and exact authority

Construct the provider only from a supervisor-owned execution context. That
context binds the mission/run, engine session, peer session, provider generation
and owned namespace identity. A path, container name or pane ID supplied by the
agent cannot construct or retarget it. A provider restart changes generation;
there is no implicit reconnection to live handles from the old generation.

Advertise `terminal: true` only after that context is admitted and its provider
is ready. With no provider, an unqualified profile or failed setup, retain the
current `false` capability and refusal behavior. The released ACP profiles do
not acquire a new execution path merely because their images already passed
the worker proofs. Any supervisor/service change must renew the affected
containment and lifetime evidence before that profile enables terminals.

`terminal/create` must create an exact-action consent proposal even when the
agent never sent `session/request_permission`. Bind consent to the effective
executable identity, ordered arguments, cwd, allowed environment, output limit,
run/session/generation and namespace identity. Apply immutable policy denials
before presenting a proposal. Record the decision before consuming its one-use
execution authority. A permission for another tool call, a pane lease or an
earlier similar command cannot satisfy this check.

Resolve cwd and executable paths inside the execution boundary. Reject traversal,
symlink escapes and disallowed environment keys there; a host-side canonical
path check alone is insufficient because the worker can mutate its workspace
between validation and use. Preserve the approved path binding through spawn,
or refuse if the binding changed. Define the effective environment explicitly;
never copy host or adapter credentials into terminal commands by default.

Persist action digests and scrubbed summaries, not raw credential-bearing
arguments or environment. Record separate requested, resolved, started, exited
and cleanup outcomes. A provider acknowledgment alone is not evidence of a
successful command or confirmed removal.

## Lifetime and concurrency

Own a bounded table of terminal handles and pending operations per session.
Every lookup checks run, peer session and provider generation. Unknown, forged,
released and stale handles fail before a provider call. A handle is never a
host PID, filesystem path, pane credential or caller-selected container name.

Keep the ACP reader independent of consent and provider waits. Schedule bounded
operations and feed their completions through a separate bounded channel, as
live permission answers already do. A waiting `wait_for_exit` must not prevent
`kill`, `release`, a permission answer, another output snapshot or cancellation.
Do not hold a provider/table lock while awaiting a command or human decision.

Use explicit request and session budgets, retaining the existing live-permission
limit and deadline. Bound creation, writes, output retention, pending waits,
drain and cleanup. When a timed-out creation might have started work, invalidate
its authority and reconcile the owned namespace; do not retry it as though
nothing happened. Stale completions must not resurrect a released handle.

A process-group kill alone cannot prove cleanup of a command that calls
`setsid`. The contained fake must demonstrate descendant ownership. If targeted
cleanup cannot be confirmed, fail the session and stop its owned namespace,
retaining an uncertain-cleanup receipt until removal is verified. Never report
a successful release merely because the host Docker client exited. Engine or
provider death must still leave the supervisor's existing lease able to end
the namespace without agent cooperation.

Sgian's current host `RunProcess` cannot be the fallback implementation. The
later Sgian slice transports scoped requests and output for this same admitted
context; a pane lease controls UI interaction and cannot create execution
authority or transfer ownership of the supervised child.

## Implementation and proof order

1. Add typed terminal requests/results and the provider interface to `kranz-acp`.
   Test schema handling and the default capability without adding engine or
   process dependencies to the isolated package.
2. Add the engine admission context, exact-action proposal, one-use authority,
   bound handle table and asynchronous completion path. Keep configuration
   admission closed while only test providers exist.
3. Implement a provider-free terminal fixture inside the owned test namespace.
   Exercise actual bounded execution and hostile descendants; an in-memory
   state mock is useful for routing tests but cannot certify containment.
4. Add adversarial tests and retained receipts before enabling a profile.
   Only then proceed to the separate Sgian integration qualification ticket.

| Proof | What must be observed |
| --- | --- |
| Capability omission | Default client remains false; unadmitted terminal requests cannot execute. |
| Authority and replay | Missing/denied/expired consent, changed argv/cwd/env and reused authority all refuse. |
| Handle identity | Foreign session/run, forged handle, previous generation and post-release lookup refuse. |
| Workspace boundary | Traversal, symlink replacement and environment redirection cannot escape at spawn time. |
| Concurrent control | An outstanding human decision and `wait_for_exit` do not block output, kill or cancellation. |
| Output bounds | Flooding and multibyte truncation preserve the byte cap, valid text and truncation status. |
| Kill versus release | Kill preserves output/status access; release invalidates the handle only with honest cleanup evidence. |
| Failure and orphans | Lost peer/provider, late create completion and detached descendants end within the namespace deadline. |
| Regression | Existing permission/event semantics, package isolation and workspace gates remain green. |

No live adapter or provider credential is needed for this slice. Real adapter
use of client terminals is a separate qualification: an advertised capability
does not prove that an adapter routes every command through it.


## Contained fixture limits

The engine binds Docker's full container ID from its own create receipt, rather
than accepting a name or target from ACP. The synthetic profile pins the existing
Python proof image by digest. It requires a writable workspace, `fs+net` with no
egress and no extra writable roots. No public configuration flag enables it.

The first policy accepts `/bin/sh` and `/usr/local/bin/python3` from the immutable
image, and only the bound workspace mount root as cwd. Nested cwd, traversal,
symlinks and workspace executables are refused. The guest supervisor opens cwd
components without following symlinks and spawns via the pinned executable fd.
Terminal commands share a separate, initially empty home under the owned scratch
root. Their effective environment is fixed PATH/LANG/HOME/TMPDIR plus explicitly approved LANG, LC_ALL,
TERM, NO_COLOR or CI entries. No adapter or host environment is copied.

Each command has a 30-second fixture lifetime, including retention of its handle;
release it within that interval. Creation, kill and release have eight-second
host deadlines. Guest descendant cleanup and output drain each have three
seconds; the capture task has a 40-second outer limit. Session completion has
one eight-second cleanup budget. Missing receipts, driver failure or an exhausted
budget fail the session and remove the entire namespace. The existing five-second
lease still stops the namespace when the engine dies.

There are at most eight live terminals, 32 creations and 16 pending operations or
consent requests per session. The request/replay budget is 1,024 IDs of at most
256 bytes. Typed requests are at most 32 KiB; output defaults to 64 KiB and may be
set from zero to 1 MiB. Output remains valid UTF-8 with a separate truncation flag.

The guest driver is a non-dumpable Linux subreaper. It uses pidfds and kernel
child ownership to kill and reap detached descendants before reporting exit.
Kill retains output and status. Release invalidates lookup immediately and keeps
the retiring resource supervised until cleanup is confirmed. Requested, consent,
started, exited, operation-result and namespace-cleanup receipts have distinct
meanings. The runner drains those receipts after normal completion or abort.
Output text stays in the live handle; this slice records bounded outcome metadata,
not a persistent terminal transcript or a Sgian pane.

The `acp_terminal_v1` CI lane runs six live-container tests, requires each named
proof and rejects skip markers. Pure contract and authority tests also cover
changed arguments/environment/output limits, expired and consumed consent,
foreign runs/sessions/generations, malformed schemas and UTF-8 tail boundaries.
These are synthetic proofs without provider credentials or paid model calls.
