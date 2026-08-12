---
state: done
state-note: "Done: 4 of 5 fixed — confirm-on-pass judgment-only now records the opportunity (additive judgmentOpportunity on ValidationConfirm, engine-core commit); pre-billing match line-anchored on parsed output (cursor-backend commit 692d92f); outcomes comparison reuses the memoized fold (no second log scan); hook-guard stdin bounded to STDIN_PAYLOAD_MAX_BYTES, fail-open like the rest of the lane. Item 4 (read_state soft-fail vs parse hard-fail) investigated: deliberate two-layer design — parse hard-errors so a mistyped terminal state can never silently re-enter the ready path, read_state stays total so work/merged/Slack reads never brick on someone else's typo; pinned by ticket_state_frontmatter_invalid_state_is_a_hard_parse_error. Not a bug. Full workspace gates green."
title: "14th-pass review minors: confirm-on-pass counting, pre-billing match, outcomes double-scan, state parse split, hook-guard stdin"
priority: 3
schedule: once
---

## Goal
Sweep the five minor 14th-pass findings in one pass: (1) confirm-on-pass judgment-only path records {confirmed:[], disagreements:[]} and undercounts opportunities (4a84034); (2) pre-billing phrase match is a loose substring over unparsed stdout/stderr (3f1900e); (3) outcomes comparison metrics double-scan every mission log (d88dec2); (4) InvalidState soft-fails in read_state while parse hard-fails — pick one posture (3973269); (5) hook-guard stdin read is unbounded while hook-status reads are bounded (815da3b). The doc half of the sixth minor (a58465a catalog doc) is already fixed; its warn-only reconcile posture stays as-is unless a finding says otherwise. Each fix small and separately tested.

## Context


## Scoping answers

## Acceptance hints
