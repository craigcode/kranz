# `backend_cursor` implementation brief

Route decision: **direct-parser**. Decided 2026-07-09 by feature f-1-4, closing out
the probe work started in f-1-1 (re-verification), f-1-2 (text/json output
shapes), and f-1-3 (write-capable stream-json fixture). Full evidence lives in
`probe-result.json` (see `.model_matrix`, `.permission_posture`,
`.recommendation_note`) and `preflight.md`. This document is the actionable
summary for whoever builds `backend_cursor`.

## Why direct-parser, not ACP, not defer

- The Cursor CLI's `--print --output-format json` (single result object) and
  `--output-format stream-json` (event stream) are stable, self-describing,
  parseable structures on their own. There is no need to speak Cursor's Agent
  Client Protocol to get structured tool-call/tool-result/usage data — it is
  already on the wire in plain JSON.
- The uncertainty that justified deferring in earlier probes (an account with
  zero provisioned models, making model-selection and model-availability
  failures indistinguishable) is gone: this account now has a populated
  ~190-model catalog and invalid model ids fail cleanly and diagnostically
  (see `model_matrix.invalid_live_2026_07_09`).
- Permission posture (mode / force / sandbox) is fully controllable and
  observable through CLI flags plus output parsing — no ACP round-trip is
  needed to enforce or detect a read-only vs. read-write turn.

## Adapter shape

- Invoke via `agent --print --output-format stream-json --workspace <dir> --model <id> [mode/force/sandbox flags] "<prompt>"`.
  Prefer `stream-json` over `json` for a live backend so tool-call progress is
  observable incrementally; fall back to `json` only for simple one-shot
  calls where only the final result matters.
- Parse events by `type`/`subtype`: `system/init`, `user`, `tool_call/started`,
  `tool_call/completed` (result nests under `.result.success` or
  `.result.failure` — a discriminated union, branch on which key is present),
  `assistant`, `result/success` (terminal; carries `usage`).
- Cost: no dollar-cost field is on the wire. Compute cost client-side from
  `usage.{inputTokens,outputTokens,cacheReadTokens,cacheWriteTokens}` against
  a locally maintained per-model price table keyed by the `--model` id used.
- Model id validation: pass the id straight through; on exit code 1 with a
  `Cannot use this model: <id>. Available models: ...` message (no
  `session_id`/`usage` present, i.e. pre-billing), surface that as a
  configuration error to the caller rather than retrying — no turn was
  billed.

## Role mapping (validator vs. worker)

| Flag | Effect (observed unless noted) | validator (read-only) | worker (read-write) |
|---|---|---|---|
| `--mode ask` | model refuses file/shell writes at the turn level | yes | no |
| `--mode plan` | model produces a plan, no writes observed | yes | no |
| default mode (no `--mode`) + `--trust` | writes proceed with no interactive prompt in headless `--print` | no | yes |
| `--force` | writes/shell commands proceed unprompted (observed) | no | yes |
| `--yolo` | documented alias of `--force` (inferred from `--help`, not separately live-tested) | no | yes |

Use `--mode ask` or `--mode plan` for any kranz validator role that must never
mutate state. Use default mode or `--force`/`--yolo` only for a worker role
that is explicitly authorized to write.

## Sandbox: do not treat as an isolation boundary

`--sandbox enabled` was tested live against two probes — outbound network
access (`curl` to a public URL) and writing a file outside the declared
`--workspace` directory (to `/tmp`) — and neither was blocked. `--sandbox
disabled` produced identical results. This is an observed negative result in
this environment/account/OS (macOS/darwin), not a claim that no sandbox
mechanism exists anywhere in the CLI; other restriction categories (e.g.
specific denylisted binaries) were not probed.

**Consequence for `backend_cursor` and kranz's no-push/no-publish/no-main-write
invariants: do not rely on `--sandbox enabled` to enforce those invariants.**
Isolation must come from external controls that are independent of this flag:
run worker/validator turns against throwaway `--workspace` directories (never
the mission repo), scope any credentials/tokens the process can reach so a
`git push` or publish step is not reachable even if attempted, and rely on
process-level sandboxing (container/VM) if stronger isolation is required.

## Known gaps for whoever picks this up next

- `--worktree` was intentionally never exercised (spec required `--workspace`
  only, to avoid mutating `~/.cursor/worktrees` state); if a future feature
  wants worktree-based isolation instead of plain `--workspace`, that flag's
  behavior is still unobserved.
- `--yolo` was not separately live-tested (relied on `--help`'s explicit
  "alias for --force" documentation) — low risk, but flagged as inferred, not
  observed, per this feature's labeling requirement.
- `--sandbox`'s negative result covers exactly two probes (network egress,
  filesystem write outside workspace) — it is not an exhaustive audit of
  every possible sandbox-relevant action.
