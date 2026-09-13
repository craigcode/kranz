# Mission plan — m-73ada5

**Goal:** Complete the live half of the Cursor CLI (`agent`) probe under authenticated conditions and finalize the `backend_cursor` route decision (direct-parser vs ACP vs defer) with an implementation brief if green, extending the evidence under docs/scoping/cursor-probe-evidence/ without touching crates/.

Branch `kranz/mission-m-73ada5` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$10.12 – $50.59** (expected ~$21.87). Rough estimate — live usage is authoritative; based on 39 completed mission(s).

## Considered alternatives

**Chosen approach:** Split the fragile billed capture into small bounded features (each under the 50-turn ceiling), each fronted by an identical auth GATE that self-checks `agent status`/`agent models`, falls back to operator-staged redacted raw captures at .kranz/cursor-raw/, and STOPS with result:blocked rather than fabricating. This is robust to the unresolved question of whether these workers get a Cursor-usable env (inherited ~/.cursor or CURSOR_API_KEY) or a relocated HOME that drops ~/.cursor, and it lets the orchestrator escalate cleanly instead of merging a dead $0 capture.

Rejected shapes:
- **One mega live-capture feature doing all read-only, write-capable, matrix, and permission runs in a single worker.** — Realistically exceeds the 50-turn budget, and a single auth fumble or redaction slip sinks the entire capture with no partial progress.
- **Operator-only out-of-band capture (Craig runs all agent prompts, workers merely commit the raw output), the pure m-7820b9 evidence-merge shape.** — Correct and safe but wastes worker delegation if the worker env is in fact Cursor-usable, and forces an idle operator round-trip that the Q1-was-unresolved-at-plan-time situation does not warrant hardcoding.
- **Assume workers always have live Cursor auth and drop the gate/no-fabricate STOP.** — Under HOME relocation the worker silently produces the m-66aff8 dead signature ($0, empty diff, 'Not logged in'), re-blocking exactly like m-7820b9 while looking superficially complete.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** probe-result.json remains valid JSON after the mission's updates. 
  `jq -e . docs/scoping/cursor-probe-evidence/probe-result.json > /dev/null`
- **[a2]** probe-result.json.cli_version is re-verified against the updated binary and is no longer the stale 2026.04.13-a9d7fb5 value. 
  `jq -e '.cli_version != "2026.04.13-a9d7fb5" and (.cli_version | length > 0)' docs/scoping/cursor-probe-evidence/probe-result.json`
- **[a3]** probe-result.json.fixture is set to a non-empty captured value (no longer null). 
  `jq -e '.fixture != null and (.fixture | tostring | length > 0)' docs/scoping/cursor-probe-evidence/probe-result.json`
- **[a4]** A canonical stream-json fixture is committed as valid, non-empty JSONL. 
  `jq -es 'length > 0' docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl`
- **[a5]** Text and JSON output-shape samples are committed, and the JSON sample is valid JSON. 
  `test -s docs/scoping/cursor-probe-evidence/fixture-text.txt && jq -e . docs/scoping/cursor-probe-evidence/fixture-json.json > /dev/null`
- **[a6]** probe-result.json.model_matrix records the live results for auto, grok-4.5-xhigh, a gpt-5.6 model, and an invalid id. 
  `jq -e '(.model_matrix | tostring) | test("auto") and test("grok-4.5-xhigh") and test("gpt-5.6") and test("invalid")' docs/scoping/cursor-probe-evidence/probe-result.json`
- **[a7]** All seven acceptance_bar items are present and each carries a non-empty scored verdict derived from the live evidence. 
  `jq -e '(.acceptance_bar | length == 7) and ([.acceptance_bar[] | select((tostring | length) > 0)] | length == 7)' docs/scoping/cursor-probe-evidence/probe-result.json`
- **[a8]** probe-result.json.recommendation is one of direct-parser, acp, or defer. 
  `jq -e '.recommendation as $r | ["direct-parser","acp","defer"] | index($r) != null' docs/scoping/cursor-probe-evidence/probe-result.json`
