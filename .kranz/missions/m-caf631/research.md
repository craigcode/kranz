# Research — m-caf631

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/permissions.rs
- crates/engine/src/backend_acp.rs
- crates/engine/src/backend_acp/terminals.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/runner.rs
- crates/engine/src/hook_gates.rs
- crates/engine/src/sgian_terminal.rs
- crates/engine/tests/runner_test.rs
- docs/design.md
- docs/knowledge/validation/child-containment.md
- docs/knowledge/decisions/positioning-governance-evidence-layer.md
- .github/workflows/ci.yml

## External sources

- Claude Code permissions docs: Bash rules recognise shell operators such as && (external CLI behaviour, not testable in this repo)

## Facts

- WORKER_DENY has no git stash rule — `crates/engine/src/permissions.rs:99-113`
- The grant matcher prefix-matches only the whole command, so a compound denied command yields no worker-deny grant offer — `crates/engine/src/permissions.rs:361-374; orchestrator.rs:2464`
- The ACP request_permission check matches the whole command string from its start, with no segment splitting — `crates/engine/src/backend_acp.rs:176-196, 247-269, 348-365`
- The Sgian terminal check splits sh -c scripts on newline ; | & ( ) and backtick — `crates/engine/src/backend_acp/terminals.rs:844-873`
- The Claude backend passes deny rules straight to --disallowedTools; the CLI does the matching — `crates/engine/src/backend_claude.rs:512-514`
- Workers' denial text on the Claude backend is written by the CLI, not the engine; prompts.rs has no deny guidance — `grep 'git push|deny' crates/engine/src/prompts.rs → no matches`
- No existing engine code or test contains the text git_stash — `grep stash crates/engine → no matches at plan time`

## Ambiguities & stale docs

- D-X (operator, unanswered at plan time): may the denial name the read-only alternative? The plan defaults to the deny rule alone, so the goal's second sentence ('is told the read-only way') is NOT delivered by this plan. Choosing a PreToolUse hook, or lifting the prompts.rs freeze, means re-planning.
- Whether the Claude Code CLI denies `Bash(git stash*)` when the stash call follows ; && or a newline is external behaviour that no repo test can pin
- codex/droid/kimi/cursor workers do not read disallowed_tools, so this rule has no effect on them
- `git -C . stash` and extra whitespace (`git  stash`) get past every prefix rule, the same as `git -C . push` today; out of scope
- docs/design.md's worker deny row was already stale (missing git remote add, yarn/pnpm publish, gem push); the feature brings it in line

## Candidate knowledge updates

- A docs/knowledge/validation note on worker deny rule enforcement per backend: the Claude CLI matcher, ACP request_permission (whole-string prefix), Sgian terminal (segment split), the grant matcher (segment split after this mission), and the codex/droid/kimi/cursor backends that do not enforce them
