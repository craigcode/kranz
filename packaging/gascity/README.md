# kranz × Gas City

Dispatch [Gas City](https://gastownhall.ai) beads as headless kranz missions.
The city routes work and holds escalations; kranz supplies mission discipline
— an approved validation contract, adversarial validators, fix cycles, and a
report — and hands back a validated branch. The city sees one opaque agent
type; kranz keeps its internal cast. No resident LLM sessions on the City
side.

## How it works

Two halves keep long-running feature execution out of Gas City's order:
`bin/kranz-dispatch` (fired on `bead.created`) atomically claims each ready
`kranz`-labelled bead, runs the bounded create/plan/approve half of
`kranz exec --enqueue --enqueue-source gascity --enqueue-external-ref <bead>`,
and leaves an approved mission in the selected rig's native `.kranz/queue/`.
The structured source receipt beside the mission is the durable City
ownership record; prompt text is never used as identity. Any legitimate
kranz dispatcher may consume the shared queue. `bin/kranz-city-worker` (a
supervised non-LLM session; see `agents/kranz-worker/`) scans registered rigs
and either drains the queue strictly serially through `kranz work --once` or
returns a terminal source-bound mission that another dispatcher drained. A slow cooldown
backstop order (`orders/kranz-dispatch-backstop.toml`, 15m) re-runs the same
`kranz-dispatch` so a dead claim in a quiet city is reclaimed and a bead
re-readied after a reopen/refine (which fires `bead.updated`, not
`bead.created`) is picked up. Dispatch flow:

1. Bead fields → ticket-shaped `mission.md`
   (`title → ## Goal`, `description → ## Context`,
   `acceptance_criteria → ## Acceptance hints`).
2. The atomic first-wins City claim marks the bead `in_progress`.
   `kranz exec --enqueue` creates and approves the mission, records its
   external producer identity before queue visibility, and the worker (or a
   sibling kranz dispatcher) later drains it with `kranz work --once`. Fix cycles remain bounded by
   `KRANZ_MAX_CYCLES` (default 1).
   Rigs that disable the scrutiny validator are REFUSED (letter-over-spirit
   risk; docs/gascity.md lesson 3) unless `KRANZ_ALLOW_UNVALIDATED=1`.
   Multi-rig cities route by bead-id prefix via `gc rig list --json`.
3. The mission's terminal state → City state:

   | mission outcome | City effect                                      |
   |-----------------|--------------------------------------------------|
   | complete        | bead closed with the mission report line          |
   | blocked         | bead marked blocked + **mail escalation to human**|
   | underspecified  | bead reopened during dispatch for refinement      |
   | failed          | bead reopened with the failure line               |

   A transient City write after terminal execution creates a
   `gascity-return-pending.json` receipt beside the mission. The next worker
   pass retries only the translation; it never re-runs the mission. Once the
   City mutation is observable, the active `enqueue-source.json` becomes the
   audit-only `enqueue-source.returned.json`.

Set `KRANZ_NATIVE_QUEUE=1` on both `kranz-dispatch` and
`kranz-city-worker`. Leaving it unset selects the previous `KRANZ_SPOOL`
implementation as an immediate rollback while the native path receives its
disposable-city operator receipt.

Steer a blocked mission from kranz's own surfaces (web dashboard, Slack
thread, CLI) — the escalation mail says where.

## Setup

1. `gc rig add <repo>` for the target checkout; give it a cheap-model
   `.kranz/config.json` if you want bounded spend.
2. Copy `orders/kranz-dispatch.toml` into your city's `orders/`, set
   `KRANZ_NATIVE_QUEUE=1` for the order and supervised worker, optionally pin
   `KRANZ_RIG_DIR`, and put the pack's `bin/` plus `kranz` and `jq` on PATH.
3. Create work: `gc bd create "<goal>" --context "..." --acceptance "..."
   --label kranz`; the `bead.created` event fires `kranz-dispatch`
   instantly. A re-readied bead (reopened then refined) and a dead claim in
   a quiet city are picked up by the 15-minute backstop order, or immediately
   by `gc order run kranz-dispatch`.

Bead-authoring rule of thumb: briefs must be self-sufficient — kranz exits 3
(underspecified) rather than guessing, and the bead bounces back for
refinement. Put constraints in `--context` and testable outcomes in
`--acceptance`.

Known spike-era deviations and the production path (multi-rig routing, sling
targets via agent-script): see `docs/gascity.md` in the kranz repo.

## Testing the bridge

`packaging/gascity/test/kranz-dispatch-roundtrip.sh` is a live-`bd` round-trip
fixture test: it stands up a throwaway `bd` store and fixture git rig under
`mktemp -d`, drives a fixture bead through status transitions and a close,
and exercises `kranz-dispatch`'s brief-field translation. It is registered
as a gate in `.kranz/merge-gates.json`.

