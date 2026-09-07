---
title: Design and validate production distribution for the Even G2 companion
priority: 2
schedule: once
state: open
state-note: "QR sideload implementation is separate; production connection and installed beta acceptance remain open."
---

## Goal

Turn the experimental G2/R1 companion into an installable Even Hub app without
exposing Kranz's full serve authority or weakening its Host, CORS and token gates.

## Scope to decide first

Use [the distribution brief](../../docs/even-g2-distribution.md) to choose
operator-hosted HTTPS or an outbound managed relay, credential scope and
lifetime, permission origins, background recovery and operational ownership.
The owner wants the Even feature at Kranz go-live; whether that includes a store
app or explicitly experimental source/sideload must be named in the release.

## Acceptance

- Reviewed production connection design; the packaged app reaches a real
  disposable Kranz host without the Vite development proxy.
- Pairing, expiry and revocation; no baked-in token, token URL, ambient
  credential relay or wildcard origin policy.
- Exact network permission origins and an honest denied/offline experience.
- Fresh review after reconnect; bounded decisions remain legible and duplicate
  or stale sends cannot silently expand authority.
- Actual package notices and secrets inspected; source commit and digest pinned.
- Installed beta receipt: five-minute phone lock, system exit, permission
  denial, relaunch, and an ordinary Kranz decision event followed by real
  mission progress. Physical QR acceptance remains a separate ticket.
- Separate owner publication approval. Do not change repository visibility,
  enable release publishing or submit an Even Hub build as part of preparation.
