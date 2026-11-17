---
title: Replace chrono and remove the advisory waiver
priority: 2
schedule: once
---

## Goal

The workspace no longer depends on `chrono`, directly or through its own crates, and the waiver for RUSTSEC-2026-0543 is gone from both advisory configs.

## Context

RUSTSEC-2026-0543 was issued on 2026-11-17. It is informational: `chrono` is unmaintained, all versions are affected, and no patched version exists. It reports no vulnerability and no unsound code. It names `jiff` and `time` as alternatives.

The advisory checks fail on any advisory, so every pull request failed `supply-chain` (`cargo deny check`) and `tauri` (`cargo audit --deny warnings` in `apps/dashboard/src-tauri`). The operator waived that one advisory on 2026-11-17 so work could keep merging. The waiver is one entry in `deny.toml` and one in `apps/dashboard/src-tauri/.cargo/audit.toml`. Any other advisory still fails, including a later one against `chrono`.

At the time of the waiver `chrono` was used at 308 call sites in 111 files across the engine, CLI, server and Slack crates. It produces the timestamps in the event log, mission state, tickets and reports.

## Scoping answers

- Flag as a D-X operator decision which crate replaces it. State the trade-offs in the plan: maintenance status, formatting and parsing coverage, time zone handling, and what each adds to the dependency tree and the license list in `deny.toml`.
- The wire format does not change. Every timestamp kranz has already written must read back to the same instant, and every timestamp it writes must be byte-identical to what `chrono` wrote for the same instant. That covers `events.jsonl`, `state.json`, queue receipts, ticket front matter, reports and the REST and WebSocket payloads.
- The event log's hash chain covers serialized events. A change in how a timestamp is rendered would break verification of existing logs. Prove it does not with existing logs as fixtures.
- Contract files (`events.rs`, `types.rs`) change additively only. A change of field type that keeps the serialized form is still a change to review as a contract change.
- Check whether a dependency kranz does not control still pulls in `chrono`. If one does, say so: the waiver can only be removed when the lockfiles no longer contain it.
- Do it on a quiet main. The change touches most files and will conflict with every open branch.
- Remove both waiver entries in the same change, and restore the sentence in `audit.toml` that says no maintenance advisory is ignored.

## Acceptance hints

- `cargo deny check` and both `cargo audit` runs pass with the waiver entries removed.
- `cargo tree -i chrono` reports no match in the root workspace and in `apps/dashboard/src-tauri`.
- A test reads event logs written before the change and verifies their hash chain.
- A test fixes a set of instants and asserts the serialized timestamp for each is byte-identical to the recorded `chrono` output, including sub-second precision and the UTC suffix.
- Anti-vacuity: the test filter matches only the new tests.
