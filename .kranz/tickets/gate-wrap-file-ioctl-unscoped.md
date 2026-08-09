---
title: Gate sandbox wrap grants unrestricted (allow file-ioctl) to every macOS gate
priority: 2
schedule: once
---

## Goal
Scope the file-ioctl allowance added for pty support (dfe3bcf, command_exec.rs ~567-571): today every sandbox-wrapped engine-run gate gets unrestricted (allow file-ioctl), not just pty sessions. Restrict it to /dev/ptmx plus the tty path regex, apply it only where pty is actually needed, and pin the scoped shape in resolve_matrix so a future widen fails loudly. Note the composition audit (8b5fc81) predates this widen, so audit green does not cover it. 14th-pass review finding.

## Context


## Scoping answers

## Acceptance hints
