# Kranz — design notes (Rust/Tauri implementation)

Derived from the v3 build plan (`mission-control-clone-plan.md`), adapted to Rust.
The original plan assumed the TypeScript Claude Agent SDK; there is no Rust SDK,
so the engine drives the `claude` CLI headless — the same process the SDKs wrap —
which preserves the plan's core premise: sessions run with `cwd` = target repo
root, so CLAUDE.md, `.claude/skills`, `.mcp.json`, and hooks are inherited for free.

## Verified CLI behavior (claude 2.1.198, probed 2026-07-02)

Fixture: `crates/engine/tests/fixtures/stream_json_single_shot.jsonl`.

1. **Single-shot**: `claude -p "<prompt>" --output-format stream-json --verbose`
   emits JSONL: `system/init` (session_id, model, tools), `system/*` noise
   (thinking_tokens, post_turn_summary), `rate_limit_event`, `assistant`
   messages (content blocks: thinking/text/tool_use), and a terminal `result`
   with `total_cost_usd`, `usage` (input_tokens, output_tokens,
   cache_read_input_tokens, cache_creation_input_tokens), `num_turns`,
   `permission_denials[]`, `is_error`. Parsers MUST tolerate unknown types.
2. **Streaming input**: `--input-format stream-json` + stdin lines
   `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}`.
   Each injected message runs one turn ending in its own `result` message;
   `system/init` re-emits per turn. Process stays alive until stdin closes.
3. **Session ids**: `--session-id <uuid>` (engine-chosen) is honored verbatim;
   `--resume <uuid>` works across processes and preserves full context.
4. **No `--max-turns`** in 2.1.198. Turn budgets are engine-enforced: count
   distinct assistant `message.id`s; abort over budget → result `partial`.
   `--max-budget-usd` gives a second, dollar-denominated cap.
5. `--effort low|medium|high|xhigh|max` maps the plan's per-role reasoning effort.
6. `--json-schema '<schema>'` constrains the session's final structured output —
   used to enforce WorkerReport / ValidatorReport / plan JSON at the source.
7. In `-p` mode there are no interactive prompts: a tool call outside the
   allowed set fails with an error the model can read and route around (§4.7).

## Permission mapping (plan §4.7)

Deny rules take precedence over allows in Claude Code.

| Role | mode | tools | allow | deny |
|---|---|---|---|---|
| Orchestrator | `default` | `Bash,Read,Glob,Grep` | `Read`, `Glob`, `Grep`, `Bash(git log*/diff*/show*/status*/rev-parse*/branch*/tag*)` | everything else (unlisted Bash fails) |
| Worker | `acceptEdits` | default set | `Bash`, edit tools via mode | `Bash(git push*)`, `Bash(npm publish*)`, `Bash(cargo publish*)`, `Bash(twine*)`, `Bash(sudo*)`, `Bash(curl*)`, `Bash(wget*)`, `WebFetch`, `WebSearch`, + config `denyPatterns` |
| Validators | `default` | `Bash,Read,Glob,Grep` | git-inspect patterns + contract `command`s + `allowValidatorCommands` | everything else |

`--dangerously-allow-all` → `bypassPermissions`, loud in UI, never default.
Denied tool results are tagged `denied` on `worker.message` events.

Live-QA mode (functional validator only): any EXTRA tool configured in
`validatorFunctional.tools` beyond the standard inspect set
(`Bash,Read,Glob,Grep`) — e.g. a browser/computer-use tool — is also folded
into that validator's `allow` list, since a tool call that exists in `--tools`
but isn't auto-approved fails outright in `-p` mode (no interactive prompt to
fall back on). This lets the functional validator drive the built app against
acceptance criteria. Write/Edit/WebFetch/WebSearch/git-push stay denied, and
the scrutiny validator never receives this treatment.

## Cross-process control

Single writer rule (§4.3): only the engine process appends `events.jsonl`
(exclusive `events.jsonl.lock`, two lines: holder pid + acquire time as unix
epoch seconds; the legacy one-line pid-only format still parses). CLI
(`kranz msg/pause/resume`) and the
server enqueue `ControlCommand` JSON files into
`.kranz/missions/<id>/control/<millis>-<rand>.json`; the engine drains the inbox
between worker runs (and a watcher aborts the active run on `--interrupt`).
Dashboard/CLI observe by reading `events.jsonl` + `state.json` (read-only tail).

