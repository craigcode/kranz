---
title: Slice 3 — kranz knowledge refresh reports drifted notes
priority: 2
schedule: once
state: done
state-note: CLI + engine report landed 2026-08-17 and was hardened fail-closed 2026-08-18 (invalid metadata/citations and failed read/parse/Git probes are check-needed; no note rewrites or command execution). Dashboard/Slack surface remains out of scope.
---

## Goal

Add `kranz knowledge refresh`: a report-only drift check over
`docs/knowledge/`. When a path in a note's `verified_against` is missing or
has commits after `last_verified`, the note is reported as check-needed. An
unusable date, outside-repo citation, unreadable/malformed note, or failed
filesystem/Git probe is also check-needed: inability to prove freshness is
never `ok`. The command never rewrites a note or changes planning injection.

## Context

`docs/scoping/repo-knowledge-store.md` slices 1–2 shipped (vault +
≤4 KiB ranked planning injection). Slice 3 is the remaining done-when:
"When a code path named in a note's `verified_against` changes,
`kranz knowledge refresh` reports the note as check-needed or proposes
a patch; it never silently injects that stale fact into a later plan."

Injection already excludes `freshness: stale` and notes with an empty
`verified_against` (`crates/engine/src/knowledge.rs`). This ticket is
the detector, not a second injection path.

Stay inside the positioning freeze: this is provenance/freshness
evidence, not a retrieval or context-management engine. No vector
search. No automatic commits to `docs/knowledge/`. No dashboard/Slack
surface in this slice (scoping said dashboard begins in slice 3; defer
it until the CLI report exists).

Open question #3 in the scoping doc (arbitrary shell in
`verified_against`): **commands are not executed.** Classify them and
report `command-skipped`. Path probes and git history only. A later
ticket may allowlist specific gate commands; do not invent that here.

## Scoping answers

- Rewrite notes? No. Report only.
- Execute `verified_against` commands? No. Skip and name them.
- Dashboard/Slack stale-note surface? Out of scope for this ticket.
- Git probe: `git log -1 --since=<last_verified>T23:59:59 --format=%H --
  <path>` nonempty ⇒ path drifted (exclusive of the verification calendar
  day). Missing path ⇒ path-missing. Untracked path that exists on disk
  is still a citation; treat as present unless HEAD history says it
  changed after `last_verified`.

## Acceptance hints

- `kranz knowledge refresh` exits 0 when every injectable note's path
  citations exist and have no commits after `last_verified`; exits 1
  when any note is check-needed (so CI can gate it later).
- Report names note path, title, and actionable verdicts
  (`ok` / `already-stale` / `unverified` / `invalid-metadata` /
  `invalid-citation` / `path-missing` / `path-drifted` /
  `command-skipped` / `probe-failed`).
- A fixture note whose `verified_against` path was edited after
  `last_verified` is `path-drifted`. A fixture whose path was deleted
  is `path-missing`. A `freshness: stale` note is reported
  `already-stale` and does not fail the run by itself.
- No note file is rewritten. Planning injection is unchanged; after a
  `path-drifted` report, the operator refreshes the note or marks it `stale`
  before relying on a later plan.
- Anti-vacuity: new tests match a unique filter (`knowledge_refresh`),
  not a substring that already passes.
