---
state: done
state-note: Done at 7ae58be (+fmt b3dd2e9): validators (both roles) carry exactly Bash(printenv KRANZ_*) — bare printenv/env/echo stay denied; both validator prompts document the sanctioned form. Gate test validator_env_reads_allow_kranz_printenv_only green. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
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

1. **Allow exactly one introspection form**: `printenv <KRANZ_…>` — the
   variable NAME must start with `KRANZ_` (exact-prefix rules matching the
   existing pattern idiom, e.g. `Bash(printenv KRANZ_BASE_SHA*)` plus the
   literal-prefix `Bash(printenv KRANZ_*)` for engine vars added later).
   Nothing wider:
   - **No bare `printenv`** and **no `env`**: a full env dump writes the
     backend's injected auth key (ANTHROPIC_API_KEY et al. — injected into
     the session env post-sanitization by design) into the transcript,
     which is otherwise sanitized. `env <cmd>` is also a command RUNNER
     (`env ls` executes `ls`), so `env*` is arbitrary shell with extra
     steps.
   - **No `echo`**: `echo $(…)` is command substitution — an allow rule
     for echo is an allow rule for the substituted command.
   The KRANZ_-prefixed vars are engine-injected by construction
   (KRANZ_BASE_SHA and friends), so name-prefixing is what makes the
   disclosure set exactly the deliberate set.
2. **Document in the validator prompts** (prompts/validator-*.md) that
   `printenv KRANZ_*` is the sanctioned way to inspect the session env,
   and that grant requests are for actual capability boundaries, so models
   stop parking on trivia.
3. **Explicitly out of scope**: widening any other command class. The
   4th-pass narrowing stands everywhere else.

## Test gate

- `cargo test --workspace validator_env_reads 2>&1 | grep -qE 'test result: ok. [1-9]'`
  — `printenv KRANZ_BASE_SHA` matches an allowed rule; bare `printenv`,
  `env`, `env <anything>`, and `echo $KRANZ_BASE_SHA` all stay DENIED
  (the auth-key-dump and command-runner/substitution shapes).
- Workspace gates green, bare exit codes, never piped.