- **[a9]** Nothing under crates/ is modified relative to the mission base commit. 
  `test -z "$(git diff --name-only $KRANZ_BASE_SHA -- crates/)"`
- **[a10]** No committed evidence file leaks an email address or an operator home path (redaction was applied). 
  `! grep -rEn '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}|/Users/[A-Za-z]' docs/scoping/cursor-probe-evidence/`
- **[a11]** The decision doc records the Cursor auth requirement (~/.cursor does not survive HOME relocation; needs inherited ~/.cursor or CURSOR_API_KEY). 
  `grep -qE 'CURSOR_API_KEY|\.cursor' docs/scoping/cursor-cli-backend.md`
- **[a12]** The committed stream-json fixture contains at least one assistant-text event, one tool-use event, one tool-result event, and a terminal event carrying token usage and/or cost. *(agent judgement)*
- **[a13]** cursor-cli-backend.md has a new dated Decision entry whose route matches probe-result.json.recommendation, retains the 2026-07-09 defer entry as history, and — only if the route is not defer — includes a scoped single-shot validator-first implementation brief mirroring the Codex mapping doc (with its permission/auth section covering the ~/.cursor / CURSOR_API_KEY requirement). *(agent judgement)*

## Milestone 1 — Live Cursor CLI evidence captured and self-consistent

### 1.1 Re-verify binary, flag surface, and auth/model state against the updated CLI (free, read-only)

Free, zero-cost re-verification only — NEVER run `agent --print` in this feature. Run and record verbatim (redacted) output + exit codes for: `agent --version`, `agent --help`, `agent status`, `agent about`, `agent models`, and `agent --list-models`. Goals: (1) confirm the CLI version is no longer 2026.04.13-a9d7fb5 and record the actual value; (2) re-verify the FULL flag surface against the new `--help` — do NOT trust the merged flag list; note any flags added/removed vs the current probe-result.json.flags array; (3) capture whether Cursor auth is usable IN THIS WORKER'S ENVIRONMENT (this is the gate for the billed features that follow) by recording the redacted `agent status`/`agent about`/`agent models` output and exit codes. Update docs/scoping/cursor-probe-evidence/preflight.md by APPENDING a new dated section headed for the new binary version (keep the existing 2026.04.13 sections verbatim as history). Update docs/scoping/cursor-probe-evidence/probe-result.json fields `cli_version` and `flags` to the re-verified values. Record the id-vs-display-name OFF-BY-ONE trap for grok tiers (e.g. `grok-4.5-medium` displays 'Grok 4.5 Low'; `grok-4.5-high` displays 'Grok 4.5 Medium') in preflight.md. If your factual changes to preflight.md make docs/scoping/cursor-probe-evidence/check-preflight.sh assert something no longer true, update that script to stay consistent (it is in scope). Redact all emails/tokens/session-ids/home paths to `<redacted>`. Do NOT modify anything under crates/. Commit only under docs/scoping/cursor-probe-evidence/.

Done when:
- probe-result.json.cli_version is updated to the value reported by `agent --version` and differs from 2026.04.13-a9d7fb5
- probe-result.json.flags matches the flags actually present in the new `agent --help`, with any additions/removals noted
- preflight.md gains a new dated section for the new binary while the prior 2026.04.13 sections remain as history
- The redacted `agent status`/`agent about`/`agent models` output and exit codes are recorded, stating plainly whether auth is usable in this environment
- The grok id-vs-display-name off-by-one trap is documented in preflight.md
- No file under crates/ is modified and no billed `agent --print` call was made

### 1.2 Capture read-only text and json output-shape samples

