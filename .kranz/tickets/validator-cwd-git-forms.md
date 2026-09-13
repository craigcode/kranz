---
state: done
state-note: implemented operator-driven (validator path was self-referential for missions): 2a9035a denial detection, 982358c scrutiny/mechanical split, ab17654 guidance persist+inject, 3cdfb7d unblock-add-fix, bfac7e6 engine-run contract commands
title: Repair the validator execution path (guidance injection, scrutiny/mechanical split, engine-run commands, denial detection, blocked-state repair)
priority: 2
schedule: once
---

## Goal
Fix the validator execution path end to end — prompt discipline alone was
necessary but provably not sufficient (m-9e4ef3, five blocks). Ordered work:

1. **Persist and inject validator guidance.** Add a dedicated
   `validatorGuidance` field to the unblock decision/event/state and inject
   it verbatim into the next validator task and its retry. Today the
   milestone.unblocked reason is recorded but validation_round
   (orchestrator.rs:3745) never passes it into run_validator_in
   (runner.rs:1030) — operator guidance literally cannot reach a fresh
   validator. Must survive process restart (event-sourced, folded).
2. **Split scrutiny from mechanical validation.** The shared task advertises
   all Cargo commands to scrutiny (runner.rs:1020) even though
   validator-scrutiny.md says it need not run software. Scrutiny gets only
   diff + criteria and works with Read/Grep/Glob + plain git forms (cwd IS
   the worktree, no cd prefix, no compound bash, dedicated tools first);
   remove the delegation/background encouragement at validator-scrutiny.md:55.
3. **Run functional commands in the engine, not via agent-authored bash.**
   Reuse the existing bounded timeout/process-tree execution for every exact
   contract/gate command and hand the validator captured results — removing
   pipes, lost exit codes, accidental backgrounding, Monitors, and
   permission improvisation from the validator's world.
4. **Correct denial detection.** The backend only maps ToolResult denials
   containing "permission" or hook failures; structured refusals like
   "requires approval", "Contains expansion", and "output redirection …
   blocked" finish with deniedToolResults=0, so the grant path never fires
   (observed live in m-9e4ef3/m-3cda6a blocks). Map those refusal shapes to
   denials so grants activate instead of silent aborts.
5. **Add a blocked-state repair action.** Today unblock paths only resume
   validation / skip / stay blocked, so "FMT FIRST" cannot be scheduled. Add
   an add-fix-feature-style action so the orchestrator can schedule a repair
   worker (fmt, doc fix) before another validation round.

## Context
m-9e4ef3 blocked five times across: awk compound, head+echo+wc compound,
Monitor stall, and two earlier scan/grant issues — implementation complete
throughout. An external review (verified against the code 2026-07-22)
supplied items 1–5 and one correction: there is no validator stall timeout
in run_session_to (runner.rs:310); it waits on validator events untimed —
the 10-minute stall timeout applies only to orchestrator turns. The Monitor
run ended Partial/aborted via backend turn/budget/session mechanics, not a
timer. The validator-cwd-git-forms prompt discipline (previously this
ticket) folds into item 2; the allow-set stays exactly as strict throughout.

## Acceptance hints
- milestone.unblocked carries validatorGuidance; the next validator task
  and its retry contain it verbatim; a kill-and-resume test proves it
  survives restart.
- Scrutiny receives diff+criteria only (no cargo command advertisement);
  its prompt drops background/delegation encouragement; a mock scrutiny
  validator completes with Read/Grep/Glob + plain git, zero denied tools.
- Contract/gate commands execute engine-side with captured output fed to
  the validator; the "requires approval"/"Contains expansion"/"output
  redirection blocked" shapes produce deniedToolResults > 0 and park a
  command grant.
- The blocked state offers a repair action that schedules a fix worker
  before re-validation (test: fmt repair then successful validation).
- cargo test --workspace green.
