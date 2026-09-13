---
state: done
state-note: Done: backend_cursor.rs — direct stream-json parser per the decided route; env-cleared seeded spawn, permission mapping (ask/force, no --sandbox), usage verbatim incl. cache tokens, additive BackendKind::Cursor with readiness model-availability probe. backend_cursor filter: 25 green; full gates green. Unblocks agent-hooks-status-signals.
title: "backend_cursor: direct stream-json parser per the decided route"
priority: 2
schedule: once
---

# backend_cursor: direct stream-json parser per the decided route

The route is DECIDED: `direct-parser`
(`docs/scoping/cursor-cli-backend.md` — the post-auth probe's
acceptance-bar table, `probe-result.json.recommendation` and
`route_decision.decision` agree), with committed wire evidence at
`docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl`. What
does not exist is the backend itself — `BackendKind` has no cursor
variant. This is the implementation ticket; `agent-hooks-status-signals`
is its first consumer (hook-derived status signals need a backend to
hang from).

## Goal

An `AgentBackend` implementation driving the Cursor CLI
(`agent --print --output-format stream-json`): spawn, supervise, and
record sessions, streaming text/tool calls/results into the event log
with the same discipline as the other CLI backends.

## What to build (per the decided route's evidence)

1. `crates/engine/src/backend_cursor.rs` modeled on `backend_codex.rs` /
   `backend_kimi.rs`: env-cleared spawn (`agent_env::sanitized_child_env`
   + cursor's auth channel — check what `agent login` persists and what
   the seed needs, mirroring `seed_kimi_scratch_home`), bounded stream
   reads, process-tree kill, permission mapping preserving the
   no-push/no-publish/no-main-write invariants (the `--mode ask|plan`,
   `--force`/`--yolo`, `--sandbox` mapping — the probe's item 6).
2. The parser against the committed fixture: terminal text stitched into
   Result.text, tool-use/result events, usage/cost fields (absent
   recorded as absent, never fabricated), model-availability failures
   deterministic (probe items 1-3, 5).
3. Worktree cwd discipline via `--workspace`/`--worktree` flags (probe
   item 4), verified, not assumed.
4. Config wiring additive: `BackendKind::Cursor` (serde idiom like the
   acp addition), all exhaustive matches updated, config validation,
   readiness probe, preflight arm. NOT a default anywhere; opt-in only.
5. The fixture test: the committed
   `docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl` drives
   a parser fixture test in the codex/kimi fixture idiom.

## Test gate

- `cargo test --workspace backend_cursor 2>&1 | grep -qE 'test result: ok. [1-9]'`
  — full session against a mock `agent` binary (house stub idiom):
  spawn → tool calls → result → exit with ordered events; absent
  usage/cost stays absent; a permission-refusal case; kill mid-session
  leaves a resumable log.
- Workspace gates green, bare exit codes, never piped.

## Out of scope

The IDE lane, hook-derived status signals (`agent-hooks-status-signals`
builds on this), ACP variants of cursor (the route was decided direct).
