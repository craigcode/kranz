---
state: done
state-note: "Mission m-fbbac4. The diff was already allowed; the pipe into grep and head was what parked. Shipped the named form, validator-only deny rules for git's write and run options, and prompt wording that rules out pipes. Other revision ranges stay allowed, as `git show` and `git log` already read any revision."
title: A validator's read-only look at the mission diff should not park for a grant
priority: 2
schedule: once
---

## Goal

A validator can inspect the mission's own diff without raising a grant request. A milestone no longer blocks for an hour because a validator wanted to read what the mission changed.

## Context

Reported from mission m-dd7d22 on 2026-11-24. While validating ms-2, a validator asked to run:

`git diff <base_sha>..HEAD -- <three files in the touch set> | grep -nE "^\+.{101,}" | head -20`

The command only reads. It was outside the validator Bash allow-set, so it parked as a command grant request (`grant.requested`, seq 881). The mission was running under `kranz work` in a background shell with no `kranz serve` host and no Slack bridge, so the only notice was the one stderr line in the work log. Nobody saw it. After 3600 seconds the request was denied by default and the milestone went BLOCKED (seq 884).

Recovery took an operator message, a `kranz run`, a second full validation pass and the same grant approved by hand. The mission lost about two and a half hours of wall-clock time, one hour of it the wait itself.

Two earlier tickets cover the neighbouring ground and are both done:

- `validator-env-reads-no-grant` gave validators exactly `Bash(printenv KRANZ_*)` after the same pattern: a harmless read parked, then blocked a milestone. That is the precedent for sanctioning one narrow read-only form.
- `headless-grant-request-fails-fast` made the wait configurable (`grantRequestTimeoutSecs`) and added the stderr line. The outcome is still deny-default.

The allow-set is built in `crates/engine/src/permissions.rs` (`command_allow_patterns`). It was narrowed on purpose to the verbatim contract command and its exact segments. Read `docs/knowledge/validation/child-containment.md` before widening it.

## Scoping answers

- Sanction one narrow form for validators: a `git diff` between `$KRANZ_BASE_SHA` and `HEAD`, optionally limited to paths. Say in both validator prompts that this is the sanctioned way to read the mission diff, as was done for `printenv KRANZ_*`.
- Decide whether a pipe into a filter is in scope. A Bash allow pattern matches a whole command line, so `git diff ... | grep ...` is a different question from bare `git diff ...`. If pipes are out, the prompt must say so, and the validator is expected to read the unfiltered diff.
- Do not allow general `git` subcommands, other revision ranges, or flags that write or run programs (`--output`, `--ext-diff`, `-c` config overrides, textconv). The test must show those stay denied.
- The grant model is unchanged for everything else. Nothing is approved automatically and deny-default stays.
- Children are hostile by default. A validator is prompt-injectable, so judge the new form by what the worst command matching the pattern can do, not by the command in this report.

## D-X operator decision

What a validator does when a grant it asked for is denied by default.

- D-X1: unchanged. The milestone blocks and waits for an operator.
- D-X2: the validator is told the request was denied and finishes its verdict without that command. The milestone blocks only if the verdict then fails or the validator says it cannot judge.

D-X2 changes when a milestone blocks, so it is not part of this ticket unless the operator picks it.

## Acceptance hints

- A permissions test in the style of `validator_env_reads_allow_kranz_printenv_only`: the sanctioned diff form is in the validator allow-set for both roles, and bare `git`, `git diff` with an output flag, and other ranges are not.
- Both validator prompts name the sanctioned form.
- Worker allow-sets are unchanged.
- Anti-vacuity: the test filter matches only the new test.
