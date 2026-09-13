---
state: done
state-note: Done at 5d6e9f6: src-tauri Cargo.lock committed (507 pkgs), macos tauri CI job (cargo check --locked, npm dist first for generate_context!), Dependabot cargo entry, port race fixed (listener kept through handoff). Left to CI: tauri job first run. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
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
