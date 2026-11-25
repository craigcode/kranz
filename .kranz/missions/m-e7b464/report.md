# Mission report — m-e7b464

**Goal:** When a milestone is completed again after a reopen, the engine moves the local tag `kranz/<mission>/<milestone>` to the newly validated commit and records it additively on `milestone.completed` (`tagCommit`, `retaggedFrom`). A real git failure still completes the milestone untagged, and the event says plainly whether a stale tag of that name remains (`staleTag`).

Branch `kranz/mission-m-e7b464` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

3 milestones, 5 features, gated by 15 contract assertions (13 command, 2 judgement).

1. **Tag primitives and additive event fields**
   - git_ops: read a tag's commit and move an annotated tag — Repository: Rust workspace, engine crate `kranz-engine` at crates/engine.
   - Additive milestone.completed fields and struct-literal sweep — Repository: Rust workspace.
2. **Re-completed milestones move their tag**
   - Milestone tag outcome module, orchestrator wiring and mission tests — Repository: Rust workspace, engine crate `kranz-engine` at crates/engine.
3. **Worktree coverage and operator surfaces**
   - Worktree-mode re-completion test — Repository: Rust workspace, engine crate `kranz-engine` at crates/engine.
   - tail display of retag and stale tag, CHANGELOG entry — Repository: Rust workspace.

