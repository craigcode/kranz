---
title: Mandatory containment's .kranz carve-out over-reads operator material
priority: 2
schedule: once
---

## Goal
Narrow the mandatory validator containment .kranz carve-out (224fa73/11bd1de): authority_read_deny_paths covers only serve tokens + config.json, so a hostile validator inside the wrap can still read .kranz/domain-terms.local (the plaintext lint vocabulary the clean-room design says must never be readable) and other gitignored runtime under .kranz. Either narrow the carve-out to what validation actually needs or extend the read-deny list to cover sensitive .kranz paths. 14th-pass review finding.

## Context


## Scoping answers

## Acceptance hints
