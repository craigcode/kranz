# Research — m-fbbac4

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/permissions.rs
- crates/engine/prompts/validator-scrutiny.md
- crates/engine/prompts/validator-functional.md
- crates/engine/src/runner.rs
- crates/engine/src/orchestrator.rs
- crates/engine/tests/runner_test.rs
- docs/knowledge/validation/child-containment.md
- .kranz/tickets/validator-diff-inspection-grant.md
- .kranz/tickets/validator-env-reads-no-grant.md
- .kranz/missions/m-dd7d22/events.jsonl

## External sources

- crates/engine/src/permissions.rs:125-143 (GIT_INSPECT)
- crates/engine/src/permissions.rs:250-298 (validator arm)
- .kranz/missions/m-dd7d22/events.jsonl seq 881-885

## Facts

- Validators (and the orchestrator) already allow `Bash(git diff*)`, `Bash(git log*)` and `Bash(git show*)`, so the bare diff in m-dd7d22 was already allowed; only the `| grep` and `| head` parts were denied. — `crates/engine/src/permissions.rs:125-129, 252; m-dd7d22 events seq 881`
- Grant requests come from CLI-side denials attributed to the preceding shell tool call; the engine does not match rules itself. — `crates/engine/src/runner.rs:608-632; orchestrator.rs:2381-2392`
- The scrutiny prompt already forbids pipes, chains and redirection; the functional prompt does not. — `crates/engine/prompts/validator-scrutiny.md:21`
- `Bash(git diff*)` also matches `git difftool`, which runs arbitrary programs via `-x`/`--extcmd`, and the broad rules admit `--output`, `--ext-diff` and `--textconv` today. — `crates/engine/src/permissions.rs:127; git-difftool(1)`
- The orchestrator read-only test expects `Bash(git diff*)` to be present. — `crates/engine/tests/runner_test.rs:234`

## Ambiguities & stale docs

- The planning questions (shape, pipes, other ranges, no-flag drivers, mid-pattern `*`) were not answered before the formal plan request, so this plan takes the stated defaults: option B, pipes out, other ranges not denied, the no-flag driver issue deferred.
- Mid-pattern `*` support in the installed Claude CLI is unverified; prefix-only fallback deny rules cover the leading-option position if it is unsupported.
- Without any flag, porcelain `git diff`, `git log -p` and `git show` already run diff.external and textconv drivers set by git config or worker-authored `.gitattributes`. This is out of scope and needs its own containment ticket (e.g. force `--no-ext-diff --no-textconv` or a clean git config for validators).
- The orchestrator profile keeps `Bash(git diff*)`, which also matches `git difftool`. Left unchanged here per scope; recommended as a follow-up ticket.

## Candidate knowledge updates

- docs/knowledge/validation/child-containment.md: record that validator git rules deny write/run diff options (`--output`, `--ext-diff`, `--textconv`, `git difftool`) and that the sanctioned mission-diff form is `git diff $KRANZ_BASE_SHA..HEAD` with no pipes.
- New ticket: validators and the orchestrator still run config- or .gitattributes-defined diff drivers by default; consider forcing `--no-ext-diff --no-textconv` or a neutral git config.
- New ticket: deny `git difftool*` for the orchestrator profile too.