**A merge-gate skip is silent; the mission contract is not.** When `bd` is
absent from a host's `PATH`, the round-trip script prints `ROUNDTRIP: SKIP
(bd not on PATH)` and exits 0 — a merge on that host proceeds without ever
having actually round-tripped against a live `bd`. The mission validation
contract greps for the `ROUNDTRIP: PASS` marker specifically, so the same
skip that lets a `bd`-less host merge quietly will fail the mission
contract outright. Install `bd` before relying on a green merge gate here as
evidence the bridge was proven against a live install.

`packaging/gascity/test/kranz-native-queue-selftest.sh` is the CI-safe native
queue contract. Deterministic `gc`/`kranz` stubs drive the production scripts
and prove create→enqueue→`work --once`, zero private-spool writes, and
return-receipt recovery without mission replay.

`packaging/gascity/test/check-bridge-hygiene.sh` is a static check (no `bd`
required): default mode fails if any bridge script under `bin/` still calls
`bd set-state`, or if anything under `packaging/gascity/` invokes `gc init`
or `gc stop`; `--status-map` mode fails until the translator scripts'
headers document the full bidirectional open/in_progress/blocked/closed
status mapping.

## Lease-aware claiming: shipped client-side (f-3-1, operator sign-off D-BW-2)

Atomic, first-wins claiming at dispatch time (`bd update --claim`, before any
spool write) IS shipped — see `bin/kranz-dispatch`'s header. So is the
behaviour bd 1.0.5 actually supports around a claim: idempotent re-claim by
the same actor, and a competing claim by a DIFFERENT actor failing outright
(`bd`'s own "issue already claimed by <assignee>" error, not anything the
bridge builds). Both are proven against a live `bd` by the `CLAIM: PASS`
case in `test/kranz-dispatch-roundtrip.sh`.

Also now shipped: the **client-side lease** for dead-claim recovery. `bd`
1.0.5 itself exposes no lease/TTL/heartbeat field (the executed live probe
in `docs/scoping/beads-bridge-dialect.md` §3, and an upstream mechanism is
only tracked for 1.1.0+), so the bridge keeps its own:

- **Heartbeat**: `kranz-run-bead` renews
  `${KRANZ_LEASE_DIR:-$GC_CITY/.gc/kranz-leases}/<id>.lease`
  (`<pid> <unix-ts>`) every 15s while the mission runs; a trap on EXIT /
  INT / TERM kills the renewal loop and removes the file, so a completed
  mission never leaves a dangling lease.
- **Reclaim sweep**: `kranz-dispatch --reclaim` walks claimed beads
  liveness-first — a claim whose recorded pid is ALIVE is never stolen,
  whatever its age (this is what the reverted `dbe5cf8` attempt got wrong:
  it could reap a verifiably-alive queued bead); a DEAD pid's claim is
  released (`bd update --status open --assignee ""`) and immediately
  re-claimable; a claim with NO lease file is released only when the
  bead's `updated_at` is older than `KRANZ_CLAIM_TTL` (default 120s) —
  expiry strictly as backstop. An active native source binding with a queue
  claim or post-approval mission state is durable ownership and stands at any
  age. The sole pre-queue crash shape — approved source, no queue file — is
  protected until the source receipt's own age reaches the TTL, then ages into
  the same recovery. This clock is deliberately independent of the older City
  claim timestamp because planning may legitimately outlive the TTL.

Proven against a live `bd` by the `LEASE: PASS (dead-claim recovered,
live-claim preserved)` and `LEASE-TTL: PASS (expiry only as backstop:
fresh kept, stale recovered)` cases in `test/kranz-dispatch-roundtrip.sh`.

### The history that required sign-off first

This mechanism was blocked twice on a HARD PRECONDITION in the feature
spec: an executed probe confirming bd 1.0.5 has no lease field, and an
explicit instruction not to invent a substitute. An earlier attempt
(`dbe5cf8`) shipped an `updated_at`-staleness heuristic anyway and was
reverted in full (`5a7585c`) because it could reap a bead that was
claimed-and-spooled-but-alive (queued for a worker, indistinguishable
from dead by any signal bd exposes), and no operator had accepted that
heuristic. The resolution that unblocked it is exactly the operator
sign-off the dialect doc's escalation section calls for: **D-BW-2
(accepted 2026-07-29)** approving the client-side design with the
liveness-first posture — a live pid's claim is never reaped — and TTL
only as the backstop for lease-less (ambiguous) claims, which closes the
failure mode that got the heuristic reverted. Residual window, documented
rather than hidden: on the legacy spool path, a claim between spool-write and
a worker's first heartbeat is TTL-only. The native path closes that window
with the durable source binding and explicitly recovers a source write that
crashed before queue visibility.

## Distribution

This directory is a Gas City pack (`pack.toml` `name = "kranz"`, `schema = 2`).
`gc lint packaging/gascity` is the pre-merge / pre-publish gate. GitHub CI
installs a pinned `gc` 1.3.2 and runs that lint plus the stub-safe tests in
`scripts/ci-gascity-pack.sh`.

**Schema versioning.** `schema = 2` is the City pack manifest Gas City's
loader understands (orders, agents, no `[[named_session]]`). Bump it only
when the manifest shape this pack ships would fail `gc lint` on the pinned
`gc` — never as a marketing version. Kranz's own pack-contract (`schema` 3
in `docs/pack-contract.md`: gates, prompts, checklists) is a different
document; this pack does not declare that contract.

**Registry listing (when a human publishes).** One-line description:
"Dispatch Gas City beads as headless kranz missions; the city routes and
escalates, kranz plans/validates/reports, no City-side LLM session."
Publish is `gc pack registry` / `gc pack release` by a human after Stage 1
(live supervisor+worker) has a receipt and a second operator wants to
`gc pack fetch` it. Ticket: `.kranz/tickets/gascity-pack-publish.md`. Do
not publish from CI or a mission.
