---
state: done
state-note: Done: pack.rs + toml-subset parser, packDir config (additive), final-gate pipeline floor-then-pack, prompt injection with honest hash, kranz pack lint, zz- fixture pack + 4 mission fixtures, docs/pack-contract.md. pack_contract filter: 31 green; full gates green.
title: Pack contract extension — packs register gates, prompts, checklists
priority: 1
schedule: once
blocked-by: [gate-plugin-interface]
---

## Goal
Packs may register gates, prompts, checklists, and artefact-store adapters
through a declared, validated contract — extending the existing
packaging/gascity pack concept rather than adding a second mechanism.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-313). This series
carries the IP boundary as well as extensibility: kranz core stays
domain-free; domain knowledge ships in private packs. The existing pack
(pack.toml schema 2, docs/gascity-citizenship.md) is stub-verified only and
has never run against a live supervisor — so contract validation here must
be fully local (`kranz` lints a pack directory), requiring no City
infrastructure. Kranz remains fully usable standalone with no pack present.
Registered model-judged gates enter the ladder behind deterministic gates
by construction (gate-plugin-interface); a pack can add gates to the floor,
never lower or replace it.

## Acceptance hints
- A synthetic example pack registering one deterministic gate and one prompt
  loads and its gate runs in a mission fixture; all example vocabulary is
  synthetic.
- A pack with an invalid declaration fails closed at load, naming the field.
- No pack ⇒ behavior identical to today (regression).
- Pack-registered gates cannot displace or precede engine floor gates (test).
- Anti-vacuity grep on a named filter unique to this work.
