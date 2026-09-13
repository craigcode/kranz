# Research — m-7820b9

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- docs/scoping/cursor-cli-backend.md
- crates/engine/src/backend_codex.rs
- .kranz/tickets/cursor-cli-grok45-backend-probe.md

## External sources

- git show 505d43d (ticket + scoping doc commit)
- crates/engine/tests/fixtures/ listing

## Facts

- backend_codex is single-shot and its entire test suite runs offline against committed fixtures (codex_exec_scrutiny.jsonl), never a live CLI call — the template a cursor probe deliverable must fit. — `crates/engine/src/backend_codex.rs tests module reads tests/fixtures/codex_exec_scrutiny.jsonl`
- SessionSpec exposes env (injected via .envs) and a sandbox field, so credentials can be pushed to a worker env, but per-feature sandbox posture is not settable from the plan JSON. — `crates/engine/src/backend_codex.rs start(): command.envs(&spec.env); SessionSpec has sandbox field`
- Morning probe: agent 2026.04.13-a9d7fb5 confirms --print/--output-format stream-json/--model/--workspace/--worktree/--sandbox, but headless commands return 'Authentication required' and 'No models available for this account'; sandboxed auth crashes SecItemCopyMatching -50 (keychain unreachable). — `docs/scoping/cursor-cli-backend.md Probe findings section`
- Doc-heavy missions have historically blown cost calibration ~9x in this repo (m-d341a7: $163 actual vs $18 estimated), so this probe's estimate will likely under-read on the green branch. — `memory kranz-project.md dogfood mission m-d341a7 entry`

## Ambiguities & stale docs

- Whether CURSOR_API_KEY will be present in the worker env and whether the account has provisioned models — both decide the green vs red branch; the plan is designed to self-determine rather than presuppose either.
- The CLI model id for Grok 4.5 is unknown (public display name may differ from the id); f1/f2 must probe plausible ids rather than assume.

## Candidate knowledge updates

- If green: record the observed cursor stream-json event schema (event type names for text/tool-use/tool-result/terminal-usage) as a knowledge note for the eventual backend_cursor implementation.
- Record that a sandboxed headless kranz worker cannot use Cursor keychain auth (SecItemCopyMatching -50) and requires CURSOR_API_KEY in env — a reusable preflight constraint for any future Cursor-runtime mission.
