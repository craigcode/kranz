---
title: parse_user flags any tool result containing "hook" as denied — validator sessions abort
priority: 1
schedule: once
state: done
state-note: Fixed in 1a87737 — 'hook' now requires a block/deny/reject phrase; structured refusals require is_error.
---

## Goal
Stop classifying claude-backend tool results as denied by unconditional substring match. parse_user (crates/engine/src/backend_claude.rs:605-609) marks a tool result denied whenever its lowercased text contains "hook", so a routine directory listing of crates/engine/tests/ — which contains hooks_test.rs — parks a deny-by-default grant that times out and aborts the session. Four validator runs aborted this way on m-eee81f alone (event seq 1859, 4072, 6091, 6205), each costing a full validator re-run.

## Context

Found via m-eee81f (flight-rules-pack-contract): its orchestrator diagnosed
the aborts after two validator sessions died and routed around them with
validator guidance ("never list crates/engine/tests/"). The substring
heuristic exists because a false NEGATIVE silently aborts the session with
deniedToolResults=0 (comment at backend_claude.rs:585-587) — the fix must
tighten the positive test (e.g. match Claude Code's actual permission-denial
phrasing, or require is_error plus a denial phrase), not just drop "hook" and
reintroduce the miss. The same unconditional substrings ("requires approval",
"contains expansion", "output redirection"+ "blocked") deserve the same
scrutiny for false-positive shapes.

## Scoping answers

## Acceptance hints

- A tool result whose text merely mentions "hook" (e.g. a listing containing
  hooks_test.rs, or this very ticket's prose) is NOT classified denied.
- A genuine permission-denied tool result (the shape that motivated the
  heuristic) is still classified denied — regression test with a captured
  real denial payload, not a synthetic string.
- Validator sessions no longer abort when a worker/validator lists
  crates/engine/tests/: regression test replaying exactly that tool result.
- Anti-vacuity: test-name filters must match only the new tests.
