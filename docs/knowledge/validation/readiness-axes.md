---
title: The two readiness axes — AMM projection and contract/consent health
owner: agent
freshness: check-on-touch
last_verified: 2026-07-23
verified_against:
  - crates/cli/src/ready.rs
  - crates/cli/src/amm.rs
  - crates/engine/src/contract_health.rs
  - .kranz/tickets/amm-readiness-projection.md
---

## Why two axes

`kranz ready` reports readiness on two axes whose source of truth stays
kranz-native:

1. **The AMM projection** (crates/cli/src/amm.rs) — the Factory Autonomy
   Maturity Model's L1–L5 as a *derived view* over the native ready.rs
   dimensions. This exists so people who think in the whitepaper's
   vocabulary can read the scorecard; it is a mapping, never an adoption.
2. **Contract/consent health** (crates/engine/src/contract_health.rs) — the
   axis the whitepaper cannot see: approval-time contract-lint pass rate,
   validation-finding waivers per mission, and the milestone.blocked cause
   histogram (grant / secret-scan / contract-bug / fix-cycle-cap /
   untrusted-validator / other), folded from mission event logs.

The second axis exists because the evidence says so: every hard bite in the
validator-repair era (m-9e4ef3's five blocks, the a7 hang, the m-66aff8
substring-collision false green) came from the contract/consent machinery —
author-broken assertions, scan friction, grant stalls, vacuous gates — not
from the foundation signals (tests, CI, gates) the paper measures. A
scorecard that omits it measures everything except the thing that hurts.

## Mapping rules

- **Map, never adopt.** Factory's 5/19/36/60/100 point thresholds are
  marketing numbers that drift whenever the paper revises. They live in ONE
  table (amm.rs `LADDER`); levels are ordinal labels; `MAPPING_VERSION`
  bumps on any change so `--json` consumers can pin.
- **L4+ requires the second axis.** A repo cannot reach "orchestrated"
  without mission history (the contract-health axis must exist to be read),
  and L5 requires the machinery to be *quiet*: lint pass ≥ 0.8, waivers per
  mission ≤ 0.5, zero contract-bug blocks. Those thresholds are kranz-native
  — ours to tune, not Factory's.
- **No second source of truth.** contract_health is a pure projection over
  events the engine already records (orchestrator.decision headlines,
  milestone.blocked reason templates). It measures nothing new.
- **Absent, not zero.** Repos without mission history omit the axis
  (`contractHealth` is skipped in JSON) — a vacuous zero would be a lie.

## Dogfood

Running `kranz ready` on this repo scores it against both axes — the
"already ahead of the AMM" claim from the whitepaper review is only as good
as this output. Check it before repeating the claim in public.