GATE (apply exactly): re-run the free check `agent status` and `agent models`. Proceed with live capture ONLY if it shows a logged-in account WITH provisioned models. If auth is unusable ('Not logged in' or 'No models available for this account'), look for operator-staged redacted raw captures at `.kranz/cursor-raw/` in the repo root and derive the samples from those. If NEITHER usable live auth NOR staged raw captures exist, STOP immediately and report result:blocked with the verbatim (redacted) `agent status`/`agent models` output and the paths you checked — DO NOT fabricate any capture. When capturing live: create a THROWAWAY git repo under $TMPDIR via `mktemp -d` then `git init` (NEVER this repo); run the SMALLEST possible read-only prompt (e.g. the single word reply 'OK') twice, once with `--output-format text` and once with `--output-format json`, invoked as `agent --print --mode ask --trust --workspace <tmpdir> --model gpt-5.6-luna-low` (cheap model; use `--workspace`, NEVER `--worktree`). Keep to at most 2 billed turns. Redact emails/tokens/session-ids/home paths to `<redacted>`. Commit the captures under docs/scoping/cursor-probe-evidence/ with EXACTLY these names: `fixture-text.txt` and `fixture-json.json`. Do NOT modify crates/.

Done when:
- docs/scoping/cursor-probe-evidence/fixture-text.txt exists, is non-empty, and is redacted
- docs/scoping/cursor-probe-evidence/fixture-json.json exists and is valid JSON
- If auth was unusable and no staged raw existed, the report is result:blocked with verbatim evidence and no fabricated fixture
- At most 2 billed turns were spent and no file under crates/ was modified

### 1.3 Capture the canonical write-capable stream-json fixture and record write posture

Apply the SAME GATE as the read-only-samples feature (check `agent status`/`agent models`; else `.kranz/cursor-raw/`; else STOP with result:blocked and never fabricate). When live: in a THROWAWAY git repo under $TMPDIR (`mktemp -d` + `git init`, NEVER this repo), run ONE small write-capable prompt in DEFAULT agent mode (non-interactive via `--force --trust`) that BOTH edits a file AND runs a shell command, e.g. 'Create hello.txt containing hi, then run cat hello.txt', invoked as `agent --print --output-format stream-json --force --trust --workspace <tmpdir> --model gpt-5.6-luna-low` (NEVER --worktree). The captured stream MUST contain assistant text, at least one tool-use event, at least one tool-result event, and a terminal event carrying token usage and/or cost. Redact emails/tokens/session-ids/home paths to `<redacted>` (preserve JSON structure and event types — only redact values). Commit it as docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl and set probe-result.json.fixture to the string 'docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl'. Additionally record, in preflight.md or probe-result.json, how file-edit, shell-command, deliberately-failing-command, and no-op prompts each present in the stream and what the resulting temp-repo `git diff` looked like (you may reuse the one write-capable run plus one failing command and one no-op; keep spend to a small handful of billed turns). Redact throughout. Do NOT modify crates/. Commit only under docs/scoping/cursor-probe-evidence/.

Done when:
- docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl is committed as valid, redacted JSONL
- The fixture demonstrably contains assistant text, at least one tool-use event, at least one tool-result event, and a terminal usage/cost event
- probe-result.json.fixture points to the committed stream-json fixture
- How file-edit, shell-command, failing-command, and no-op present (plus the temp-repo git diff) is recorded
- If auth was unusable and no staged raw existed, the report is result:blocked with no fabricated fixture; no file under crates/ was modified

### 1.4 Live model matrix and permission posture

Apply the SAME GATE as the other billed features (check `agent status`/`agent models`; else `.kranz/cursor-raw/`; else STOP with result:blocked and never fabricate). Model matrix (in a THROWAWAY $TMPDIR git repo, smallest prompts, `--workspace` never `--worktree`): exercise `--model auto`, `--model grok-4.5-xhigh`, `--model gpt-5.6-luna-low`, and `--model definitely-not-a-real-model` (invalid). Record the REAL response for each and exactly how a model-availability / invalid-model failure presents (message, exit code, whether it fails before or after a billed turn starts). Update probe-result.json.model_matrix with keys covering auto, grok-4.5-xhigh, a gpt-5.6 case, and invalid. Keep the grok-4.5-xhigh run to a single smallest prompt to limit spend. Permission posture: record the observed behavior of `--mode ask`, `--mode plan`, default agent mode, `--force`/`--yolo`, and `--sandbox enabled|disabled`, and map which are read-only enough to satisfy a validator role vs which permit writes for a worker role, against kranz's no-push/no-publish/no-main-write invariants — you may infer sandbox/mode semantics from small runs plus `--help` where a live run would waste spend, but label inferred-vs-observed. Redact emails/tokens/session-ids/home paths to `<redacted>`. Do NOT modify crates/. Commit only under docs/scoping/cursor-probe-evidence/.

