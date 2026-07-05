# kranz × Gas City

Dispatch [Gas City](https://gastownhall.ai) beads as headless kranz missions.
The city routes work and holds escalations; kranz supplies mission discipline
— an approved validation contract, adversarial validators, fix cycles, and a
report — and hands back a validated branch. The city sees one opaque agent
type; kranz keeps its internal cast. No resident LLM sessions on the City
side.

## How it works

`bin/kranz-dispatch` (invoked by the `kranz-dispatch` order, manually or on a
cooldown) drains READY beads labeled `kranz`:

1. Bead fields → ticket-shaped `mission.md`
   (`title → ## Goal`, `description → ## Context`,
   `acceptance_criteria → ## Acceptance hints`).
2. Bead claimed (`in_progress`), then `kranz exec -f mission.md` runs in the
   rig checkout — fully autonomous, auto-approved plan, bounded fix cycles
   (`KRANZ_MAX_CYCLES`, default 1).
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
