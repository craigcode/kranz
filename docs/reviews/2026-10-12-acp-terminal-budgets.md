# ACP terminal budgets: R4 fixture implementation

The approved R4 change separates terminal deadlines and request accounting in
the existing contained synthetic provider. Production terminal capability stays
disabled. The v1/v2 profile definitions, qualified resources, selected host tuple
and 600-second production session ceiling are unchanged. No provider invocation,
credential read, installed binary or operator configuration change is required.

## Contract

The code-owned fixture defaults are a 600-second command and 300 seconds of
output retention after exit, narrowed by the remaining terminal session window.
Tests can select shorter positive durations within those maxima. Peer requests
cannot select or extend a deadline. The exact consent action records the terminal
budgets and requested OOM preference; start receipts bind the effective remaining
window to the terminal scope, namespace and action digest.

Command expiry reaps descendants and drains output before the waiter receives a
`-32000` error with `data.kind: terminalBudget`, a named budget and the actual
exit status. Output remains readable until retention ends. Normal exit still
returns its status. Retention expiry closes and reaps the Docker control driver
before freeing capacity. Bounded tombstones preserve exit status across races
with concurrent wait/release; expired output is not retained in them. Cleanup
uncertainty remains a session failure. Create/kill/release keep eight-second
bounds, guest reap and drain each keep three-second bounds, and host capture
covers command plus retention (bounded by the session window) with 24 seconds
for launch, cleanup, drain and control-process reaping. Waits use that envelope
instead of the former fixed 35-second failure deadline. Namespace ownership,
stdin-EOF shutdown and the existing lease remain in force.

Capacity is reserved before consent: at most eight combined live handles,
reservations and issued authorities, and at most 32 creations including reserved
future creations. Denial releases the slot. Authority consumption and insertion
of the live entry share one lock, preserving the cap under concurrent approvals.
Malformed or stale create attempts still burn their authority. Consent keeps
16 pending decisions and 1,024 lifetime request identities. Controls use their
own 32 in-flight slots and release identities after a response. Output reads use
a fixed one-second window of ten reads per terminal. Capacity, creation, pending,
rate and deadline limits have named, recoverable budget errors. Duplicate active
request identities and uncertain provider cleanup still fail closed. The async
receipt queue allows three lifecycle records for each of 32 possible creations.

The guest requests `oom_score_adj=500` only in the command child before exec,
reads it back over a private pipe closed before untrusted code runs, and reports
unavailable observations as null. The supervisor is single-threaded. The forked child temporarily restores normal
procfs ownership only to open its own hint file, then restores non-dumpability
before readback; the supervising process never changes that protection. Synthetic
proof checks child and descendant inheritance and an unchanged peer value. The
hint does not guarantee victim selection and never classifies SIGKILL alone as
an OOM event. The container resource ceiling remains the boundary.

## Proof and remaining scope

Three new named Docker cases cover deadline expiry with descendant cleanup,
retention and hint inheritance; eight outstanding consent reservations with a
ninth refused before consent and recovery after denial; and 1,100 repeated
output polls plus 32 pending waits while a separate consent/create succeeds.
Unit coverage checks one-use authority, reservation/creation accounting, control
identity reuse, pending limits and the bounded lifecycle receipt queue. CI
requires each new actual-container case by name and rejects skip markers.

R5 remains open for a multi-minute build, delayed waits, unpolled output floods,
all remaining exhaustion cases and cooperative cancellation with live terminals.
This implementation does not turn those future proofs into completed evidence.
Real-adapter terminal enablement remains a separate authorized qualification.

## Five-axis review

Self-review, not an independent agent review.

- Correctness: reservation transitions are atomic; expiry is recoverable only
  after confirmed child cleanup; retained status survives concurrent release.
- Readability: budgets and typed limit names are explicit, and wire request
  shapes stay unchanged. One provider owns terminal lifetime and output state.
- Architecture: reuses the existing provider, consent and owned-container seams.
  No execution pool, routing feature or persisted event/state schema change.
- Security: commands still run only in the admitted fixture namespace with exact
  authority and cleared environment. Peer metadata cannot widen budgets. OOM
  observations come from the trusted supervisor rather than command output.
- Performance: terminal/control tables, output tails, tombstones and receipts
  remain bounded. Polling releases control identities instead of accumulating
  lifetime state. Normal completion does not wait for the retention deadline.

## Validation

All four required workspace gates passed: 3,182 tests passed, zero failed and
16 explicitly ignored; Clippy across all targets with warnings denied;
`cargo fmt --all --check`; and workspace build. All nine named actual-container
terminal proofs passed without a skip marker. The final proof uses 1,100 distinct
poll identities and then reuses completed IDs for waits and a new consent.
The [evidence summary](evidence/2026-10-12-acp-terminal-budgets.json) binds final
source hashes, required case names and successful gate-log hashes. It is a test
verdict summary, not real-adapter admission or full per-command transcripts.
Knowledge freshness passed after rechecking the four notes citing changed code.

The first container proof found unavailable OOM-hint readback: a fork inherited
the supervisor's non-dumpable procfs ownership. Restricting the ownership change
to opening the command child's own hint file fixed it; the final proof confirms
500 in the child and descendant, with the peer unchanged. The supervisor retains
its protection throughout. The existing authority, foreign-handle, hostile-peer,
engine-death and default-disabled tests also passed.

Obsolete regenerable build artifacts were removed to preserve the existing
4 GiB disk-admission floor; retained provider evidence and its original prepared
runner were untouched. The final Docker inventory contained zero containers and
zero owned networks, and Colima was restored to its original stopped state.
The staged secret scan and public-tree audit passed; the optional private
operator-vocabulary scan was unconfigured and reported its skip.
Work stayed in the isolated worktree. No source edit was
made in the primary checkout and no remote ref was changed.
