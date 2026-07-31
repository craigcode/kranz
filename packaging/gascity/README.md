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

## Lease-aware claiming: dead-claim recovery blocked pending operator sign-off (finding [f-3-1])

Atomic, first-wins claiming at dispatch time (`bd update --claim`, before any
spool write) IS shipped — see `bin/kranz-dispatch`'s header. So is the
behaviour bd 1.0.5 actually supports around a claim: idempotent re-claim by
the same actor, and a competing claim by a DIFFERENT actor failing outright
(`bd`'s own "issue already claimed by <assignee>" error, not anything the
bridge builds). Both are proven against a live `bd` by the `CLAIM: PASS`
case in `test/kranz-dispatch-roundtrip.sh`.

What is NOT shipped is the heartbeat/TTL layer that would let a dead claim's
holder be distinguished from a live one and recovered automatically.

`docs/scoping/beads-bridge-dialect.md` §3 records an executed, live probe
(not a `--help` scan) against the installed `bd` 1.0.5 that confirms no
`lease_expires_at`, `heartbeat_at`, or any other lease/TTL/heartbeat field or
flag exists anywhere in its data model. This milestone's own feature spec
carries a HARD PRECONDITION for exactly that finding: STOP, do not invent a
substitute (no emulating a lease via comments, metadata, sentinel files, or
status abuse), and report the block rather than fabricate a mechanism.

An earlier attempt (`dbe5cf8`) built a heartbeat + `updated_at`-staleness
backstop anyway, reasoning that the same feature spec's numbered steps
describe building exactly that kind of mechanism. It was reverted in full
(`5a7585c`) because (a) it could reap a bead that is claimed and spooled but
not yet picked up by `kranz-city-worker` — verifiably alive in the sense
that a mission is still queued for it, but indistinguishable from dead by
any signal `bd` 1.0.5 exposes — and (b) no operator has signed off on that
staleness heuristic as the alternate mechanism the dialect doc's escalation
section calls for.

That tension — a HARD PRECONDITION to stop, inside a feature spec whose own
numbered steps assume the mechanism gets built — has now been hit twice in
independent fix cycles with the same resolution (stay honest, don't ship the
heuristic). Resolving it for real needs one of: an operator-approved upgrade
to `bd` 1.1.0+ (where `docs/scoping/beads-workstore.md:66` records
`lease_expires_at`/`heartbeat_at` as verified against source), or explicit
operator sign-off on the `updated_at`-staleness mechanism and its documented
residual exposure window. Until either happens, `bin/kranz-dispatch` and
`bin/kranz-run-bead` stay in the honest NOT-YET-IMPLEMENTED state their
headers already document for dead-claim recovery specifically, and
`test/kranz-dispatch-roundtrip.sh` keeps printing `LEASE: SKIP (dead-claim
recovery not implemented — no lease/TTL/heartbeat signal in bd 1.0.5;
awaiting operator decision. Idempotent re-claim and competing-claim-fails
are proven by the CLAIM case above.)` rather than a fabricated `LEASE:
PASS`.
