---
title: Prove Kranz Mission Control on Even Realities G2 glasses
priority: 3
schedule: once
state: open
state-note: "Simulator-ready thin client implemented; physical G2/R1 QR-sideload receipt remains human-gated."
---

## Goal

Prove the experimental `apps/even-g2` thin client on paired Even Realities G2
glasses and an R1 ring: observe a real disposable mission, review a bounded
human decision, deliberately confirm it, and see Kranz resume without moving
mission state or policy into the wearable.

## Context

The repository already has the right server boundary: mission summaries and
folded state are reads; structured question answers and grant decisions are
mutation-token-gated POSTs. The first client slice is simulator-ready and uses
a same-origin Vite proxy for development sideloads so Kranz's CORS, Host, and
token gates remain intact.

This ticket is a hardware receipt, not an invitation to make the glasses a
general control surface. Plan approval, merge, release, free-text answers, and
any mutation that cannot be fully reviewed on 576x288 stay out of scope.

Do not publish the `.ehpk` yet. A production Even Hub package needs a separate
HTTPS relay/origin design and must never embed or URL-encode a serve token.

## Acceptance hints

- `npm ci`, test, build, lint, and simulator start pass under `apps/even-g2`.
- Demo mode shows deterministic fixture missions without contacting Kranz.
- Live mode refuses to start unless the local `kranz serve --read-auth` returns
  401 to an unauthenticated catalog probe; it reaches Kranz only through the
  documented Vite `/api` proxy and sends the token only in
  `x-kranz-token`.
- A real G2/R1 can move through missions and choices, cancel confirmation by
  swiping, and exit by double-tap.
- A grant approve or deny requires review plus a distinct confirmation tap;
  a structured question sends the exact displayed option and index.
- A free-text question stays non-actionable and routes the operator to the
  dashboard or Slack.
- A disposable live mission resumes after the decision, and its event log
  records the ordinary Kranz decision event with no glasses-owned state.
- Record simulator and physical-device screenshots plus the Kranz event-log
  receipt before setting this ticket done.
