---
state-note: S2 basic compatibility implemented and live-proven for pinned Claude/Codex adapters; PR #60 review and downstream containment/mission acceptance remain separate.
state: done
title: ACP adapters — prove released protocol, authentication and report compatibility
priority: 1
schedule: once
---

## Goal

Establish a reproducible compatibility baseline for the maintained Claude and
Codex ACP adapters under Kranz's actual cleared environment and session contract.

## Context

S2 of docs/scoping/acp-worker-gate-contract.md. KRZ-301 is done;
backend_acp.rs already speaks ACP v1. Current worker-only/opt-in restrictions
remain until evidence justifies an explicit change.

## Scope

- Pin released adapter plus underlying runtime versions and schema references;
  distinguish released behavior from main-branch documentation.
- Capture redacted initialize/new/prompt/update/result fixtures, permission
  option shapes, report boundaries and process lifecycle for each adapter.
- Prove explicit authentication using minimal credentials/private native state;
  never restore ambient environment or HOME as a workaround.
- Record engine ID versus peer session ID, capabilities, selected model/mode,
  actual report parsing and truthful missing-cost behavior.
- Correct stale resume/spec assertions based on current stable documentation.
- Provide a bounded live runbook and readiness diagnostics; live model calls
  require their own operator-authorized budget and fixture.

## Acceptance hints

- Fixture tests cover handshake rejection/timeouts, malformed/oversized frames,
  foreign session IDs, unknown methods, report truncation and absent telemetry.
- No configured model label is misrepresented as peer-confirmed selection.
- Cancellation reaches process-tree termination even when peer stdin is stuck;
  descendants do not survive peer death or normal completion.
- Permission fixtures include changed raw input, missing action identity,
  durable-only options and unknown option IDs.
- Receipts distinguish fixture proof, live proof and untested adapter versions;
  a runbook alone is not a live compatibility pass.

## Out of scope

Client fs/terminal services, same-feature resume, ACP validator roles and
automatic backend promotion.

## Completion evidence

The pinned Claude and Codex adapters both passed the bounded native-login
text/report probe on macOS arm64. The runbook, redacted live receipts, released
source hashes and full installation lock are retained under
`docs/acp-compatibility.md` and `docs/compatibility/acp/`. The five-axis self-review
and regression results are in `docs/reviews/2026-09-14-acp-compatibility.md`.
PR #60 remains subject to review; this closes S2 implementation and basic
compatibility proof, not downstream permission, containment or governed-mission
acceptance.
