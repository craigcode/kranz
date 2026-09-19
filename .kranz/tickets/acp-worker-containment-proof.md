---
state: open
state-note: Docker lifetime, pinned-image and hostile descendant proofs implemented; mount-proof dependency is closed. Basic contained Claude/Codex report checks and the separately authorized native shell batch now pass on macOS ARM64 / Colima, with one-time consent, exact file verification, one host feature commit, unchanged primary state and confirmed cleanup per provider. See docs/compatibility/acp/native-tool-proof-v2.json and docs/reviews/2026-09-19-acp-native-tool-pass.md. Earlier denied-egress, permission-encoding, cleanup and synthetic failures remain retained. Native Linux vendor receipts, qualified ordinary mission admission and S7 remain open; no new provider calls are authorized by these receipts.
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
- Follow the remaining admission breakdown in docs/reviews/2026-09-19-acp-linux-preparation.md; Linux ARM64 provider-free preparation passes, but the new live batch, credential/startup integration and ordinary mission admission remain pending.

- Out of scope: Reimplementing a sandbox, client filesystem/terminal RPC services, ACP validators, credential inheritance or changing defaults without proof.

## Acceptance hints

- A hostile peer bypassing every ACP callback still cannot write outside its permitted roots, read serve tokens, mutate protected git metadata or perform forbidden egress/push effects.
- Shell, direct I/O and nested/MCP children are included in probes.
- Abort and parent death reap descendants, including a peer blocking stdin.
- A normal fixture worker produces a real feature commit inside its worktree; primary checkout bytes and protected refs remain unchanged.
- Unsupported enforcement fails before spawn; sandbox-off keeps its explicitly documented posture and is never advertised as enforced.
- Full workspace gates pass and live receipts name adapter/runtime/platform.
