---
state: open
title: Keep orchestrator repair-budget evidence current after config changes
priority: 1
schedule: once
---

## Goal

Make each orchestrator decision receive the effective repair allowance from
durable state, so an old planning observation cannot silently become the
budget used to justify later repairs or waivers.

## Context

The unattended acceptance mission `m-53a35b` passed on source `d1eafe0`, but
its two waiver rationales cited an exhausted two-cycle budget after the CLI's
`--max-cycles 3` override had been accepted. See
`docs/reviews/2026-09-06-repair-limit-investigation.md` for exact event and
transcript references. The planner read the default two from state and saved
it in research; event 38 changed the current cap to three. Every injected
digest omitted that cap. Both milestones completed with two cycles used.

The engine's `findings::fix_cycle_exhausted` already uses the current config.
The defect is missing current policy evidence at the decision boundary,
which is within Kranz's governance remit. Reuse `digest::render` and the
existing control/reducer/reseed paths. Do not add prompt routing, autonomous
prompt optimization, or a new memory layer.

**Scope**

- Render the current per-milestone cap, cycles used, and remaining rounds
  from folded state on normal, single-shot, and reseeded execution turns.
  Remaining rounds saturate at zero when a cap is lowered below usage.
- State that current policy supersedes planning/research observations.
  Exhaustion bounds repair attempts; it does not establish that a finding
  is cosmetic or that the contract is met.
- Preserve accepted config-change events, strict cap enforcement, and
  non-waivable final-gate/Flight Rules checks. Never rewrite old transcripts
  or silently reset cycle usage to make the numbers fit.
- Decide whether a CLI override should also become visible before initial
  planning, retaining its audit trail and enqueue-only semantics. Correct
  execution context must not depend on restarting a session.
- Avoid dumping the whole config into a prompt. No persisted schema change
  is needed for the current cap and usage fields.

## Acceptance hints

- A deterministic mock mission plans with cap two, accepts a config change to three, and reaches findings conversion after two rounds. Capture the actual next injected message: it must identify three allowed, two used, and one remaining, superseding the older observation.
- A third requested repair round is emitted under cap three; a fourth is refused without creating extra repair features. A waiver must not be synthesized by the engine merely because budget is exhausted.
- Lowering the cap below rounds already used renders zero remaining and keeps the engine blocked for further requested repairs; it never underflows or resets history.
- A restarted/reseeded orchestrator and the single-shot path receive the same current allowance from event replay. The resume path must not restore the planning cap.
- A failing non-waivable command assertion stays blocked or enters the existing authorized repair/escalation path despite a model waiver. Existing Flight Rules authorization boundaries remain intact.
- Use non-vacuous test names after checking for collisions; run all four Rust workspace gates and platform CI before closing this release follow-up. The old live receipt remains pinned to its original SHA.
