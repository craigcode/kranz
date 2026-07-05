# kranz × Gas City

Dispatch [Gas City](https://gastownhall.ai) beads as headless kranz missions.
The city routes work and holds escalations; kranz supplies mission discipline
— an approved validation contract, adversarial validators, fix cycles, and a
report — and hands back a validated branch. The city sees one opaque agent
type; kranz keeps its internal cast. No resident LLM sessions on the City
side.

## How it works

Two halves, split by Gas City's order-exec deadline: `bin/kranz-dispatch`
(the order, on a cooldown) CLAIMS ready `kranz`-labeled beads and spools
mission briefs; `bin/kranz-city-worker` (a supervised long-running session —
see `agents/kranz-worker/`) drains the spool strictly serially, running each
brief and mapping the exit code back into City state. Dispatch flow:

1. Bead fields → ticket-shaped `mission.md`
   (`title → ## Goal`, `description → ## Context`,
   `acceptance_criteria → ## Acceptance hints`).
2. Bead claimed (`in_progress`) and spooled; the worker runs
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
