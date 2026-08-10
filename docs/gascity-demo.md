# Gas City × kranz demo runbook

Status: **demo-ready** (2026-08-10). This doc is the operator runbook for
showing kranz as a Gas City rig type at a training course or any live-city
session. It covers a disposable, single-operator city so you do not risk a
production city.

For the design record, see [gascity-citizenship.md](gascity-citizenship.md).
For spike-era lessons, see [gascity.md](gascity.md).

## What is demo-ready

The `packaging/gascity/` pack now:

- Dispatches on `bead.created` events instead of polling every 5 minutes.
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
- `kranz` installed or available on PATH, built from this repo.
- `jq` installed (the bridge scripts use it).
- The pack at `packaging/gascity/` is visible to the test city (copy or
  symlink it into the city's pack search path after init).

## Demo setup

```bash
# 1. Create a disposable city WITHOUT starting a supervisor.
#    --no-start avoids the live billed mayor session until you explicitly start.
gc init --no-start --template minimal --default-provider codex /tmp/kranz-demo-city
cd /tmp/kranz-demo-city

# 2. Make the kranz pack visible to this city.
#    Option A: copy the pack into the city's pack path.
cp -R /path/to/kranz/repo/packaging/gascity ./packs/kranz
#    Option B: if `gc import` supports a local path, use that instead.

# 3. Register the city with the machine-wide supervisor.
gc register /tmp/kranz-demo-city

# 4. Start the city supervisor (this spawns a live mayor session).
gc start

# 5. Add the kranz worker agent if it is not already registered.
gc agent add kranz-worker

# 6. Start the long-lived kranz worker that drains the spool.
#    It runs until you Ctrl-C it or stop the city.
kranz-city-worker &
WORKER_PID=$!
```

## Running the demo

Create a bead labeled `kranz`. The bead title becomes the mission goal; the
description becomes context; acceptance criteria become acceptance hints.

```bash
# Create a small, safe bead that kranz can complete in the rig.
gc bd create 'Add a one-line greeting to README.md' \
  --labels kranz \
  --description 'The README in the rig checkout has no greeting. Add "Hello, Gas City!" on its own line.' \
  --acceptance 'README.md contains the literal string "Hello, Gas City!"'
```

The order `kranz-dispatch` fires on the `bead.created` event, claims the bead,
and spools a mission brief for the worker. Watch City events:

```bash
gc event list --follow
```

You should see:

1. `kranz.mission.started` when the worker picks up the spool entry.
2. `kranz.mission.complete` if the mission exits 0.
3. `kranz.mission.blocked` if the mission exits 2 and escalates to a human.

Check the bead status:

```bash
gc bd show <bead-id>
```

On success it is `closed` with a `kranz mission COMPLETE` reason. On block it
is `blocked` with a comment naming the mission id and an escalation mail.

## Forced escalation demo

To show the blocked/escalation path, create a bead whose brief is intentionally
ambiguous:

```bash
gc bd create 'Make it better' \
  --labels kranz \
  --description 'Improve the project.' \
  --acceptance 'It is better.'
```

With `skipScrutiny=false` (the default), kranz should refuse to autonomously
satisfy the underspecified brief and exit 2, leaving the bead `blocked` with a
human escalation.

## Cleanup

Do not leave the mayor session or tmux server running:

```bash
# Stop the worker you started.
kill "$WORKER_PID" 2>/dev/null || true

# Stop and unregister the city.
gc stop /tmp/kranz-demo-city
gc unregister /tmp/kranz-demo-city

# Confirm no orphaned tmux server.
tmux -L kranz-demo-city ls 2>/dev/null && tmux -L kranz-demo-city kill-server

# Remove the disposable city.
rm -rf /tmp/kranz-demo-city
```

## Verification without a live city

If you only want to confirm the pack is still valid before travelling:

```bash
# Must exit 0.
gc lint packaging/gascity

# Stub tests for the new event emissions and order config.
packaging/gascity/test/order-trigger-event.sh
packaging/gascity/test/kranz-run-bead-events.sh

# Full live-bd round-trip regression.
packaging/gascity/test/kranz-dispatch-roundtrip.sh
```
