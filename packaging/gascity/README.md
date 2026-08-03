# kranz × Gas City

Dispatch [Gas City](https://gastownhall.ai) beads as headless kranz missions.
The city routes work and holds escalations; kranz supplies mission discipline
— an approved validation contract, adversarial validators, fix cycles, and a
report — and hands back a validated branch. The city sees one opaque agent
type; kranz keeps its internal cast. No resident LLM sessions on the City
side.

## How it works

Two halves, split by Gas City's order-exec deadline: `bin/kranz-dispatch`
(the order, on a cooldown) marks ready `kranz`-labeled beads `in_progress`
(direct status write, not an atomic claim) and spools
mission briefs; `bin/kranz-city-worker` (a supervised long-running session —
see `agents/kranz-worker/`) drains the spool strictly serially, running each
brief and mapping the exit code back into City state. Dispatch flow:

1. Bead fields → ticket-shaped `mission.md`
   (`title → ## Goal`, `description → ## Context`,
   `acceptance_criteria → ## Acceptance hints`).
2. Bead marked `in_progress` by a direct status write (not an atomic claim)
   and spooled; the worker runs
   `kranz exec -f mission.md` in the rig checkout — fully autonomous,
   auto-approved plan, bounded fix cycles (`KRANZ_MAX_CYCLES`, default 1).
   Rigs that disable the scrutiny validator are REFUSED (letter-over-spirit
   risk; docs/gascity.md lesson 3) unless `KRANZ_ALLOW_UNVALIDATED=1`.
   Multi-rig cities route by bead-id prefix via `gc rig list --json`.
3. Exit code → City state:

   | exit | meaning        | City effect                                      |
   |------|----------------|--------------------------------------------------|
   | 0    | complete       | bead closed with the mission report line          |
   | 2    | blocked        | bead marked blocked + **mail escalation to human**|
   | 3    | underspecified | bead reopened with a refine-the-brief comment     |
   | 1    | failed         | bead reopened with the failure line               |

Steer a blocked mission from kranz's own surfaces (web dashboard, Slack
thread, CLI) — the escalation mail says where.

## Setup

1. `gc rig add <repo>` for the target checkout; give it a cheap-model
   `.kranz/config.json` if you want bounded spend.
2. Copy `orders/kranz-dispatch.toml` into your city's `orders/`, set
   `KRANZ_RIG_DIR`, put `bin/kranz-dispatch` (and `kranz`, `jq`) on PATH.
3. Create work: `gc bd create "<goal>" --context "..." --acceptance "..."
   --label kranz`, then `gc order run kranz-dispatch` (or let the cooldown
   trigger fire).

Bead-authoring rule of thumb: briefs must be self-sufficient — kranz exits 3
(underspecified) rather than guessing, and the bead bounces back for
refinement. Put constraints in `--context` and testable outcomes in
`--acceptance`.

Known spike-era deviations and the production path (event triggers, sling
targets via agent-script, multi-rig routing): see `docs/gascity.md` in the
kranz repo.

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
  expiry strictly as backstop.

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
rather than hidden: a claim between spool-write and a worker's first
heartbeat is TTL-only.
