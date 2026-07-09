# Research — m-73ada5

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- docs/scoping/cursor-cli-backend.md
- .kranz/tickets/cursor-cli-live-capture-route-decision.md
- docs/scoping/cursor-probe-evidence/probe-result.json
- docs/scoping/cursor-probe-evidence/preflight.md
- docs/scoping/codex-backend.md
- docs/scoping/worker-auth-preflight.md
- docs/scoping/worker-sandboxing.md
- .kranz/lessons/m-7820b9.md

## External sources

- git show 3d8f93e (ticket: cursor auth does not survive HOME relocation)

## Facts

- The prior mission deferred purely on auth; probe-result.json.fixture is null and all seven acceptance_bar items are 'unresolved' pending a live authenticated --print capture. — `docs/scoping/cursor-probe-evidence/probe-result.json (fixture:null, acceptance_bar all 'unresolved')`
- Cursor auth state lives under $HOME/.cursor and does NOT survive a relocated HOME (HOME=/tmp/x agent status -> Not logged in). — `git show 3d8f93e; .kranz/tickets/cursor-cli-live-capture-route-decision.md lines 62-66`
- Worker env hygiene relocates HOME to a scratch dir carrying only claude_min_config_entries (a Claude-only allowlist); ~/.cursor is not among them, so a relocated worker cannot authenticate `agent`. — `docs/scoping/worker-sandboxing.md 'Env hygiene' + claude_min_config_entries reference; docs/scoping/worker-auth-preflight.md`
- Tier-2 Seatbelt enforce:fs write-allowlist is worktree + mission dir + TMPDIR, which would block writes to ~/.cursor and ~/.cursor/worktrees; a temp repo must live under TMPDIR and use --workspace, not --worktree. — `docs/scoping/worker-sandboxing.md Tier 2 macOS section`
- backend_codex maps a per-line JSONL event vocabulary (thread.started/turn.started/item.completed/turn.completed with usage) to AgentEvent; the cursor brief should mirror this single-shot validator-first shape from a committed fixture. — `docs/scoping/codex-backend.md`
- grok-4.5 CLI model ids display one tier off from their id (grok-4.5-medium displays 'Grok 4.5 Low'); model ids are not client-side validated at the arg-parse layer. — `.kranz/tickets/cursor-cli-live-capture-route-decision.md lines 24-27; docs/scoping/cursor-probe-evidence/preflight.md model-selection section`
- A spec-directed helper script committed outside the plan's touch set triggers an out-of-contract-write finding; the touch-set glob docs/scoping/cursor-probe-evidence/** covers check-*.sh under that dir. — `.kranz/lessons/m-7820b9.md`

## Ambiguities & stale docs

- Q1 unresolved at plan time: whether this mission's workers receive a Cursor-usable env (inherited real HOME with ~/.cursor, or CURSOR_API_KEY in session env) or a relocated HOME that drops ~/.cursor. The plan hedges with a per-feature auth gate, an operator-staged-raw fallback at .kranz/cursor-raw/, and a no-fabricate STOP.
- The final route (direct-parser vs acp vs defer) cannot be known until the stream-json structure is captured; the contract requires a captured fixture and a scored bar but does not presuppose green.

## Candidate knowledge updates

- Add a note (e.g. to docs/scoping/worker-sandboxing.md open questions, or a new docs/scoping/cursor-cli-min-env.md mirroring claude-cli-min-env.md) that Cursor auth lives in ~/.cursor and is NOT in claude_min_config_entries, so relocated workers cannot run `agent` — live Cursor work needs an inherited HOME or CURSOR_API_KEY.
- If this probe goes green, capture a lesson that Cursor live-capture missions must front billed capture with an auth self-check gate + no-fabricate STOP because worker HOME relocation silently kills `agent` auth.
