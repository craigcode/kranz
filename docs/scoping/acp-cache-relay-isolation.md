# ACP cache and relay isolation

Status: implemented 2026-11-01.

Ticket: [acp-cache-relay-isolation](../../.kranz/tickets/acp-cache-relay-isolation.md).
Related contracts: [ACP containment](../acp-containment.md),
[ACP recovery and credential lifecycle](acp-recovery-and-credential-lifecycle.md).

## Problem statement

Under the `-v1` single-operator host baseline:
1. Container workers mount the operator's read-only toolchain cache directories (`$RUSTUP_HOME`, `$CARGO_HOME/{bin,registry,git}`, `$NPM_CONFIG_CACHE`) so offline builds do not cold-bootstrap toolchains or registries into scratch. If the operator's host cache holds private registry crates or proprietary git checkouts, a worker container can read those cached sources.
2. The container egress relay (`container_egress::RELAY_SCRIPT`) joined both the per-session `--internal` network (`kranz-egress-<id>`) and Docker's default `bridge` network, and bound its TCP listener to `0.0.0.0:3128`. Unrelated containers on the default `bridge` network could therefore reach the relay listener IP on the `bridge` interface and share the session's allowlist during the run.

## Decisions (D-C1…D-C4)

| Decision | Choice | Consequence |
|---|---|---|
| D-C1: Relay listener binding | Bind the relay server in `container_egress::RELAY_SCRIPT` exclusively to `socket.gethostbyname("kranz-egress")` (the relay's IPv4 address on the per-session `--internal` network `kranz-egress-<id>`) rather than `0.0.0.0` | Unrelated containers on the default `bridge` network receive TCP `ECONNREFUSED` when probing the relay's bridge IP; only the worker on the owned `--internal` network can reach `kranz-egress:3128` |
| D-C2: Default `-v1` profile compatibility | Preserve the `-v1` profile definitions (`claude-acp-0.77.0-arm64-v1`, `codex-acp-1.11.0-arm64-v1`) and default container argv byte-for-byte when no isolated cache override is configured | Existing qualified profiles do not silently change their mount shape or require automatic migration |
| D-C3: Isolated toolchain cache opt-in | Support `KRANZ_CONTAINER_ISOLATED_CACHE_DIR` (`sandbox_container::ISOLATED_CACHE_DIR_ENV`) validated by `validate_isolated_cache_dir` | When set, `push_toolchain_caches` mounts only `<isolated>/{.rustup,.cargo/bin,.cargo/registry,.cargo/git,.npm}` read-only and never mounts the operator's `$HOME` / `$CARGO_HOME` / `$RUSTUP_HOME` / `$NPM_CONFIG_CACHE` |
| D-C4: Fail-closed cache validation | Refuse relative paths, symlinks, paths overlapping `global_authority_dir()`, or any isolated cache containing `.cargo/credentials.toml` or `.cargo/credentials` | An invalid or credential-bearing isolated cache root mounts zero toolchain caches rather than falling back to the operator's `$HOME` |

## Verification

- `container_egress::tests::relay_script_binds_only_to_internal_worker_network_alias` verifies that `RELAY_SCRIPT` resolves and binds `kranz-egress` rather than `0.0.0.0`.
- `container_egress::tests::container_per_host_egress_live_proof` proves that a worker on the owned `--internal` network tunnels through `kranz-egress:3128` while a peer on the default `bridge` network is refused (`RELAY-ISOLATED: default bridge peer refused`).
- `sandbox_container::tests::isolated_toolchain_cache_replaces_operator_cache_and_rejects_credentials` and `sandbox_container::tests::container_cache_probe_documents_readable_sources_and_denied_writes` prove both the `-v1` read-only cache baseline and the isolated cache posture where the operator's cache paths are absent inside the container.
