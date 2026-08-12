---
title: Flight Rules pack contract — canonical RFC and rule schema
priority: 1
schedule: once
state: done
state-note: Implemented — schema-4 [standards] root, strict bounded loader (pack/standards.rs), normalized manifest + sha256 digest, kranz standards lint [--against ref] with transition refusals, trust-aware pack lint. 27 flight_rules_contract_* tests; full workspace gates green.
---

## Goal
Extend the existing pack contract with a schema-4 standards root whose strict
RFC/rule Markdown loader produces one normalized, stable-sorted Flight Rules
manifest and content digest, plus a local lifecycle-aware lint command.

## Context
KRZ-341, first slice of `docs/scoping/flight-rules-engineering-standards.md`
(D-A through D-C, D-J). This extends `pack.rs`; it must not create a parallel
knowledge/check mechanism. Schema 2/3 and no-pack behavior remain byte-
identical. The structured frontmatter statement is canonical; RFC/rule prose
is rationale, not a second machine authority. Use a strict documented subset,
not the permissive knowledge-note parser.

House standards are prompt input and policy input. Traverse the configured
root capability-relative and no-follow, accept bounded regular UTF-8 files
only, and cap file bytes, file count, rule count, and total normalized bytes.
Loading/linting executes no checker or source text.

Contract requirements:

- Add optional `[standards] root = "..."` in pack schema 4.
- RFC metadata: stable `id`, title, owner, lifecycle status, optional RFC3339
  `effective-at` and supersession references.
- Rule metadata: stable `id`, positive revision, parent RFC, RFC 2119 level,
  active/retired status, one-line statement, domains, stages, optional
  path/task-class scopes, typed checker reference, and fail-closed `waivable`
  posture.
- IDs are pack-wide unique and never path-derived; normalized output ordering
  and hashing are deterministic. Governing bytes include referenced pack gate
  declarations, so changing a checker changes the standards digest.
- Lifecycle transition lint refuses absent/draft → enforced and a semantic
  rule change without a revision increment when checked against a base ref;
  known rule IDs cannot disappear, and retirement is a one-way tombstone.
- Draft rules may omit a checker; promotion to approved requires a valid typed
  binding so the advisory period is mechanically evaluable.

## Acceptance hints
- A synthetic pack with one RFC/two rules loads to the expected byte-stable manifest and digest; renaming a file preserves identity.
- Duplicate/disappearing IDs, tombstone reactivation, missing/unknown fields, invalid levels/statuses/stages, orphan rules, bad revisions/checkers, and illegal transitions fail closed naming the file/field.
- Symlinked parents/leaves, FIFOs/devices, invalid UTF-8, and every size/count cap fail promptly without following or unbounded reading.
- `kranz standards lint <pack> [--against <ref>]` reports rules and transition errors; `kranz pack lint` reports the standards registration.
- Existing schema-2/3 pack fixtures and no-pack missions are unchanged.
- An external/untracked pack may supply approved advisory rules but cannot activate enforced rules in this slice; the refusal names the trust remedy.
- Anti-vacuity: unique test filter `flight_rules_contract_` has no pre-existing collision and reports at least one passing test.
