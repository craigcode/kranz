# Mission report — m-73ada5

**Goal:** Complete the live half of the Cursor CLI (`agent`) probe under authenticated conditions and finalize the `backend_cursor` route decision (direct-parser vs ACP vs defer) with an implementation brief if green, extending the evidence under docs/scoping/cursor-probe-evidence/ without touching crates/.

Branch `kranz/mission-m-73ada5` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 1h 05m 08s
**Tokens:** 51582 in / 271008 out / 21456348 cache read / 1223155 cache write
**Cost:** $73.16 actual vs $10.12–$50.59 estimated (expected $21.87)

## What shipped

### Milestone 1 — Live Cursor CLI evidence captured and self-consistent ✅

- ✅ **Re-verify binary, flag surface, and auth/model state against the updated CLI (free, read-only)** — 1 run
  - `e3a1f17` [f-1-1] re-verify Cursor CLI binary, flags, and auth against 2026.07.08-0c04a8a
- ✅ **Capture read-only text and json output-shape samples** — 1 run
  - `b88fdc2` [f-1-2] capture live Cursor CLI text/json output-shape samples
- ✅ **Capture the canonical write-capable stream-json fixture and record write posture** — 1 run
  - `7194297` [f-1-3] capture live write-capable stream-json fixture and record write posture
- ✅ **Live model matrix and permission posture** — 1 run
  - `0c91545` [f-1-4] capture live model matrix, permission posture, and finalize backend_cursor route decision
- ✅ **Fold the direct-parser route decision, brief, and Cursor auth requirement into cursor-cli-backend.md** *(fix)* — 1 run
  - `c68337a` [ms-1-fix-1-1] add revised direct-parser Decision entry to cursor-cli-backend.md
- ✅ **Fix finding: a13 — new dated Decision entry in cursor-cli-backend.md matching recommendation** *(fix)* — 1 run
- ✅ **Fix finding: a13 / ticket 3d8f93e — implementation brief must cover the ~/.cursor / CURSOR_API_KEY auth requirement** *(fix)* — 1 run
  - `2325b58` [ms-1-fix-1-3] add auth/permission pointer to implementation-brief.md

### Milestone 2 — Route decision finalized with brief if green ✅

- ✅ **Score the seven acceptance-bar items and set the recommendation** — 1 run
- ✅ **Update the Decision section and write the implementation brief if green** — 1 run
- ✅ **Fix finding: ms-2 milestone diff (a13 / f-2-2 deliverables)** *(fix)* — 1 run
  - `a86dc52` [ms-2-fix-1-1] record ms-2 as inherited/no-op milestone

## Validation history

### ms-1 round 1 — Live Cursor CLI evidence captured and self-consistent

- [critical] a13 — new dated Decision entry in cursor-cli-backend.md matching recommendation — probe-result.json.recommendation = "direct-parser" (probe-result.json:167) and route_decision.decision = "direct-parser" (line 179). But docs/scoping/cursor-cli-backend.md is NOT in the 1b32c8d..HEAD … [truncated]
- [major] a13 / ticket 3d8f93e — implementation brief must cover the ~/.cursor / CURSOR_API_KEY auth requirement — a13 requires the single-shot validator-first brief to include a permission/auth section covering the ~/.cursor / CURSOR_API_KEY requirement, and pre-milestone ticket 3d8f93e ('cursor auth does not sur… [truncated]

Disposition: 3 fix feature(s) created.

### ms-1 round 2 — Live Cursor CLI evidence captured and self-consistent

No findings.

### ms-2 round 1 — Route decision finalized with brief if green

- [major] ms-2 milestone diff (a13 / f-2-2 deliverables) — The milestone range 2325b58..HEAD is empty: `git rev-list --count 2325b58..HEAD` = 0 and `git diff --name-only 2325b58 HEAD` returns nothing. HEAD is exactly the range base (2325b58). Every ms-2 deliv… [truncated]

Disposition: 1 fix feature(s) created.

### ms-2 round 2 — Route decision finalized with brief if green

No findings.

## Contract outcomes

- ✅ **[a1]** probe-result.json remains valid JSON after the mission's updates. *(command: `jq -e . docs/scoping/cursor-probe-evidence/probe-result.json > /dev/null`)*
- ✅ **[a2]** probe-result.json.cli_version is re-verified against the updated binary and is no longer the stale 2026.04.13-a9d7fb5 value. *(command: `jq -e '.cli_version != "2026.04.13-a9d7fb5" and (.cli_version | length > 0)' docs/scoping/cursor-probe-evidence/probe-result.json`)*
- ✅ **[a3]** probe-result.json.fixture is set to a non-empty captured value (no longer null). *(command: `jq -e '.fixture != null and (.fixture | tostring | length > 0)' docs/scoping/cursor-probe-evidence/probe-result.json`)*
- ✅ **[a4]** A canonical stream-json fixture is committed as valid, non-empty JSONL. *(command: `jq -es 'length > 0' docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl`)*
- ✅ **[a5]** Text and JSON output-shape samples are committed, and the JSON sample is valid JSON. *(command: `test -s docs/scoping/cursor-probe-evidence/fixture-text.txt && jq -e . docs/scoping/cursor-probe-evidence/fixture-json.json > /dev/null`)*
- ✅ **[a6]** probe-result.json.model_matrix records the live results for auto, grok-4.5-xhigh, a gpt-5.6 model, and an invalid id. *(command: `jq -e '(.model_matrix | tostring) | test("auto") and test("grok-4.5-xhigh") and test("gpt-5.6") and test("invalid")' docs/scoping/cursor-probe-evidence/probe-result.json`)*
- ✅ **[a7]** All seven acceptance_bar items are present and each carries a non-empty scored verdict derived from the live evidence. *(command: `jq -e '(.acceptance_bar | length == 7) and ([.acceptance_bar[] | select((tostring | length) > 0)] | length == 7)' docs/scoping/cursor-probe-evidence/probe-result.json`)*
- ✅ **[a8]** probe-result.json.recommendation is one of direct-parser, acp, or defer. *(command: `jq -e '.recommendation as $r | ["direct-parser","acp","defer"] | index($r) != null' docs/scoping/cursor-probe-evidence/probe-result.json`)*
- ✅ **[a9]** Nothing under crates/ is modified relative to the mission base commit. *(command: `test -z "$(git diff --name-only $KRANZ_BASE_SHA -- crates/)"`)*
- ✅ **[a10]** No committed evidence file leaks an email address or an operator home path (redaction was applied). *(command: `! grep -rEn '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}|/Users/[A-Za-z]' docs/scoping/cursor-probe-evidence/`)*
- ✅ **[a11]** The decision doc records the Cursor auth requirement (~/.cursor does not survive HOME relocation; needs inherited ~/.cursor or CURSOR_API_KEY). *(command: `grep -qE 'CURSOR_API_KEY|\.cursor' docs/scoping/cursor-cli-backend.md`)*
- ✅ **[a12]** The committed stream-json fixture contains at least one assistant-text event, one tool-use event, one tool-result event, and a terminal event carrying token usage and/or cost. *(agent judgement)*
- ✅ **[a13]** cursor-cli-backend.md has a new dated Decision entry whose route matches probe-result.json.recommendation, retains the 2026-07-09 defer entry as history, and — only if the route is not defer — includes a scoped single-shot validator-first implementation brief mirroring the Codex mapping doc (with its permission/auth section covering the ~/.cursor / CURSOR_API_KEY requirement). *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
