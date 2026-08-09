---
title: 14th-pass review minors: confirm-on-pass counting, pre-billing match, outcomes double-scan, state parse split, hook-guard stdin
priority: 3
schedule: once
---

## Goal
Sweep the five minor 14th-pass findings in one pass: (1) confirm-on-pass judgment-only path records {confirmed:[], disagreements:[]} and undercounts opportunities (4a84034); (2) pre-billing phrase match is a loose substring over unparsed stdout/stderr (3f1900e); (3) outcomes comparison metrics double-scan every mission log (d88dec2); (4) InvalidState soft-fails in read_state while parse hard-fails — pick one posture (3973269); (5) hook-guard stdin read is unbounded while hook-status reads are bounded (815da3b). The doc half of the sixth minor (a58465a catalog doc) is already fixed; its warn-only reconcile posture stays as-is unless a finding says otherwise. Each fix small and separately tested.

## Context


## Scoping answers

## Acceptance hints
