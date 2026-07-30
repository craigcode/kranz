---
title: "Validators: read-only KRANZ_* env reads shouldn't park for a grant"
priority: 2
schedule: once
---

# Validators: read-only KRANZ_* env reads shouldn't park for a grant

Motivation: mission m-83d1ed (2026-07-30). A functional validator ran
`printenv KRANZ_BASE_SHA` — a trivial, safe env read — outside the narrowed
validator Bash allow-set, parked for an operator grant, and the milestone
BLOCKED on deny-default while no operator was watching. The contract the
validator was checking literally uses `$KRANZ_BASE_SHA`, so needing the
value is a normal, expected validator behavior, not a capability boundary.

## Problem

The validator Bash allow-set (crates/engine/src/permissions.rs,
`command_allow_patterns`) was narrowed (4th-pass hardening) to the verbatim
contract command plus its exact `&&`/`||`/`;`/`|` segments — correctly
closing the `python3 -*` widening. But it is now TOO narrow in one
predictable spot: a validator that needs to *inspect* the env the engine
gave it (`KRANZ_BASE_SHA` and friends) has no sanctioned way to read it
except the contract command itself, and any env-read command (`printenv
X`, `echo $X`, `env | grep`) parks the milestone for a grant — which
deny-defaults into a block when the operator is away. A park for a
read-only env read is consent-boundary noise: nothing about it can affect
the checkout or the world.

## Design (locked)

1. **Allow a fixed set of read-only env introspection forms** for
   validators, alongside the contract commands: `printenv`, `printenv
   <NAME>`, `env`, and `echo`-of-env forms — expressed as exact prefix
   rules (`Bash(printenv*)` style matching the existing pattern idiom),
   NOT a wildcarded shell. The narrowest form that covers the incident is
   `printenv` (bare and with names); `env` is included for parity (same
   read-only class).
2. **No variable-name filtering**: the session env is already the
   sanitized set (agent_env.rs) — anything readable there is already
   deliberate. No secrets ride the validator env by construction, so
   allowing env reads adds no disclosure surface (unlike allowing
   arbitrary file reads).
3. **Document in the validator prompts** (prompts/validator-*.md) that
   env introspection is allowed and grant requests are for actual
   capability boundaries, so models stop parking on trivia.
4. **Explicitly out of scope**: widening any other command class. The
   4th-pass narrowing stands everywhere else.

## Test gate

- `cargo test --workspace validator_env_reads 2>&1 | grep -qE 'test result: ok. [1-9]'`
  — a validator allow-set contains `printenv` forms and NOT arbitrary
  shell (e.g. no `Bash(curl*)`); the incident command `printenv
  KRANZ_BASE_SHA` matches an allowed rule and never produces a grant
  park.
- Workspace gates green, bare exit codes, never piped.