**Chosen approach:** Move the canonical tag with `git tag -f -a` to the commit that last passed, and record the previous target additively on the event (`retagg…
**Elapsed:** 18h 38m 57s
**Tokens:** 524 in / 157064 out / 16014338 cache read / 854714 cache write
**Cost:** $9.79 actual vs $9.68–$48.40 estimated (expected $22.19)
**Interventions:** 0 (0 steers, 0 grant decisions, 0 revision rulings, 0 operator unblocks)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-e7b464-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 2 issue(s): [warn] command assertion [a1] runs `cargo test` without anti-vacuity (`ok. [1-9]`); a zero-test filter would pass vacuously; [warn] command assertion [a7] runs `cargo test` with… [truncated]

## What shipped

### Milestone 1 — Tag primitives and additive event fields ✅

- ✅ **git_ops: read a tag's commit and move an annotated tag** — 1 run
  Repository: Rust workspace, engine crate `kranz-engine` at crates/engine.
  - `c0bcba4` [f-1-1] add GitRepo::tag_commit and GitRepo::retag git primitives
  - ✓ milestone_tag_ref_ test: tag_commit returns Ok(None) for a tag that does not exist
  - ✓ milestone_tag_ref_ test: after repo.tag(name, msg), tag_commit returns the HEAD commit sha (the peeled commit, not the tag object sha)
  - ✓ milestone_tag_ref_ test: after a new commit, retag(name, msg2) makes tag_commit return the new HEAD; `git cat-file -t refs/tags/<name>` is still `tag`; the tag message is msg2
  - ✓ milestone_tag_ref_ test: tag_commit and retag both return Err for a flag-shaped name such as `-d`, and no tag is created
  - ✓ `cargo test -p kranz-engine --test git_ops_test milestone_tag_ref_` passes and clippy is clean
- ✅ **Additive milestone.completed fields and struct-literal sweep** — 1 run
  Repository: Rust workspace.
  - `730f719` [f-1-2] add additive milestone.completed tag fields and sweep construction sites
  - ✓ milestone_tag_fields_ test: pre-field milestone.completed JSON deserializes with tag_commit, retagged_from and stale_tag all None and folds the milestone to Complete
  - ✓ milestone_tag_fields_ test: an event with all three fields set serializes using camelCase keys tagCommit, retaggedFrom and staleTag and round-trips equal; with all None those keys are absent
  - ✓ `git diff` of crates/engine/src/events.rs against the base shows only added lines; crates/engine/src/types.rs is unchanged
  - ✓ `cargo clippy --workspace --all-targets -- -D warnings` passes, proving every construction site was swept

### Milestone 2 — Re-completed milestones move their tag ✅

- ✅ **Milestone tag outcome module, orchestrator wiring and mission tests** — 3 runs, 2 respawns
  Repository: Rust workspace, engine crate `kranz-engine` at crates/engine.
  - `ac197c2` [f-2-1] checkpoint (engine commit)
  - `50ee6bc` [f-2-1] checkpoint (engine commit)
  - `7a3916b` [f-2-1] fix mission test assertions for milestone retag (commit sha parsing, exact tag-ref match)
  - ✓ lib tests under milestone_tag:: (at least 5) cover Created, AlreadyCurrent, Moved (previous = old commit, ref now at new HEAD), Failed with stale_tag None on a ref conflict, Failed with stale_tag Some on a blocked move, and is_failure only for Failed
  - ✓ milestone_retag_after_final_gate_fix_names_fixed_commit: mission Complete; the second milestone.completed has retagged_from == the first tag_commit and tag_commit == the tag's peeled commit; exactly one kranz/<id>/* tag; the fix commit is reachable from the tag and not from the first target; the tag is annotated and its message contains 'completed again'
  - ✓ milestone_retag_real_tag_failure_completes_untagged: with a conflicting kranz/<id>/ms-1/blocker tag the mission still completes, milestone.completed has tag, tag_commit and stale_tag all None, and refs/tags/kranz/<id>/ms-1 does not exist
  - ✓ the existing happy-path tag test and the final-gate non-waivable test still pass
  - ✓ re-completion logs at info with 'milestone completed again'; only real failures warn

### Milestone 3 — Worktree coverage and operator surfaces ✅

- ✅ **Worktree-mode re-completion test** — 1 run
  Repository: Rust workspace, engine crate `kranz-engine` at crates/engine.
  - `033253b` [f-3-1] add worktree-mode re-completion/retag test
  - ✓ milestone_retag_in_worktree_mode_leaves_primary_untouched passes: mission Complete, tag moved to the second tag_commit, retagged_from equals the first tag_commit, exactly one kranz/<id>/* tag
  - ✓ the primary checkout's branch and HEAD are unchanged and there are no tracked modifications after the run
  - ✓ the existing worktree tag and primary-untouched tests still pass
- ✅ **tail display of retag and stale tag, CHANGELOG entry** — 1 run
  Repository: Rust workspace.
  - `6741262` [f-3-2] show retag/stale-tag detail in tail, document in CHANGELOG
  - ✓ at least 3 milestone_tag_display_ tests pass, covering moved-tag, stale-tag and plain tagged renderings
  - ✓ CHANGELOG.md Unreleased has a Fixed entry describing tag moves on re-completion, the three new optional fields, untagged-on-failure honesty, and that no code reads the tag to pick a commit
  - ✓ clippy is clean for the kranz package

## Validation history

### ms-1 round 1 — Tag primitives and additive event fields

- [critical] a14 — orchestrator.rs:7401-7410 `tag_milestone` still calls `self.active_repo().tag(&name, "kranz milestone complete")` (the pre-existing, non-retagging primitive) and on any Err (including 'tag already exi… [truncated]

Disposition: waived.
- a14: This finding is premature, not a defect. Under the approved plan, ms-1 only adds the git primitives (f-1-1) and the new event fields set to None at every emission site (f-1-2, whose spec says to leave tagging behaviour unchanged). The change the finding asks for is exactly what the next milestone's feature, f-2-1, specifies: the new milestone_tag.rs module, a tag_milestone that uses tag_commit and retag, info-level 'milestone completed again' logging, and the outcome filling tag, tagCommit, retaggedFrom and staleTag at both completion sites. a14 has no milestone, so it is judged against the full mission diff at the final gate, and a3 and a4 cover the same behaviour once ms-2 lands. A fix-feature now would duplicate f-2-1 and conflict with it in orchestrator.rs. If f-2-1 does not deliver this, ms-2 validation and the final gate will catch it.

### ms-2 round 1 — Re-completed milestones move their tag

No findings.

### ms-3 round 1 — Worktree coverage and operator surfaces

No findings.

## Contract outcomes

- ✅ **[a1]** The git layer can read the commit an annotated tag names (or report that the tag is absent) and can move an existing annotated tag to HEAD while keeping it annotated. Both refuse flag-shaped names without running git. *(command: `cargo test -p kranz-engine --test git_ops_test milestone_tag_ref_`)*
- ✅ **[a2]** A pre-field `milestone.completed` event, with only `milestoneId` and optionally `tag`, still deserializes and folds. The new `tagCommit`, `retaggedFrom` and `staleTag` fields round-trip under those camelCase keys and are omitted from the JSON when unset. *(command: `cargo test -p kranz-engine --test reducer_test milestone_tag_fields_`)*
- ✅ **[a3]** Milestone tag outcome classification against real temporary git repos. A first completion creates the tag. A re-completion at a new HEAD moves the tag and reports the previous commit. A re-completion at the already-tagged commit leaves the tag alone. A ref conflict fails with no stale tag. A failed move leaves the old tag and reports it as stale. Only the failure outcome is classified as a warning. *(command: `cargo test -p kranz-engine --lib milestone_tag::`)*
- ✅ **[a4]** End-to-end (checkout mode): a mission whose milestone completes, is reopened by a final-gate finding, gets a fix commit and completes again ends Complete. Exactly one `kranz/<id>/ms-1` tag exists. It names the commit recorded as `tagCommit` on the second `milestone.completed`, and the fix commit is reachable from it. The second event records `retaggedFrom` equal to the first event's `tagCommit`. A real tag failure (a ref conflict) completes the milestone and the mission untagged, with no `kranz/<id>/ms-1` ref. *(command: `cargo test -p kranz-engine --test mission_test milestone_retag_`)*
- ✅ **[a5]** End-to-end (worktree mode): the same reopen-and-complete-again scenario moves the tag to the fixed commit. The primary checkout's branch, HEAD and tracked tree are unchanged. *(command: `cargo test -p kranz-engine --test worktree_isolation milestone_retag_`)*
- ✅ **[a6]** `kranz tail` renders the three new states of a `milestone.completed` event: tagged, tag moved from an earlier commit, and untagged with a stale tag. *(command: `cargo test -p kranz milestone_tag_display_`)*
- ✅ **[a7]** Anti-vacuity for milestone 1: the new git_ops and event-field filters each match real tests (at least 3 and at least 2), and no test with those name prefixes existed at the base commit. *(command: `n1=$(cargo test -p kranz-engine --test git_ops_test milestone_tag_ref_ -- --list 2>/dev/null | grep -c ': test$') && test "$n1" -ge 3 && n2=$(cargo test -p kranz-engine --test reducer_test milestone_tag_fields_ -- --list 2>/dev/null | grep -c ': test$') && test "$n2" -ge 2 && ! git grep -q -E 'fn (milestone_tag_ref_|milestone_tag_fields_)' $KRANZ_BASE_SHA -- crates`)*
- ✅ **[a8]** Anti-vacuity for milestone 2: the mission-test filter matches at least 2 tests and the lib filter at least 5. The `milestone_tag` module and the `milestone_retag_` tests did not exist at the base commit. *(command: `n1=$(cargo test -p kranz-engine --test mission_test milestone_retag_ -- --list 2>/dev/null | grep -c ': test$') && test "$n1" -ge 2 && n2=$(cargo test -p kranz-engine --lib milestone_tag:: -- --list 2>/dev/null | grep -c ': test$') && test "$n2" -ge 5 && ! git cat-file -e $KRANZ_BASE_SHA:crates/engine/src/milestone_tag.rs && ! git grep -q -E 'fn milestone_retag_' $KRANZ_BASE_SHA -- crates`)*
- ✅ **[a9]** Anti-vacuity for milestone 3: the worktree filter matches at least 1 test and the tail display filter at least 3. Neither existed at the base commit. *(command: `n1=$(cargo test -p kranz-engine --test worktree_isolation milestone_retag_ -- --list 2>/dev/null | grep -c ': test$') && test "$n1" -ge 1 && n2=$(cargo test -p kranz milestone_tag_display_ -- --list 2>/dev/null | grep -c ': test$') && test "$n2" -ge 3 && ! git grep -q -E 'fn (milestone_retag_|milestone_tag_display_)' $KRANZ_BASE_SHA -- crates/engine/tests/worktree_isolation.rs crates/cli`)*
- ✅ **[a10]** The existing tag, final-gate and worktree behaviour still holds: the happy-path tag test, the final-gate non-waivable fix test, the multi-milestone worktree tag test and the primary-untouched worktree test all pass. *(command: `cargo test -p kranz-engine --test mission_test -- happy_path_completes_mission_with_tag_and_contract_gate command_assertion_at_final_gate_is_non_waivable && cargo test -p kranz-engine --test worktree_isolation -- multi_milestone_worktree_mode_preserves_a1_a6_a7 primary_checkout_untouched_in_worktree_mode`)*
- ✅ **[a11]** The full reducer integration suite still passes with the new event fields. *(command: `cargo test -p kranz-engine --test reducer_test`)*
- ✅ **[a12]** Event and type shapes change only additively: `types.rs` is byte-identical to the base, and the `events.rs` diff removes no lines. *(command: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/types.rs && ! git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs | grep -q -E '^-([^-]|$)'`)*
- ✅ **[a13]** The workspace is formatted and every target, including all test files that construct `milestone.completed`, compiles clippy-clean. *(command: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a14]** Logging: the expected re-completion paths (tag moved, or tag already on HEAD) log at info level with wording that says the milestone was completed again. Only real git failures log at warn. Tag failures never fail or block the mission. Every `milestone.completed` emitted on a normal completion path accurately describes the ref: `tag` and `tagCommit` are set only when the ref names HEAD at completion, `retaggedFrom` only when the tag was moved, and `staleTag` whenever the milestone completed untagged and a ref of that name may still exist. The skip-milestone path is unchanged apart from carrying the new fields as None. *(agent judgement)*
- ✅ **[a15]** CHANGELOG.md's Unreleased section has an entry describing the new tag behaviour and the three new event fields. It also states that no engine, CLI, server or dashboard code resolves the milestone tag to pick a commit: merge detection uses branch tips, and resume and the report ignore the field, so only humans checking out the tag see the difference. `tag_milestone`'s doc comment no longer claims a pre-existing tag downgrades to None. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