Done when:
- probe-result.json.model_matrix records real live results for auto, grok-4.5-xhigh, a gpt-5.6 model, and an invalid id, including how availability/invalid failures present
- Permission posture for --mode ask, --mode plan, default, --force, and --sandbox enabled|disabled is recorded and mapped to validator-vs-worker role needs, labeling observed vs inferred
- If auth was unusable and no staged raw existed, the report is result:blocked with no fabricated data
- No file under crates/ was modified and grok-4.5-xhigh spend was held to a single smallest prompt


## Milestone 2 — Route decision finalized with brief if green

### 2.1 Score the seven acceptance-bar items and set the recommendation

Pure synthesis from the committed evidence (preflight.md, fixture-stream-json.jsonl, fixture-text.txt, fixture-json.json, model_matrix) — no new billed `agent` calls. For each of the seven probe-result.json.acceptance_bar items (terminal_text_stitching, tool_use_and_results_observable, usage_cost_on_wire_or_priceable, cwd_workspace_worktree_isolation, model_availability_failures_deterministic_readable, permission_mapping_preserves_invariants, fixture_offline_parser_test), replace the prior 'unresolved' verdict with a scored verdict (resolved/partially-resolved/unresolved) justified by a specific pointer into the captured evidence. Set probe-result.json.recommendation STRICTLY from the observed structure: `direct-parser` if the stream-json exposes per-event structure (assistant text, tool-use, tool-result, terminal usage/cost) comparable to backend_codex's item/turn vocabulary; `acp` if stream-json is too lossy but ACP would give a stable protocol; `defer` only if the evidence genuinely does not support a backend. Do not force a green verdict — follow the evidence. Do NOT modify crates/. Commit only under docs/scoping/cursor-probe-evidence/.

Done when:
- All seven acceptance_bar items carry a non-empty scored verdict, each citing specific captured evidence
- probe-result.json.recommendation is one of direct-parser, acp, or defer and is justified by the scored bar
- The recommendation is consistent with the stream-json fixture's actual event structure
- No file under crates/ was modified

### 2.2 Update the Decision section and write the implementation brief if green

Update docs/scoping/cursor-cli-backend.md: APPEND a NEW dated Decision entry (heading like '## Decision (2026-07-DD)') that keeps the existing '## Decision (2026-07-09)' defer entry intact as history. The new entry must state the final route matching probe-result.json.recommendation and include the acceptance-bar verdict table refreshed from the live evidence. In the permission/auth portion, RECORD the verified constraint that Cursor auth state lives under $HOME/.cursor and does NOT survive a relocated HOME, so any worker/validator running `agent` needs either the real ~/.cursor carried into its HOME or a CURSOR_API_KEY in its session env (reference CURSOR_API_KEY explicitly). IF the route is `direct-parser` or `acp`: also write a scoped implementation brief in the same doc, mirroring docs/scoping/codex-backend.md's shape (single-shot, validator-first) — an event-to-AgentEvent mapping table grounded in the committed fixture, how terminal text / usage / cost are obtained, how permission modes map onto kranz invariants, and the auth requirement above; do NOT implement any backend and do NOT touch crates/. IF the route is `defer`: state precisely why and the preconditions to unblock, and do not write a brief. Commit only to docs/scoping/cursor-cli-backend.md.

Done when:
- cursor-cli-backend.md has a new dated Decision entry whose route matches probe-result.json.recommendation, with the 2026-07-09 defer entry preserved as history
- The refreshed acceptance-bar verdict table reflects the live evidence
- The permission/auth section records the ~/.cursor-does-not-survive-HOME-relocation constraint and the ~/.cursor-or-CURSOR_API_KEY requirement
- If the route is direct-parser or acp, a single-shot validator-first implementation brief with a fixture-grounded event mapping table is present; if defer, a why-plus-preconditions rationale is present and no brief
- No file under crates/ was modified

