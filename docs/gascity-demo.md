# Gas City × kranz demo runbook

Status: **demo-ready** (2026-08-10). This doc is the operator runbook for
showing kranz as a Gas City rig type at a training course or any live-city
session. It covers a disposable, single-operator city so you do not risk a
production city.

For the design record, see [gascity-citizenship.md](gascity-citizenship.md).
For spike-era lessons, see [gascity.md](gascity.md).

## What is demo-ready

The `packaging/gascity/` pack now:

- Dispatches instantly on `bead.created` events instead of polling every 5
  minutes, with a 15-minute cooldown backstop order
  (`kranz-dispatch-backstop`) that reclaims dead claims in a quiet city and
  picks up beads re-readied after a reopen/refine (which fire `bead.updated`,
  not `bead.created`).
- Emits `kranz.mission.started`, `kranz.mission.complete`, and
  `kranz.mission.blocked` City events from `kranz-run-bead`.
- Keeps the existing exit-code contract, lease/reclaim logic, and scrutiny
  floor unchanged.

Human-gated Stage 1 (live-city validation of the supervised worker wiring) is
still the operator's responsibility — that is what this runbook walks through.

## Prerequisites

On the demo machine:

- `gc` (Gas City) installed, on PATH, version 1.3.2 or compatible.
- `bd` installed, on PATH (the bead CLI).
- `kranz` installed on PATH, built from this repo.
- `jq` installed (the bridge scripts use it).
- A **rig**: a git checkout where kranz missions will run. The rig must:
  - be `kranz init`-initialized (has `.kranz/` with `config.json`, merge gates, etc.);
  - have working backend credentials in the environment that will run the
    worker (the same env the worker process inherits — kranz workers spawn
    env-cleared sessions, so credentials must come from the contract/operator
    env, not a stray shell);
  - keep the scrutiny validator enabled (`.skipScrutiny != true`), or the
    dispatch floor refuses the bead.

## Demo setup

```bash
# 1. Create a disposable city WITHOUT starting a supervisor.
#    --no-start avoids the live billed mayor session until you explicitly start.
gc init --no-start --template minimal --default-provider codex /tmp/kranz-demo-city
cd /tmp/kranz-demo-city

# 2. Import the kranz pack. For a pack inside a git worktree (the kranz repo
#    has uncommitted runtime dirs), edit pack.toml directly — `gc import add`
#    promotes only a clean worktree to a file:// source.
cat >> pack.toml <<'EOF'

[imports.kranz]
source = "/path/to/kranz/repo/packaging/gascity"
EOF
# Actually install the import (check only VALIDATES; it exits 0 even when the
# import has issues, so a green check alone is NOT proof the pack is wired in).
gc import install
gc import check            # resolves the path import; must print OK
# Confirm the pack's order and agent actually landed:
gc order list              # expect kranz-dispatch AND kranz-dispatch-backstop
gc agent list              # expect: kranz.kranz-worker

# 3. Register the rig the worker will run missions in.
#    Multi-rig cities: dispatch routes bead -> rig by the bead id prefix via
#    `gc rig list --json`, so the rig's prefix must match the beads you create.
#    Single-rig cities: dispatch also honours KRANZ_RIG_DIR, but only if the
#    variable is visible to the ORDER's process (the supervisor's env, not
#    your login shell) — for the fallback manual worker below it just works.
gc rig add /path/to/rig --name demo

# 4. Register and start the city (spawns the live mayor session — budget it).
gc start /tmp/kranz-demo-city

# 5. Confirm the kranz worker agent and the event order are live.
gc agent list              # expect: kranz.kranz-worker  active
gc order list              # expect: kranz-dispatch  exec  event  bead.created
                           #     and: kranz-dispatch-backstop  exec  cooldown  15m
gc status                  # kranz-worker should appear as a long-running agent
```

If the supervisor does not start the worker itself, run it manually as a
fallback (the supervisor's health patrol won't restart it in that case). The
worker needs the pack's `bin/` on PATH (for `kranz-run-bead` and
`kranz-dispatch`) alongside `kranz`, `gc`, `bd`, and `jq`:

```bash
export GC_CITY=/tmp/kranz-demo-city
export KRANZ_RIG_DIR=/path/to/rig
export PATH="/path/to/kranz/repo/packaging/gascity/bin:$PATH"
kranz-city-worker &
WORKER_PID=$!
```

## Running the demo

Create a bead labeled `kranz`. The bead title becomes the mission goal; the
description becomes context; acceptance criteria become acceptance hints.

```bash
gc bd create 'Add a one-line greeting to README.md' \
  --labels kranz \
  --description 'The README in the rig checkout has no greeting. Add "Hello, Gas City!" on its own line.' \
  --acceptance 'README.md contains the literal string "Hello, Gas City!"'
```

The `kranz-dispatch` order fires on the `bead.created` event, claims the bead,
and spools a mission brief for the worker. Watch City events (note: the
follow command is `gc events`, plural):

```bash
gc events --follow
```

You should see, in order:

1. `kranz.mission.started` when the worker picks up the spool entry.
2. `kranz.mission.complete` if the mission exits 0.
3. `kranz.mission.blocked` if the mission exits 2 and escalates to a human.

Check the bead status:

```bash
gc bd show <bead-id>
```

On success it is `closed` with a `kranz mission COMPLETE` reason. On block it
is `blocked` with a comment naming the mission id and an escalation mail.

## Escalation path (what blocks look like)

Know the exit-code contract before demoing failures — it is not symmetric:

- **exit 0 → closed** (`kranz.mission.complete` emitted).
- **exit 2 → blocked** + `gc mail send human --notify` escalation
  (`kranz.mission.blocked` emitted). This is the "parked for a human" path —
  e.g. a mission that hits a grant decision it cannot resolve autonomously.
- **exit 3 → reopened** with an UNDERSPECIFIED comment. A vague brief like
  "make it better" takes this path, *not* the blocked path — it is the
  brief-refusal outcome, not an escalation.
- **exit 1/other → reopened** (mission failed; retry or refine).

A reopened bead becomes `ready` again, so the 15-minute backstop order will
re-dispatch it on its next tick (there is no instant re-dispatch on reopen —
that gap is deliberate, it is the human's window to refine an underspecified
brief before the retry). To stop an underspecified bead from retry-looping on
the backstop cadence, refine it or close it; to re-dispatch immediately, run
`gc order run kranz-dispatch`.

So do not promise the room that an ambiguous brief will "escalate to a
human" — it will come back as `open` with a refine-this-brief comment. If you
want the escalation mail on screen, give the mission a brief that is specific
but requires a human decision kranz is not allowed to make alone.

## Cleanup

Do not leave the mayor session or tmux server running:

```bash
# Stop the manually-started worker, if you used the fallback above.
kill "$WORKER_PID" 2>/dev/null || true

# Stop and unregister the city.
gc stop /tmp/kranz-demo-city
gc unregister /tmp/kranz-demo-city

# Confirm no orphaned tmux server (the city name is the directory basename
# unless you passed --name at init).
tmux -L kranz-demo-city ls 2>/dev/null && tmux -L kranz-demo-city kill-server

# Remove the disposable city.
rm -rf /tmp/kranz-demo-city
```

## Verification without a live city

If you only want to confirm the pack is still valid before travelling (run
from the kranz repo root):

```bash
# Must exit 0.
gc lint packaging/gascity

# Stub tests for the new event emissions and order config.
packaging/gascity/test/order-trigger-event.sh
packaging/gascity/test/kranz-run-bead-events.sh

# Full live-bd round-trip regression.
packaging/gascity/test/kranz-dispatch-roundtrip.sh
```
