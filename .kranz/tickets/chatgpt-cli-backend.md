---
title: Absorb the ChatGPT CLI as a dispatch adapter (gpt-5.6-sol)
priority: 2
schedule: once
state: open
---

## Goal

Add a sibling `AgentBackend` for the ChatGPT CLI aimed at `gpt-5.6-sol`,
reusing the Cursor probe → parser → picker path. Probe first: auth,
print-mode event structure, model/cost capture. Then implement the
adapter. Do not start until the probe is written down.

## Context

`docs/roadmap-options.md` still listed this in the same "Now" line as
Cursor Grok 4.5. The Cursor half shipped
(`backend-cursor-direct-parser`, `cursor-cli-grok45-backend-probe`,
`cursor-cli-live-capture-route-decision`). The ChatGPT CLI half was
never ticketed.

This is a dispatch adapter under the positioning ADR's retained
`AgentBackend` seam, not an in-harness execution primitive. Confirm the
exact CLI binary and model id at probe time (post-cutoff; do not vouch
a string from memory).

## Acceptance hints

- A probe note records auth, print-mode events, and cost/model capture
  against the installed CLI, or records why the CLI is not a backend
  candidate (same bar as the Cursor probe).
- If the probe passes: a `backend_chatgpt` (name as fits crate
  convention) implements the backend trait; picker/config expose it;
  tests use a fixture stream, not a live account.
- Scrutiny/validator pairing follows existing non-Claude rules. Do not
  widen sandbox enforcement to a backend that cannot honor it.
- Anti-vacuity on a unique filter once implementation starts.
