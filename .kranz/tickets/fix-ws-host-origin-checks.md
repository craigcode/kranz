---
state: done
state-note: landed in f4ba469 (Craig); reviewed + negative-tested + gates green
title: WS + tokenless GETs lack Origin/Host checks (cross-site read exfiltration)
priority: 1
schedule: once
---

## Goal
cors_layer() is the only read-side defense but CORS does not cover WebSocket handshakes: ws_handler (crates/server/src/ws.rs:54) upgrades after checking only safe_id + events-file existence, never Origin — so any page a victim visits while kranz serve runs can open ws://127.0.0.1:4560/api/missions/<id>/ws and receive the full snapshot (entire MissionState) + live event stream (mission ids are only m-+6 hex = 24 bits). Separately, no Host header is validated, so DNS rebinding (attacker.com -> 127.0.0.1) makes every tokenless GET same-origin-readable, including run transcripts that can carry source/secrets. Mutations stay safe (token + JSON preflight); impact is read-only exfiltration but it defeats the documented read defense. Fix: validate Origin (reuse origin_allowed) on the WS upgrade, and add middleware asserting Host in {127.0.0.1:<port>, localhost:<port>} on all requests.

## Context
Found by a security code review 2026-07-07 (HEAD 0740072). Local-service security; the read-side defense the docs claim. Reviewer verified line-by-line.

## Acceptance hints
- WS upgrade rejects a disallowed/absent Origin; a Host not in the localhost allowlist is rejected on all routes; existing same-origin dashboard + tokened flows unaffected. cargo test -p kranz-server green.
