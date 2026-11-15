---
title: Stop command output being read as a permission denial
priority: 2
schedule: once
---

## Goal

A tool result is marked as a denial when its text contains certain words, even when the command ran and succeeded. Make the denial signal depend on the result being a refusal, not on what the command printed.

## Context

Found on 2026-11-14 in mission `m-373b57`. A scrutiny validator ran a read-only `git diff` on `crates/slack/src/inbound.rs`. The command succeeded and returned the diff. The diff text contained the words "hook" and "block", because that file handles Slack block actions and webhooks. The result was marked as denied, the command was recorded as a denied command, and the milestone parked for an operator grant. Nobody answered within the hour and the mission blocked.

The same command on other files in the same session was not marked, because their diffs did not contain both words.

The classifier is in `parse_user` in `crates/engine/src/backend_claude.rs`. Most of its clauses require `is_error`. The hook clause and the output-redirection clause match on substrings alone.

Its doc comment records the trade-off that was chosen: a false positive parks a visible, deny-by-default grant request, and a false negative silently aborts a session. That trade-off still holds. This ticket narrows the false positive without reopening the false negative.

## Scoping answers

- Read `docs/knowledge/validation/child-containment.md` and the gate signal policy before changing the classifier.
- A result that is not an error and whose text is the command's own output is not a denial. Find out what a real hook block and a real output-redirection block look like in the stream (is `is_error` set, is there a fixed prefix) and match on that shape. Use recorded transcripts from past missions as fixtures.
- If a real hook block cannot be told apart from command output by structure, say so in the ticket discussion and flag the choice as a D-X decision. Do not weaken the signal on a guess.
- The other backends have their own classifiers. Check them for the same substring pattern and report what is found; fix only what a test demonstrates.
- No contract file change is expected.

## Acceptance hints

- A test feeds a successful tool result whose text contains "hook" and "block" and asserts it is not marked as denied.
- Tests for the existing real denial shapes still pass unchanged.
- A test built from the recorded m-373b57 result asserts no denied command is recorded for it.
