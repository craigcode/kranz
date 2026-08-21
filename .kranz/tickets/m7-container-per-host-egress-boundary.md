---
state: done
state-note: Docker internal-network enforcement shipped with an authenticated dual-homed relay, private no-bind credential volume, structured per-run denials, verified teardown and stale-owner recovery; protected Ubuntu job 96861680799 proved allowed and denied CONNECT behavior plus a failed direct-socket bypass on SHA 95a87357.
title: Enforce container per-host egress with a non-bypassable network boundary
priority: 2
schedule: once
---

## Goal

Replace the intentionally refused container `fs+net` + non-empty-egress mode
with a hard boundary, such as an internal network plus filtering sidecar, so a
container process cannot bypass the allowlist by ignoring proxy environment
variables.

## Constraints

- Keep the current refusal until the boundary is demonstrably non-bypassable.
- Preserve `--network none` for empty egress.
- Attribute denial records to one run and keep grant application extend-only
  and auditable.

## Acceptance hints

- A direct-socket bypass to a disallowed host fails even after proxy variables
  are removed.
- An allowed CONNECT succeeds; a disallowed CONNECT produces the structured
  denial consumed by the existing egress-grant flow.
- Teardown removes all per-run networks, sidecars, and credentials after
  success, failure, timeout, and crash recovery.
- Linux live proof is mandatory; Windows support follows the provider decision
  in `m7-windows-containment-parity`.

See [the committed Docker live-proof receipt](../../docs/reviews/m7-container-per-host-egress-live-proof.md).
