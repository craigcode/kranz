# Research — m-9dc8c1

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/server/src/lib.rs
- crates/server/src/ws.rs
- crates/server/tests/server_test.rs
- crates/cli/src/cli.rs
- crates/cli/src/commands.rs
- crates/server/src/multi.rs
- apps/dashboard/src/lib/token.ts
- apps/dashboard/src/lib/api.ts
- apps/dashboard/src/lib/ws.ts
- apps/dashboard/src/components/TokenPrompt.tsx
- apps/dashboard/package.json
- docs/deploy.md
- docs/operator-gates.md
- .kranz/tickets/m6-read-side-auth.md

## External sources

- .kranz/tickets/m6-read-side-auth.md
- ~/Desktop/kranz-repo-review-main.md (finding 12/4, referenced by the ticket; not read directly)

## Facts

- The read-token gate, `?token=[REDACTED] and `/api/health` exemption already exist in middleware. — `crates/server/src/lib.rs `require_mutation_token` (needs_token computed from POST || (require_read_token && is_read); is_health exempt; query_ok only when is_read).`
- Read-auth is currently a bind-class side effect, and `bind_is_loopback` is derived as `!require_read_token` — conflating two independent concepts. — `crates/server/src/lib.rs: `let require_read_token = [REDACTED] in serve_multi_on_listener and `let bind_is_loopback = !require_read_token;` in router_with_multi_repo_host_and_addr.`
- The dashboard client already attaches the token to GETs (header) and parks read 401s on the token gate, and the WS client already appends `?token=`. — `apps/dashboard/src/lib/api.ts getJson (x-kranz-token header + `await awaitToken()` on 401); apps/dashboard/src/lib/ws.ts url() (`params.set('token', token)`).`
- An existing test simulates a LAN bind by passing require_read_token=true through a loopback-assuming constructor, so it relies on the conflation and must be migrated when the two bits are decoupled. — `crates/server/tests/server_test.rs `ws_lan_mode_accepts_ip_origin_and_native_clients_with_token` calls `router_with_shared_host_and_bind(host, None, Some(token), Some(4560), true)`.`
- The dashboard test script runs vitest once (no watch), so `npm --prefix apps/dashboard test` is a valid non-interactive contract command. — `apps/dashboard/package.json: `"test": "vitest run"`.`
- docs/deploy.md currently states read-auth is unbuilt and must ship before exposing a server. — `docs/deploy.md §5: `kranz serve` has no TLS and (today) no read-auth; "Until read-auth ships, do not expose a remote server…".`
- The CLI refuses non-loopback binds without --insecure-lan; read-auth on loopback must be additive to this, not a replacement. — `crates/cli/src/commands.rs `refuse_non_loopback_without_insecure_lan` and the Serve subcommand args in crates/cli/src/cli.rs (host/insecure_lan/token).`

## Ambiguities & stale docs

- Config surface: chose a `--read-auth` CLI flag over a persisted JSON config key (deploy shape is `docker run … serve --read-auth`); revisit if operators want it persisted in ~/.kranz/config.json.
- Reverse-proxy Origin/Host under a TLS proxy fronting a loopback serve is treated as a non-goal — WS origin policy stays keyed to the real bind class and the proxy must forward a loopback Host/Origin (documented in deploy.md).

## Candidate knowledge updates

- docs/knowledge: note that `bind_is_loopback` (Host gate + WS-origin regime) and `require_read_token` (read gating) are now independent bits threaded through the serve entry points — do not re-derive one from the other.
- docs/knowledge: record `--read-auth` as the shipped, config-gated read-auth mode and that off-loopback binds force it implicitly.
