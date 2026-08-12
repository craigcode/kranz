---
state: done
state-note: Done: domain_lint.rs + kranz domain-lint [--seed-config] + trusted-base CI workflow; salted-hash policy (no readable terms), path-scoped waivers, 616 files scanned green. domain_lint filter: 16 green; full gates green. Follow-up: operator seeds private vocabulary.
title: Clean-room CI lint — core fails on domain vocabulary
priority: 1
schedule: once
---

## Goal
A mechanical lint over kranz core (code, comments, docs, tickets, fixtures,
example config) that fails when consumer/domain vocabulary appears, wired as
a CI job and a local CLI surface — making the domain-free boundary auditable
instead of aspirational.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-314). The denylist must
not itself leak the vocabulary it bans: store salted hashes of banned terms
(matching is normalize → hash → compare), with the plaintext list kept
outside the repo. Follow the secret-scan precedent for shape: engine-side
check + CLI command + CI workflow (.github/workflows/secret-scan.yml), with
a documented waiver path. Scope: core crates, apps, docs, tickets — mission
runtime artifacts (.kranz/missions/) are excluded as operator content.
Inventory note: the tree was grepped clean on 2026-07-29 (zero hits), so the
first full run is expected GREEN — the deliverable is the standing guard,
not a cleanup.

## Acceptance hints
- A seeded synthetic banned term in a scratch fixture trips the lint, naming
  file and line; removing it goes green.
- The committed lint config contains no readable domain terms (test asserts
  hash-form entries only).
- CI job runs on the same tree scope as the local command; both green on the
  current tree.
- Anti-vacuity grep on a named filter unique to this work.
