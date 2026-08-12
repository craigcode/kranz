---
state: done
state-note: "Done: parse_routing_rules rejects an empty table fail-closed at the file surface (naming the file + remedy), so draft/approve fail exactly as for a present-invalid file; missing file keeps layered config byte-identical. routing_rules_config_empty filter: 2 green; full workspace gates green (14th-pass fix batch)."
title: An empty {} routing-rules file silently demotes layered routing to legacy
priority: 2
schedule: once
---

## Goal
Fail closed on an empty routing-rules table (bc3d4cf): today a MISSING .kranz/routing-rules.json keeps the layered config, but a present empty {} loads as Some(empty) and falls through to legacy task_class_to_tier — silently demoting a non-empty layered routing table if the file is emptied or truncated. Reject an empty rules table at validation (draft/approve fail closed, matching the present-invalid posture), or treat empty as None explicitly and loudly. 14th-pass review finding.

## Context


## Scoping answers

## Acceptance hints
