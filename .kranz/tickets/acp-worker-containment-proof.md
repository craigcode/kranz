---
state: open
state-note: Bounded live Claude and Codex missions passed consent, checks, exact-tree local merge and export with scripted controller/reviewers; the earlier failed Claude fixture remains retained. Fresh independent branch review found one orphan-process reaping bug, now fixed and independently re-reviewed with a real-Docker regression. Local workspace gates pass (3064 tests); corrected-revision CI and stacked integration remain before closure. See docs/reviews/2026-09-20-acp-independent-review.md and docs/reviews/2026-09-20-acp-governed-live-preparation.md.
title: ACP containment — wrap adapters and prove the complete descendant boundary
priority: 1
schedule: once
blocked-by: [acp-adapter-compatibility-proof, container-mount-proof-owned-cleanup]
---

## Goal

Support enforced ACP worker execution only for adapter/platform combinations
whose filesystem, authority, network and process boundaries have been proven.

## Context

S6 of docs/scoping/acp-worker-gate-contract.md. Generic ACP continues to declare
no sandbox enforcement; the qualified worker profiles provide the explicit exception. Client fs/terminal capabilities and permission callbacks
are not containment. Follow D-F/D-H and reuse sandbox.rs/command_exec primitives.

## Scoping answers

- Wrap the adapter, underlying runtime and all descendants at spawn.
- Bound workspace, metadata, private auth/session home, server-token reads and network according to the resolved policy. Preserve env_clear.
- Prove required adapter authentication/caches work without ambient authority.
- Keep no-push/no-primary-write invariants and post-worker integrity checks.
- Expose tested adapter/platform support explicitly; keep refusal for unproven combinations. Start macOS, then Linux; Windows needs proof before enablement.
- The admission breakdown in docs/reviews/2026-09-19-acp-linux-preparation.md is implemented by the explicit profiles in docs/acp-containment.md. The live scope and limits remain in docs/reviews/2026-09-20-acp-linux-native-tool-pass.md; ordinary mission integration and review are in docs/reviews/2026-09-20-acp-worker-profile-admission.md.

- Out of scope: Reimplementing a sandbox, client filesystem/terminal RPC services, ACP validators, credential inheritance or changing defaults without proof.

## Acceptance hints

- A hostile peer bypassing every ACP callback still cannot write outside its permitted roots, read serve tokens, mutate protected git metadata or perform forbidden egress/push effects.
- Shell, direct I/O and nested/MCP children are included in probes.
- Abort and parent death reap descendants, including a peer blocking stdin.
- A normal fixture worker produces a real feature commit inside its worktree; primary checkout bytes and protected refs remain unchanged.
- Unsupported enforcement fails before spawn; sandbox-off keeps its explicitly documented posture and is never advertised as enforced.
- Full workspace gates pass and live receipts name adapter/runtime/platform.
