---
title: Commit the Tauri lockfile and bring the desktop shell into gates (P2)
priority: 3
schedule: once
---

## Goal
The Tauri workspace has no committed lockfile and sits outside root Cargo
gates/Dependabot/CI, so its dependency tree is unauditable (as the review
notes). Commit Cargo.lock for src-tauri, add it to workspace checks (at
minimum check+audit), and fix the free-port race (listener released
before the server binds).
