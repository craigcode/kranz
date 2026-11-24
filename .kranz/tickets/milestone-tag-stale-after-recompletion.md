---
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

## D-X operator decision

- D-X1: leave the tag where it is. Record the supersession: the later `milestone.completed` carries the commit it completed at, and the report says the tag names an earlier completion.
- D-X2: write a second tag for each later completion, for example `kranz/<mission>/<milestone>.2`. Every completion keeps a ref and nothing is moved.
- D-X3: move the tag to the latest completion. This rewrites a ref the engine created. Tags are part of the audit trail, so this needs a stated reason.

D-X2 keeps the most evidence and never rewrites a ref, so it is the suggested default.

## Scoping answers

- Whatever the decision, the mission never fails because a tag could not be written.
- Any new event field is additive with `#[serde(default)]`. `events.rs` and `types.rs` do not change shape otherwise.
- Tags are created in the mission worktree only. The primary checkout stays untouched.
- Log the expected "tag already exists" case at info level with wording that says the milestone was completed again. Keep the warning for real git failures.
- Check whether anything reads the milestone tag to pick a commit (reports, `merged.rs`, resume). If something does, name it in the pull request and say which completion it should see.

## Acceptance hints

- A mission test that completes a milestone, reopens it with a finding, and completes it again. It asserts the chosen behaviour: which refs exist, which commits they name, and what the second `milestone.completed` records.
- A test that a real tag failure still completes the milestone untagged.
- Anti-vacuity: the test filter matches only the new tests.
