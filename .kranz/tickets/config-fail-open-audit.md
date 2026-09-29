---
state: done
state-note: Done: docs/config-composition.md inventory (zero unmitigated fail-opens; F1-F6 dispositioned) + 14 regression tests locking composition semantics per surface (extends never replace, deny-wins, dangerous-prefix tripwire). composition_audit filter: 14 green; full gates green.
title: Audit config surfaces for fail-open composition footguns
priority: 2
schedule: once
---

## Goal
Sweep every kranz config/permission surface for three footgun classes and
lock the findings in with regression tests plus documented composition
semantics: (a) custom lists that REPLACE safer defaults instead of
extending them, (b) any flag that silently bypasses a deny/blocklist, and
(c) allow/deny precedence that is not deny-wins.

## Context
The 2026-08-04 Warp CLI documentation review prompted checks for two
composition hazards: an auto-approval path bypassing a denylist and a custom
list replacing safer defaults. Audit Kranz's own stated rules: deny precedence
(`permissions.rs`), fail-closed sandboxing, and missing/invalid merge-gates
refusal. Surfaces to inspect include role permission profiles and
worker deny-rule grants, sandbox enforce/extraWrite/egress plus egress
grants, secret-allowlist waivers, merge-gates and workspace-contract
validation, `--allow-unvalidated` and every `--dangerously-*` flag, the
Slack spend allowlist, and any auto-approve-shaped path in serve/exec.

## Acceptance hints
- A written inventory records each surface's verdict: extends-vs-replaces,
  bypass paths named, precedence direction.
- Regression tests assert: custom lists extend defaults wherever that is
  the documented contract; deny beats allow on every surface claiming
  precedence; no flag short-circuits a deny list unless its name carries
  the dangerously- prefix.
- Every fail-open finding is either fixed or recorded as an accepted
  exception with rationale — never left silent.
- Anti-vacuity grep on a named filter unique to this work.
