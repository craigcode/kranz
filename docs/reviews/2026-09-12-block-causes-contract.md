# contractChangeRequest: typed milestone block context

Status: implementation authorized by the user's approval of all six reliability enhancements on 2026-09-12.

Both `milestone.blocked` and `milestone.unblocked` gain an optional `blockContext` object. Existing `reason`, `milestoneId`, and `validatorGuidance` fields retain their meaning and wire shape. No state snapshot fields or transitions change.

```json
{"blockContext":{"owner":"workspaceGate","cause":"workspaceCheck"}}
```

`owner` names the recovery flow: `workspaceGate`, `engine`, `operator`, or `unknown`. `cause` names the block or resolution category: `workspaceCheck`, `grant`, `secretScan`, `contractBug`, `fixCycleCap`, `untrustedValidator`, `validatorTamper`, `reviewerIndependence`, `validation`, `authentication`, `operator`, or `unknown`. Engine-owned causes require the existing operator/orchestrator recovery path. Operator contexts identify operator-mediated unblock decisions; the model may interpret the operator's steering as before.

Missing `blockContext` is the legacy event shape and retains the historical message-prefix/exact-message fallback. A present object is authoritative. Missing members and future enum values deserialize as `unknown`; explicit null or malformed object members are rejected. Such values must never fall back to prose. Only the exact `workspaceGate`/`workspaceCheck` pair permits the existing automatic workspace recovery or exclusion of its lift from human-intervention metrics. The latest block/unblock event still determines ownership. Reason text remains for display, unchanged.

All production emitters populate context. Test fixtures using absent context continue to cover old logs. Contract-health aggregation uses typed causes when present, adding counters for newly distinguished causes, and retains its historical classifier for legacy events. Provenance and corpus-export human-decision classification use the same authoritative lift classifier as escalation metrics.

Validation covers absent-field round trips; unknown/missing members; null refusal; contradictory legacy-looking prose; changed display wording; later unrelated blocks; metric classification; and legacy reducer replay. Workspace-wide regression gates remain required before delivery.

## Related accounting correction

Per-run `WorkerSpawned.backend` / `WorkerRun.backend` already exists. Accounting now prefers it for token grouping and fallback pricing; reported `costUsd`, including explicit zero, remains authoritative. No new accounting schema is introduced. For old runs with no backend evidence, outcomes retains creation-config fallback and calibration retains folded-config fallback, exactly preserving each consumer's previous historical estimates. If no config exists, the original Claude default remains. This deliberately does not fabricate precise backend attribution for old logs.
