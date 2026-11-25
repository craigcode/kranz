---
state: done
state-note: "Mission m-e7b464. A milestone completed again now moves its tag to the commit that passed. `milestone.completed` records the new commit, the commit the tag left, and a stale tag name when a real git failure leaves the completion untagged. Nothing in the engine reads the tag to pick a commit."
title: A milestone's tag stays on its first completion after the milestone is completed again
priority: 3
schedule: once
---

## Goal

After a mission finishes, the tag `kranz/<mission>/<milestone>` either names the commit at which that milestone last passed validation, or the record says plainly that it does not.

## Context

Seen on mission m-dd7d22 on 2026-11-24, milestone ms-2:

- seq 691: `milestone.completed` with `tag: kranz/m-dd7d22/ms-2`. The tag was created on commit `9742e3a1`.
- seq 703: a later validation finding reopened the milestone. A fix feature landed as commit `f80431e8`.
- seq 1056: `milestone.completed` again, with no `tag` field. The log line was `milestone tag failed; completing untagged ... tag 'kranz/m-dd7d22/ms-2' already exists`.

So the tag still points at `9742e3a1`, the commit that validation went on to reject, and the commit that finally passed has no tag. Anyone who checks out the tag to see "ms-2 as completed" gets the pre-fix tree.

This is the documented behaviour, not a crash. `tag_milestone` in `crates/engine/src/orchestrator.rs` says a pre-existing tag "downgrades to `None` rather than failing the mission". The second completion event is honest about being untagged. What is missing is any sign on the first tag that it was superseded, and any ref for the final state. The warning also reads like a failure when it is the expected path.

## D-X operator decision (DECIDED 2026-11-24: D-X3)

The operator chose D-X3: move the tag to the latest completion. The operator first chose D-X2 the same day and changed it after reading the drafted plan. The options are kept for the record.

- D-X1: leave the tag where it is. Record the supersession: the later `milestone.completed` carries the commit it completed at, and the report says the tag names an earlier completion.
- D-X2: write a second tag for each later completion, for example `kranz/<mission>/<milestone>.2`. Every completion keeps a ref and nothing is moved.
- D-X3: move the tag to the latest completion. This rewrites a ref the engine created. Tags are part of the audit trail, so this needs a stated reason.

The stated reason: under D-X2 the plain name `kranz/<mission>/<milestone>` keeps naming the tree that validation rejected, which is the complaint this ticket was filed for. Moving the tag makes the plain name always check out the validated tree.

No evidence is lost by the move. The later `milestone.completed` records the commit the tag left, and that commit stays reachable from the mission branch. The engine only ever moves a tag it created for the same mission and milestone.

## Scoping answers

- Whatever the decision, the mission never fails because a tag could not be written.
- Any new event field is additive with `#[serde(default)]`. `events.rs` and `types.rs` do not change shape otherwise.
- Tags are created in the mission worktree only. The primary checkout stays untouched.
- Log the expected "tag already exists" case at info level with wording that says the milestone was completed again. Keep the warning for real git failures.
- Check whether anything reads the milestone tag to pick a commit (reports, `merged.rs`, resume). If something does, name it in the pull request and say which completion it should see.

## Acceptance hints

- A mission test that completes a milestone, reopens it with a finding, and completes it again. It asserts that the tag names the later completion's commit, that the second `milestone.completed` records the tag and the commit the tag left, and that the earlier commit is still reachable from the mission branch.
- A test that a real tag failure still completes the milestone untagged.
- Anti-vacuity: the test filter matches only the new tests.