Lock stealing is tiered by the holder's probed liveness (`kill(pid, 0)` on
unix; unprobeable elsewhere). A holder that is provably DEAD — ESRCH, or an
alive pid whose process START time postdates the lock's acquire time by more
than 2s (pid reuse: the writer is dead, the pid was recycled; start time via
`/proc/<pid>/stat` on linux, `ps -o etime=` on macOS) — is stale and stolen
automatically, no flag needed. Otherwise:

| holder liveness                     | (no flag) | `--force-lock` | `--dangerously-steal-live-lock` |
|-------------------------------------|-----------|----------------|---------------------------------|
| dead / pid reused                   | steal     | steal          | steal                           |
| unknown (unparseable pid, non-unix) | refuse    | steal          | steal                           |
| provably ALIVE                      | refuse    | refuse         | steal (loud warning)            |

`--force-lock` therefore can no longer rip the lock from a running engine —
the failure mode where an operator decides a long parallel batch is "stuck"
and corrupts it with a second engine. Stealing from a provably live holder
requires `--dangerously-steal-live-lock` (implies `--force-lock`), reserved
for holders verified — e.g. via `ps -p <pid>` — to be zombies or foreign
processes. The probe must never report a false "dead": anything uncertain
reads as unknown or alive, because a false dead lets two engines write one
log.

## Orchestrator loop specifics (§4.5)

- The orchestrator is one long-lived streaming session. Every injected turn is
  prefixed with a deterministic digest rendered from state (§4.8): goal,
  contract, milestone/feature table, last 10 decisions, open user messages.
- Post-worker judgement, finding→fix-feature conversion, dirty-tree decisions,
  and agent-judgement contract checks are orchestrator turns that must answer
  in JSON (parse leniently, one retry, then conservative default: respawn ≤ cap
  else fail feature / block milestone).
- Re-seed path: if the streaming session dies or `--resume` fails, start fresh
  and seed with digest + plan.json. This is a tested property, not an
  emergency procedure.
- Final contract gate: engine runs every `check:"command"` assertion itself
  (std process, cwd = repo); failures become a validation round on the final
  milestone. `agent-judgement` assertions go to the orchestrator with the
  `base..HEAD` diff.

## Deviations from the plan document (all deliberate)

1. **Rust/Tauri instead of TS/Node/Fastify** (user request). Packages →
   Cargo workspace crates: `engine`, `cli`, `server`; `apps/dashboard`
   (React/Vite + Tauri shell that embeds the axum server).
2. **`MissionStatus::Validating` added** — the plan's §4.2 enum lacked a value
   for the §4.5 final-gate phase its own event models; without it the status
   would lie during the gate.
3. **Model defaults use aliases** (`opus`/`sonnet`) rather than the plan's
   pinned `claude-sonnet-4-6` (stale id); users pin full ids in config.
4. **Dashboard consumes reduced state snapshots + events over WS** instead of
   sharing reducer code (plan assumed one TS codebase). The Rust reducer stays
   the single source of truth; the UI is a pure view.
5. **Worker turn budget enforced by the engine** (CLI dropped `--max-turns`).
6. **Pinned-base validation contract** — after the m-660ffc incident, where a
   contract's `git diff main` assertion raced a concurrent commit landing on
   the base branch mid-mission, the base branch's commit SHA is now captured
   at plan approval, recorded on the `plan.approved` event (optional field,
   additive for backward compatibility), and exposed to every worker and
   validator session as the `KRANZ_BASE_SHA` env var. The role prompts
   reference it so contracts diff against a pinned commit instead of a
   moving branch name.
7. **Repo-level cross-mission lessons** (not in the plan document) — a
   deliberate learning loop closes the gap between missions. `.kranz/lessons/`
   is a repo-level, append-only store (an `index.md` manifest plus one file
   per lesson) that survives mission deletion and `kranz clean`. At mission
   completion the orchestrator is asked for at most one reusable lesson —
   a short imperative note or `NONE` — which is appended and committed
   alongside the final report. Capping only happens at injection time: a
   future mission's planning seed gets a byte-capped (2048 bytes) rendering
   of the index, newest-first, at most 10 entries, with the 3 newest inlined
   in full text.
