---
title: Egress grants — sandbox egress-denial instrumentation (prerequisite)
priority: 3
schedule: once
---

## Why this is blocked (feasibility finding 2026-07-13)

Egress grants would extend the grant-request decision flow to sandbox egress
denials: a worker/validator blocked from reaching a host parks for an operator
approve/deny → extend the mission's egress allowlist. The park/approve/deny
machinery already exists (see the shipped `GrantKind` flow — command + touch
grants). The BLOCKER is the trigger: no egress-denial signal reaches the
engine today.

`crates/engine/src/sandbox.rs` uses OS-level containment (macOS Seatbelt SBPL
`network-outbound` allowlist; Linux bubblewrap). A blocked connection is a
kernel-level failure delivered to the sandboxed process as a generic connection
error — NOT a structured event carrying the destination host back to the
engine's event stream. Unlike command denials (a `tool_result` the runner
correlates) or touch-set writes (the out-of-contract sweep names the path),
nothing wired into the engine says "egress to `<host>` was denied." macOS does
log denials OS-side (see the research below), but that signal is untapped and
fragile, and Linux has none at all — so the grant flow has nothing to trigger
on today, and can't name the destination the operator would grant.

## Feasibility research (2026-07-14) — the signal EXISTS on macOS, not on Linux

Detail behind the opener's caveat: what signal each OS offers and what it would
take to tap it. Confidence is mixed; verify each against current OS docs before
building.

**macOS (Seatbelt) — an observable signal exists, but fragile.** Seatbelt writes
every denial to the unified log under a `Sandbox:` prefix, format
`Sandbox: <process>(<pid>) deny(1) <operation> <detail>` — for a blocked
connection the operation is `network-outbound` and the detail carries the
destination. So the engine COULD tail `log stream --predicate 'eventMessage
CONTAINS "Sandbox: "'`, parse the `network-outbound` denials, and correlate by
pid to the run. Caveats that make this real work: the log format is not a stable
API; pid→run correlation is fragile (workers spawn subprocesses); `sandbox-exec`
is Apple-deprecated; and our own `sandbox.rs` currently REFUSES `fs+net` on macOS
because Seatbelt's allow side only takes `*`/`localhost` (per-host `(remote tcp …)`
allow rules exist on some versions but aren't what we emit today). So macOS needs
both a per-host egress profile AND a log-tailing denial reader.

**Linux (bubblewrap) — no signal; needs new enforcement infra.** bwrap isolates
network via a namespace: it's all-or-nothing (`--unshare-net` = no network), with
NO per-host allowlist and NO per-destination deny logging. A blocked connection
is a bare `connect()` error. Getting per-destination egress denials requires
building egress *enforcement* first — realistically a userspace filtering proxy
(route worker traffic through it, it logs blocked hosts) or a cgroup/socket eBPF
hook. Both are substantial, and the proxy is probably the portable path (works on
macOS too, sidestepping the Seatbelt fragility).

## Recommended sequence (the actual work)

1. Build per-host egress ENFORCEMENT with a structured denial signal — most
   likely a filtering egress proxy shared by both platforms (portable, gives a
   clean `host + run-id` denial event, avoids unified-log/log-tailing fragility).
2. Surface the denial on `RunOutcome` (mirror `denied_commands`).
3. THEN the grant flow is a small addition: a fourth `GrantKind` (`egress`) that
   extends the mission's egress allowlist on approval, reusing
   park/approve/deny/timeout/cap and all four surfaces.

Sources for the macOS findings:
- <https://theapplewiki.com/wiki/Dev:Seatbelt>
- <https://github.com/microsoft/mxc/blob/main/docs/macos-support/seatbelt-backend.md>
- <https://github.com/michaelneale/agent-seatbelt-sandbox>

Deferred from the grant-request-decision-flow work (see
`grant-request-decision-flow.md`), where command + touch-set grants shipped and
egress was scoped out as blocked-on-infra.

## Progress (2026-07-28) — step 1 (3.3a) shipped

The filtering egress proxy + denial signal landed
(`crates/engine/src/egress_proxy.rs`): one CONNECT-only tokio proxy per
`fs+net` run, per-host allowlist enforced at CONNECT time (DEFAULT_EGRESS +
configured `egress[]` + the mission's `egress_grants`, a new additive
`Mission` field read where grant extension will write it), structured
`{"ts","host","port"}` denials fsynced to `runs/egress-denials.jsonl`, and
`RunOutcome.denied_egress` surfacing them. macOS `fs+net` now resolves to
Seatbelt with a loopback-only egress profile (the proxy is the only way
out); container `fs+net` with a non-empty egress list routes the bridge
through the host-side proxy (`--network none` kept for an empty list).
Explicitly still OUT: bwrap (netns cannot reach a host proxy without veth
plumbing), the macOS unified-log tail (proxy makes it unnecessary),
plain-HTTP forward, per-run attribution via Proxy-Authorization, and the
grant flow itself — steps 2–3 of the sequence (a fourth `GrantKind` that
extends `egress_grants` on approval) are 3.3b.

## Progress (2026-07-28) — steps 2–3 (3.3b) shipped

The grant flow landed, completing this ticket. A fourth `GrantKind`
(`egress`, serde `"egress"`; `#[default]` stays `command` so old logs
parse) parks a sandboxed run whose untrusted outcome carries
`denied_egress`: the orchestrator's validator path offers a grant naming
the first refused `host:port` BEFORE burning the retry, reusing the same
park/approve/deny/timeout/cap machinery verbatim. Approval extends
`Mission.egress_grants` (extend-only, logged — never `command_grants`,
`touch_set`, or `deny_exceptions`), which the re-run's proxy allowlist
already reads (3.3a's `apply_egress_grants` fold); denial blocks the
milestone with the egress reason, same refusal semantics as a command
grant. All four decision surfaces render the kind distinctly: CLI tail
(`egress grant requested/approved/denied`), REST (kind flows through the
snapshot serde; approve/deny endpoints are kind-agnostic), the dashboard
grant panel (its own blurb + `GrantKind` union arm), and the Slack grant
card (egress blurb + field label). The mock backend grew a
`connects_via_proxy` seam so the macOS e2e drives the REAL proxy path:
scripted CONNECT → 403 + denial record → park → approve → re-run's spec
carries the granted host in its allowlist. The block-reason classifier
(`contract_health`) counts an egress-denied block as a grant denial.
Still OUT (unchanged from 3.3a): bwrap denial signal, unified-log tail,
plain-HTTP forward, per-run attribution, and a worker-path egress park
(validators only — the worker's `result != pass` gating and respawn
budget coupling are a deliberate separate decision if ever needed).
