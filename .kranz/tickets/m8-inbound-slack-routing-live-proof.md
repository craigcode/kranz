---
state: done
state-note: A human `/kranz status` event crossed the live Socket Mode bridge on 2026-08-15, routed by the exact configured workspace/channel mapping to kranz-proof while two repositories were healthy, and returned that repository's distinctive 6-captured/211-landed status. Temporary config and host were removed.
title: Observe a real inbound Slack command route through the multi-repository host
priority: 1
schedule: once
blocked-by: []
---

## Goal

Close M8's final literal live-QA gap by sending a harmless human Slack command
through the authenticated Socket Mode bridge while at least two repositories
are healthy, and observe the command route to exactly the intended repository.

## Constraints

- Use a read-only command such as `/kranz status`; do not approve, queue, run,
  merge, or otherwise spend from the proof.
- Resolve the current workspace/channel immediately before sending.
- Preserve the operator's global host and Slack configuration byte-for-byte;
  any temporary catalog or route entry must be removed after the receipt.
- Do not record tokens, message bodies unrelated to the proof, or private
  workspace content.

## Acceptance hints

- One host reports at least two healthy repository ids and one connected Slack
  bridge.
- A real inbound workspace event is observed in the bridge log and produces a
  threaded or channel response naming/showing the expected repository state.
- A second check proves ambiguity still fails closed when no explicit/channel/
  thread affinity applies, without triggering a mutation.
- The committed receipt records timestamps, redacted repository/channel ids,
  routing outcome, and cleanup.

## Receipt

See [the inbound Slack routing proof](../../docs/reviews/m8-inbound-slack-routing-proof.md).
