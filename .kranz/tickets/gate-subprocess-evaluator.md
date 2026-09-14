---
state: open
title: External gate evaluator — contained JSON-RPC subprocess and pack registration
priority: 1
schedule: once
blocked-by: [gate-evaluation-contract-v1]
---

## Goal

Run a generic external checker against pinned evidence through a versioned,
bounded subprocess contract and register it through the existing pack system.

## Context

S3 of docs/scoping/acp-worker-gate-contract.md. Extend the existing gate/pack
seams; deterministic command gates and Flight Rules remain supported.

## Scope

- One process/evaluation, one NDJSON gate/evaluate request and terminal response;
  stdout is protocol, stderr diagnostic. Validate schema and correlation.
- Add an explicit compatible pack declaration/version for executable + argv,
  stages and evidence needs; reject unsupported declarations at load.
- Resolve checker code and dependencies from approved trusted content, including
  script bytes rather than only a command-string hash.
- Reuse command_exec environment/sandbox/tree supervision with structured I/O
  and an asynchronous driver. No human wait inside synchronous Gate::evaluate.
- Restrict reads to evaluator inputs and writes to snapshot/build/output roots.
  Import output artifacts with no-follow traversal, caps and redaction.

## Acceptance hints

- A synthetic checker implemented outside Rust returns an accepted result.
- Pass JSON plus nonzero exit, duplicate/missing response, timeout, overflow,
  wrong evaluation/evidence ID and traversal all fail a blocking evaluation.
- The gate cannot read authority tokens, mutate the candidate/main, select its
  own stage/kind/enforcement or run worker-substituted checker code.
- Existing command-pack behavior, deterministic-before-model order, engine
  floors, advisory posture and standards waivers are unchanged.
- Cancellation kills descendants; unknown versions fail before judging.
- Contract tests use unique nonvacuous names; full workspace gates pass.

## Out of scope

Plugin daemons, remote protocols, a marketplace and domain-specific checkers.
