---
state: open
title: Run the M7 hostile and overhead proof on a real Linux bubblewrap host
priority: 2
schedule: once
---

## Goal

Repeat the macOS M7 live receipt on Linux using the real bubblewrap process
provider: hostile filesystem and network attempts are denied, legitimate gates
stay green, the primary checkout stays untouched, and warm-command overhead is
measured rather than inferred from unit tests.

## Acceptance hints

- Run on a Linux host with bubblewrap installed; do not substitute mocked
  command construction.
- Under `enforce: fs+net`, a sibling write and outbound connection both fail and
  leave no external effect.
- A normal Node and/or Rust contract command passes under the wrapper.
- Record median wrapped/unwrapped warm timings and flag overhead above ~10%.
- Commit a receipt with host/runtime versions, commands, results, and cleanup.
