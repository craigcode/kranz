---
title: "Beads bridge: translator correctness (D-BW-2 brief 1)"
priority: 2
schedule: once
---

# Beads bridge: translator correctness

Decision source: `docs/scoping/beads-workstore.md` D-BW-2 (accepted
2026-07-29; operator wants beads/Gas City interop this week). Sister
ticket: `beads-bridge-provenance-return-path`.

## Problem

The Gas City spike (`packaging/gascity/bin/kranz-dispatch`,
`kranz-run-bead`) translates between kranz tickets and beads issues, but
its claim/status machinery is stale: it uses `bd set-state blocked`, which
no longer exists (the fallback `bd update --status blocked` is the correct
form), and its claim path has no lease semantics — a dead dispatcher holds
work forever. If this drift appeared in a 200-line spike, the unhardened
translator cannot be the interop path.

## Design (from the decision doc, locked)

1. **Status verbs:** replace every `set-state` call with `bd update
   --status`; the status map is `open/in_progress/blocked/closed` ↔ kranz
   Queued / Running / Blocked-report / Done (both directions, explicit
   table in the script's header comment).
2. **Lease-aware claims:** claim via `bd update --claim` with a heartbeat
   or short TTL so a dead dispatcher's claim expires and the work becomes
   claimable again (mirror the kranz queue's liveness-over-age posture:
   liveness first, expiry only as the backstop).
3. **Acceptance-criteria array unwrapping:** keep the spike's existing
   behaviour, now covered by test.
4. **Round-trip fixture test:** script-level test against a LIVE `bd`
   binary (skip with a clear note when `bd` is absent): create → claim →
   status transitions both directions → close; fixture repo, no global
   state.

## Test gate

- Round-trip fixture test passes on a host with `bd` (and skips cleanly
  where absent): `bash packaging/gascity/test/kranz-dispatch-roundtrip.sh`
- No `set-state` remains: `! grep -R 'set-state' packaging/gascity/bin/`
- Workspace gates green: `cargo test --workspace`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`,
  `cargo build --workspace` (bare exit codes, never piped).
