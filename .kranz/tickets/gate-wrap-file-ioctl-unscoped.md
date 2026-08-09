---
state: done
state-note: "Done: gate-wrap file-ioctl scoped to (literal /dev/ptmx) + regex ^/dev/tty[p-t][0-9a-f]+$ — never the bare (allow file-ioctl); live-probed on macOS arm64 (openpty/termios/TIOCSWINSZ/read/write chain passes, dropping the line EPERMs). resolve_matrix pins the scoped shape and the bare allow's absence; gate_profile_extras_scopes_file_ioctl_to_pty_devices pins the pure profile. Full workspace gates green."
title: Gate sandbox wrap grants unrestricted (allow file-ioctl) to every macOS gate
priority: 2
schedule: once
---

## Goal
Scope the file-ioctl allowance added for pty support (dfe3bcf, command_exec.rs ~567-571): today every sandbox-wrapped engine-run gate gets unrestricted (allow file-ioctl), not just pty sessions. Restrict it to /dev/ptmx plus the tty path regex, apply it only where pty is actually needed, and pin the scoped shape in resolve_matrix so a future widen fails loudly. Note the composition audit (8b5fc81) predates this widen, so audit green does not cover it. 14th-pass review finding.

## Context


## Scoping answers

## Acceptance hints
