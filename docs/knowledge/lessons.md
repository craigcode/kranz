---
title: Lessons — curated index
owner: mixed
freshness: check-on-touch
last_verified: 2026-11-24
verified_against:
  - .kranz/lessons/index.md
  - crates/engine/src/lessons.rs
  - crates/engine/src/lesson_checks.rs
  - AGENTS.md
  - docs/knowledge/decisions/inviolable-invariants.md
---

Rechecked 2026-11-24 (UTC): lessons gained an optional companion check record
(`.kranz/lessons/<mission-id>.check`) and a read-only `kranz lessons list
[--json] [--needs-check]` listing (see
[Could a check catch it?](#could-a-check-catch-it) below). Lesson bytes,
`index.md` and planning injection are unchanged.

Rechecked 2026-11-24 (UTC) against the invariants note's test-only `contract_lint.rs` recheck: no lesson changed.

Rechecked 2026-11-23 (UTC) against the invariants note's contract command
timeout recheck (a contract `command` assertion's wall-clock limit is now
configurable, via plan `timeoutSecs` or operator config
`contractCommandTimeoutSecs`, capped at 14400 seconds): no lesson changed.

Rechecked 2026-11-23 (UTC) against `.kranz/lessons/index.md`, which gained the m-ec06f2 lesson (scope contract test commands; do not use `cargo test --workspace` as a contract command). The curated list is unchanged.

Rechecked 2026-11-22 (UTC) against the invariants note's worker `git stash` deny recheck: no lesson changed.

Rechecked 2026-11-20 (UTC) against the invariants note's milestone-scoped assertion recheck (not-yet-due assertions are recorded but do not block until their milestone; final gate unchanged): no lesson changed.

Rechecked 2026-11-18 (UTC) against `AGENTS.md`'s Even G2 command line now runs `scripts/check-npm-audit.py` in place of `npm audit --audit-level=high`. No lesson changed.

Rechecked 2026-11-17 (UTC) against the invariants note's grant wait and stall record recheck: no lesson changed.

Rechecked 2026-11-17 (UTC) against the invariants note's temp-directory cleanup recheck: no lesson changed.

Rechecked 2026-11-16 (UTC) against the invariants note's Windows first-run recheck: no lesson changed.

Rechecked 2026-11-15 (UTC) against the invariants note's test-only recheck: no lesson changed.

Rechecked 2026-11-14 (UTC) against the invariants note's grant decision binding recheck: no lesson changed.

Rechecked 2026-11-13 (UTC) after the invariants note was rechecked against a test-only engine change. No lesson changed.

Rechecked 2026-11-12 (UTC) after the Sgian mission-runner terminal invariants recheck; lesson capture and provenance rules remain unchanged.

Rechecked 2026-11-11 (UTC) after the invariants note was rechecked against the second audit round. No lesson changed.

Rechecked 2026-11-11 (UTC) after the invariants note was rechecked against the Linux socket mask. No lesson changed.

Rechecked 2026-11-11 (UTC) after the invariants note was rechecked against the evidence and containment fixes. No lesson changed.

Rechecked 2026-11-11 (UTC) after a recheck-only edit to the inviolable invariants note for the base-policy merge check; no lesson changed.

Rechecked 2026-11-11 (UTC) after a recheck-only edit to the inviolable invariants note for the push-guard and secret-scan fixes; no lesson changed.

Rechecked 2026-11-11 (UTC) after a recheck-only edit to the inviolable invariants note for the consent-binding fixes; no lesson changed.

Rechecked 2026-11-11 (UTC) after a recheck-only edit to the inviolable invariants note; no lesson changed.

Rechecked 2026-11-10 (UTC) after a recheck-only edit to the inviolable invariants note; no lesson changed.

Rechecked 2026-11-10 (UTC) after the Sgian execution-grant invariants recheck; lesson capture and provenance rules remain unchanged.

Rechecked 2026-11-09 (UTC) after merging ACP R4 terminal budgets and v2 admission onto v0.5.0 main; lesson capture and provenance rules remain unchanged.

Rechecked 2026-11-07 (UTC) after the ACP reconciliation and AGENTS.md reference split. Full-workspace gates, exact staging, containment, provenance and lesson capture remain unchanged.

Rechecked 2026-11-07 against the AGENTS.md reference extraction and security invariant update. The containment guidance moved to its own knowledge note; vocabulary and lesson capture rules are unchanged.

Rechecked 2026-11-02 (UTC) after the signal intake invariants update; no lesson changed.

Rechecked 2026-10-22 (UTC) after a recheck-only edit to the inviolable invariants note; no lesson changed.

Rechecked 2026-10-18 (UTC) after a recheck-only edit to the inviolable invariants note; no lesson changed.

Rechecked 2026-10-11 (UTC) after a recheck-only edit to the inviolable invariants note; no lesson changed.

Rechecked 2026-10-06 (UTC) against the R2 invariants note. Resource failures
require observed evidence and explicit cleanup state; lesson capture and its
provenance channel are unchanged.

Rechecked 2026-10-04 (UTC) against the fixture-platform invariants update.
Lesson capture, provenance and the anti-vacuity rule are unchanged.

Rechecked 2026-10-03 (UTC) for the completed container-denial fixture scope.
File and network access probes require guest receipts and confirmed cleanup;
production routing, process ownership and mission contracts are unchanged.

Rechecked 2026-09-30 against the container-denial invariants note. The test
receipts strengthen the existing anti-vacuity rule; lesson capture, provenance
and production cleanup requirements are unchanged.

Rechecked 2026-09-29 (UTC) against the public-documentation rule and invariants note; append-only lesson capture and its provenance rules are unchanged.

Rechecked 2026-09-26 against the updated invariants. Explicit Sgian opt-in, visible approvals, committed-source coverage and fresh mount proofs refine safety rules without changing mission lesson capture.

# Lessons

Rechecked 2026-09-25 against the shared ACP invariants update. The isolated
crate check supplements the full-workspace gate, and shared transport does not
change process ownership, permission authority or the lesson-capture channel.

`.kranz/lessons/` is the append-only, mission-capture memory: at most one
reusable lesson per mission, injected tightly-capped into planning seeds. It
stays the source of truth for hard-won operator scars — this file does **not**
absorb it. Per the [scoping decision](../scoping/repo-knowledge-store.md)
(D-F), the vault only *summarizes and links*.

## Where the lessons live

- Index: [`.kranz/lessons/index.md`](../../.kranz/lessons/index.md) — appended in
  capture order, oldest first. Planning injection selects the newest
  provenance-clean lessons first.
- One file per mission: `.kranz/lessons/<mission>.md`.
- Injected into planning via the engine's lessons channel, with its own byte
  budget, separate from the shipped ranked knowledge-vault block.

## Could a check catch it?

Each captured lesson may have a companion record,
`.kranz/lessons/<mission-id>.check`, that says whether a deterministic check
(an approval-lint rule, a preflight rule, or a gate) could catch the same
mistake. It is a plain `key: value` text file — deliberately **not**
`.md`, so it can never be mistaken for a lesson body and the renderer that
injects lessons into planning never reads it:

```
# Lesson check record for m-2026-example.
orchestrator: checkable
operator: not-checkable
link: .kranz/tickets/add-lint-rule.md
```

Recognized keys are `orchestrator`, `operator`, and `link`; `#` starts a
comment. `orchestrator` and `operator` take one of `checkable`,
`not-checkable`, or `unclassified`. The **operator's value wins** over the
orchestrator's when both are set; if neither is set, or the file doesn't
exist, the lesson reads as `unclassified`. Every lesson captured before this
record existed therefore reads as `unclassified` — there is no backfill.

The record is a separate file, not a field inside the lesson or the index,
so that writing or editing it can never change a lesson's bytes or its
`index.md` line: provenance and the capped planning-injection budget stay
exactly as before. The renderer that builds the planning-injection block
(`render_lessons_manifest`) only ever reads `.md` names out of the index and
never looks at `.check` files.

The orchestrator *proposes* a value at lesson capture: it ends its lesson
reply with a trailing `CHECK: checkable` or `CHECK: not-checkable` line,
which the engine strips out of the saved lesson body and instead records as
the initial `orchestrator:` value of the `.check` file. Operators classify or
link a check by hand — editing the `.check` file directly and committing it,
the same as any other repo file — setting `operator:` and/or `link:` (a path
or ticket/rule reference, e.g. a line in `AGENTS.md` or the invariants note,
or a ticket under `.kranz/tickets/`). This is decision **D-X**: both sides
may classify a lesson, and when they disagree the operator's answer wins.

`kranz lessons list [--json] [--needs-check]` lists lessons and their
effective classification, oldest first, exactly as they appear in
`index.md`. It is read-only and takes no engine lock. `--json` emits the
listing as JSON; `--needs-check` narrows it to lessons classified
`checkable` with no `link:` set — the ones still waiting for someone to
write the check and link it here.

Lesson checks stay a plain ticket/rule link rather than reusing
`review_memory.rs`'s replay-suite-and-promotion route. That route promotes
*rule text fed to a reviewer model* — prompt-shaping, which sits on the
frozen side of the
[positioning note](decisions/positioning-governance-evidence-layer.md) — and
gates promotion on passing a replay suite of required/prohibited findings,
which fits a model-judged rule, not a deterministic code check. A lesson
check, by contrast, is ordinary Rust (or other deterministic) code that ships
through a normal mission gate and PR review, which already supplies the
human approval a replay suite exists to simulate. No lint rule is generated
automatically from a `.check` record; writing the check, and linking it back
here, remains a human task.

## Durable rules already promoted to standing docs

Some lessons recur enough that they are now standing rules, enforced in
[AGENTS.md](../../AGENTS.md) and the
[inviolable invariants](decisions/inviolable-invariants.md) note rather than
left to per-mission capture:

- **Full-workspace gate.** The final gate is `cargo test --workspace` (never a
  single `-p <crate>`), plus `clippy --workspace --all-targets -D warnings` and
  `fmt --all --check`. Run `cargo fmt` before finishing.
- **Bare exit codes.** Judge gate commands by their exit status; do not pipe the
  final gate command (piping can mask a non-zero exit).
- **Anti-vacuity.** A contract command that asserts test coverage must guard
  against zero matched tests:
  `... 2>&1 | grep -qE 'test result: ok\. [1-9]'`.
- **Contract-file additivity.** A revision may not weaken or drop an existing
  validation assertion; completed milestones are frozen.
- **Separate human review from validator inputs.** Human packets require the
  existing read capability and stay out of committed reports and fresh
  validator inputs. Decision controls still recheck identity and freshness.
- **kranz never pushes by default.** The explicit cloud handoff can push only
  a completed `kranz/*` branch; see the invariants note.
- **Own preflight helpers.** A host CLI timeout does not remove its daemon
  container. Bound the guest lifetime, delete only owned full IDs, and require
  confirmed absence; retain recovery intent when creation or cleanup is uncertain.
  See mount preflight ownership in the invariants note.
- **Name the actual isolation boundary.** Read-only cache mounts protect guest
  writes, not cache confidentiality or host-side mutation. Default-bridge
  relay access assumes trusted Docker peers; see the invariants note.
- **Sandbox mount ordering.** Restoring a writable private workspace must
  preserve covered Git/cache write protections and later authority masks;
  see the authority-material rule in the invariants note.

When a new lesson turns out to be a durable rule (not just a one-mission scar),
promote it into AGENTS.md / the invariants note and link it here — keep the
`.kranz/lessons/` entry as the origin record.
