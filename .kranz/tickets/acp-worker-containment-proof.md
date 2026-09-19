---
state: open
state-note: Docker lifetime and pinned-image fixtures implemented; a newly authorized contained Codex report check passed with plugins disabled and the allowlist unchanged, preserving the first denied-egress failure. See docs/compatibility/acp/codex-contained-plugins-disabled-proof.json. Concurrent network-recovery and deletion-confirmation races are addressed in docs/reviews/2026-09-19-acp-concurrent-cleanup.md; the historical failure is retained. Mount-proof timeout cleanup is a new blocking follow-up. Claude proof, broader qualification, production admission and S7 remain open.
title: ACP containment — wrap adapters and prove the complete descendant boundary
priority: 1
schedule: once
blocked-by: [acp-adapter-compatibility-proof, container-mount-proof-owned-cleanup]
---

## Goal

Support enforced ACP worker execution only for adapter/platform combinations
whose filesystem, authority, network and process boundaries have been proven.

## Context

S6 of docs/scoping/acp-worker-gate-contract.md. ACP currently declares no
sandbox enforcement. Client fs/terminal capabilities and permission callbacks
are not containment. Follow D-F/D-H and reuse sandbox.rs/command_exec primitives.

## Scoping answers

- Wrap the adapter, underlying runtime and all descendants at spawn.
- Bound workspace, metadata, private auth/session home, server-token reads and network according to the resolved policy. Preserve env_clear.
- Prove required adapter authentication/caches work without ambient authority.
- Keep no-push/no-primary-write invariants and post-worker integrity checks.
- Expose tested adapter/platform support explicitly; keep refusal for unproven combinations. Start macOS, then Linux; Windows needs proof before enablement.

- Out of scope: Reimplementing a sandbox, client filesystem/terminal RPC services, ACP validators, credential inheritance or changing defaults without proof.

## Acceptance hints

- A hostile peer bypassing every ACP callback still cannot write outside its permitted roots, read serve tokens, mutate protected git metadata or perform forbidden egress/push effects.
- Shell, direct I/O and nested/MCP children are included in probes.
- Abort and parent death reap descendants, including a peer blocking stdin.
- A normal fixture worker produces a real feature commit inside its worktree; primary checkout bytes and protected refs remain unchanged.
- Unsupported enforcement fails before spawn; sandbox-off keeps its explicitly documented posture and is never advertised as enforced.
- Full workspace gates pass and live receipts name adapter/runtime/platform.
