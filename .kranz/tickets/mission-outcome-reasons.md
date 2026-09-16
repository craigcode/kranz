---
state: open
title: Mission outcomes — distinguish defective work, environment blocks and human boundaries
priority: 2
schedule: once
blocked-by: [acp-governed-mission-acceptance]
---

## Goal

Explain why work did not advance by projecting recorded causes into the existing
outcomes report. Separate defective output, environment prerequisites and
deliberate human/policy boundaries without replacing mission states or altering
permission, retry and merge behavior.

## Context

The operator authorized this follow-up on 2026-09-16. Vercel describes success,
flawed, blocked and manual runs in its
[software factory article](https://vercel.com/blog/building-a-software-factory-for-ai-sdk).
Use that distinction as reporting guidance, not a replacement Kranz state machine
or a mandate to automate away every human step.

Start after S7 (governed mission acceptance); S5 (stage/evidence integration)
and S7 are defined in the
[ACP/gate implementation sequence](../../docs/scoping/acp-worker-gate-contract.md#delivery-slices-and-effort).

Reuse `outcomes.rs`, its task-class rows and escalation ledger, S5's evaluation
results and resolutions, and the append-only export paths. Authentication being
unavailable and an independent checker rejecting a defect need different next
actions; a safety-policy denial must not be labelled missing credentials.

**Out of scope**

Automatic prompt optimization, new agent roles or pools, autonomous remediation,
new execution states, policy downgrades, and performance claims based on closure
or PR counts alone.

## Scoping answers

- Build a deterministic, versioned mapping from explicit recorded causes to operator-facing reason categories. Keep original event, stage, attempt, mission status, actor attribution and detailed reason reachable.
- Separate evidence-backed defective output, missing environment prerequisites, and deliberate human/policy boundaries. Preserve unknown, interrupted, cancelled, mixed and currently pending cases instead of forcing every record into Vercel's four buckets. Do not infer cause from free-text error matching or an LLM summary when authoritative structured evidence is absent.
- Distinguish individual attempts from eventual mission outcome. A repaired defect remains in history even if the mission later completes; a completed code mission does not prove a release, deployment or operational cutover.
- Report counts and denominators by existing task class and selected time window. Overlapping reasons must be labelled so totals do not imply mutually exclusive populations. Missing causes remain unknown rather than zero-filled.
- Keep this a read-only fold and display extension. Where current events lack a necessary fact, specify an additive `contractChangeRequest` at the owning event producer and retain honest unknowns for older logs. Do not invent a second outcome store, provision credentials, retry work or weaken a policy.

## Acceptance hints

- Fixtures distinguish missing authentication, a genuine failed assertion, explicit human approval pending, a nonwaivable denial, and process interruption. None is promoted to success or to a retry/grant by the reporting operation.
- An initially blocked run later completed retains the earlier cause and reports the current state separately. A repair history cannot become several completed missions in the denominator, and multiple reasons cannot silently double-count.
- Missing/ambiguous legacy records produce unknown with provenance. Replaying the same events under the same mapping version produces identical data and starts no checks, provider calls or permission responses.
- Existing outcomes fields, task-class grouping and status semantics remain compatible. CLI and API outputs agree; add the existing dashboard presentation where appropriate and run the corresponding full validation gates.
