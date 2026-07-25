---
title: Thin remote WorkspaceProvider adapter (Coder-shaped substrate)
priority: 3
schedule: once
blocked-by: [workspace-provider-seam, workspace-provider-pin-at-approval]
---

## Goal
Ship a thin remote WorkspaceProvider adapter for a Coder-shaped (or
documented equivalent) substrate: provision from a pinned template/image,
surface preview/takeover URLs, inject secret *names* via the provider —
without building a VM scheduler or cloud IDE inside kranz.

## Context
Monaco bought Coder in ~2 engineer-weeks; design D-B / D-G. Prefer baked
images over nested Devcontainers for v1. Local missions remain valid
without this provider.

## Acceptance hints
- Remote provider selectable when configured; missing creds/template fail
  closed at provision/preflight.
- Pin-at-approval fields populated; previews/takeover in events + dashboard.
- No public-IP requirement documented; secrets never logged.
- Anti-vacuity grep on a named filter unique to this work.
