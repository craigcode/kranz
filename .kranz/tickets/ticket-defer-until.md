---
state: done
state-note: Done at 7659358: defer-until frontmatter, kranz ticket ready [--include-deferred], shared approve gate refuses not-yet-ready naming the time (--force overrides), malformed timestamp hard-errors naming the ticket. 8 tests. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
title: "Ticket defer_until (D-BW-3, adopted from beads)"
priority: 3
schedule: once
---

# Ticket defer_until

Decision source: `docs/scoping/beads-workstore.md` D-BW-3 (accepted
2026-07-29): beads' `defer_until` is a strictly better `schedule` for
one-shot tickets — hidden from ready until a time, then present.

## Problem

One-shot tickets that should not run yet currently have no honest home:
leave them out of the queue (and forget them), or queue them and have the
dispatcher pick them up prematurely. A deferral field — present but not
ready until a timestamp — is the correct primitive, and beads' version is
the proven shape.

## Design (locked)

1. **Frontmatter:** optional `defer-until: <RFC 3339 timestamp>` on
   `.kranz/tickets/<slug>.md` (kebab-case, matching the existing
   frontmatter style; additive — absent means ready now).
2. **Readiness:** the ready/queue listing EXCLUDES deferred tickets until
   the timestamp passes; `kranz ticket ready` gains a `--include-deferred`
   flag that shows them with their defer time (operator visibility without
   polluting the machine path).
3. **Queue admission:** `kranz ticket queue <slug>` on a not-yet-ready
   deferred ticket refuses with the defer time named (fail closed with the
   reason, not a silent skip); an explicit `--force` overrides.
4. **No scheduler machinery:** deferral is evaluated at listing/admission
   time against the clock — no daemon, no timer events (a deferred ticket
   simply becomes listable on its day).

## Test gate

- `cargo test --workspace defer_until 2>&1 | grep -qE 'test result: ok. [1-9]'`
  covering: past timestamp is ready, future is excluded, --include-deferred
  shows with time, queue refuses with the time named, --force overrides,
  malformed timestamp rejected at parse with the ticket named.
- Workspace gates green (bare exit codes, never piped).
