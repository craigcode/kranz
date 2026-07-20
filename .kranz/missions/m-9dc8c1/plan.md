# Mission plan — m-9dc8c1

**Goal:** Make read-side authentication a first-class, config-gated mode of `kranz serve` — GETs and the WS upgrade require the token on any bind class when the mode is on — without regressing single-operator localhost UX, and document it as the shipped M6 read-auth story.

Branch `kranz/mission-m-9dc8c1` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$7.53 – $37.63** (expected ~$16.39). Rough estimate — live usage is authoritative; based on 41 completed mission(s).

## Considered alternatives

**Chosen approach:** Make read-auth an explicit, independent boolean threaded from a `--read-auth` CLI flag through the serve entry points into the router, decoupling it from the real bind-class bit (`bind_is_loopback`) that drives the Host gate and WS-origin regime. This reuses the already-built middleware token gate (header + `?token=` reads, `/api/health` exemption) and the already-built client token plumbing, so the surface area is the plumbing/decoupling, tests, and docs — not new auth mechanics.

Rejected shapes:
- **Persist read-auth as a JSON config key in ~/.kranz/config.json / HostConfig instead of a CLI flag.** — Adds a new config-parsing surface and precedence rules for little gain — the deploy shape is `docker run … serve --read-auth`, so an invocation flag matching the existing `--insecure-lan`/`--token` style is sufficient and simpler.
- **Keep read-auth derived from bind class and teach the server to trust the reverse proxy's public Origin/Host so a proxied loopback serve 'just works'.** — Widens the trust boundary of the WS-origin/Host allowlist and risks DNS-rebinding regressions; the acceptance bar says WS origin policy stays unchanged. Forwarding a loopback Host/Origin at the proxy is the documented, safer contract.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** With read-auth mode ON on a loopback bind, an `/api` GET without a token returns 401, while the same GET with the token in the `x-kranz-token` header (and a WS upgrade with `?token=`) succeeds.
  `cargo test -p kranz-server read_auth_loopback`
- **[a2]** `/api/health` returns 200 without any token even when read-auth mode is ON.
  `cargo test -p kranz-server read_auth_health_exempt`
- **[a3]** A POST that presents the token only as a `?token=` query parameter (no `x-kranz-token` header) is still rejected with 401 — mutation authority never rides a URL.
  `cargo test -p kranz-server read_auth_post_rejects_query_only_token`
- **[a4]** With read-auth mode OFF on the default loopback bind, `/api` GETs and the WS upgrade remain tokenless (no localhost UX regression).
  `cargo test -p kranz-server read_auth_off_loopback_reads_tokenless`
- **[a5]** The Host-gate and WS-origin policy are unchanged: enabling read-auth on a loopback bind keeps the strict loopback origin/Host allowlist (it does not flip into LAN mode), and every existing origin/host/ws test still passes.
  `cargo test -p kranz-server`
- **[a6]** In the dashboard, a read (GET) that returns 401 opens the token prompt and, once a valid token is provided, retries and succeeds; the WebSocket URL carries the token as `?token=`.
  `npm --prefix apps/dashboard test`
- **[a7]** `docs/deploy.md` documents the shipped `--read-auth` mode where its "never expose without read-auth" warning previously referenced an unbuilt capability.
  `grep -q -- "--read-auth" docs/deploy.md`
- **[a8]** The workspace is formatted to the CI standard.
  `cargo fmt --all --check`
- **[a9]** The full workspace test suite passes.
  `cargo test --workspace`
- **[a10]** `kranz serve --read-auth` on a loopback bind actually serves with read-token enforcement (the flag is wired end-to-end, not just parsed): the printed guidance and behaviour reflect that reads now require the token. *(agent judgement)*

## Milestone 1 — Read-auth is a first-class, config-gated server mode

### 1.1 Decouple read-auth from bind class and thread a first-class read-auth flag through the router

In `crates/server/src/lib.rs`, read-side auth is currently a bind-class side effect: `serve_multi_on_listener` computes `require_read_token = [REDACTED] and `router_with_multi_repo_host_and_addr` then derives `let bind_is_loopback = !require_read_token;`. This CONFLATES two independent concepts — (1) whether reads/WS need a token, and (2) whether the bind is actually loopback (which drives the Host gate strictness in `require_host`/`host_allowed` and the WS-origin regime in `ws_origin_allowed`). They must be decoupled so read-auth can be forced ON while the bind stays loopback WITHOUT flipping the Host/WS-origin policy into LAN mode.

Do:
1. Add an independent `bind_is_loopback: bool` parameter to `router_with_multi_repo_host_and_addr` (and thread it, alongside the existing `require_read_token`, through the wrapper chain: `router_with_shared_host_and_addr`, `router_with_shared_host_and_bind`, `router_with_shared_host`, `router_with_host`, `router_with_token`, `router_with_static`, `router`). Remove the `let bind_is_loopback = !require_read_token;` derivation and use the passed-in value for `HostGate`, `ws_origin_allowed`, and `ServerState.bind_is_loopback`. `TokenGate.require_read_token` continues to gate reads. The back-compat/test wrappers that previously implied loopback should default `bind_is_loopback = true` and `require_read_token = false` to preserve their current behaviour.
2. In `serve_multi_on_listener`, compute both independently from the real bound address plus a new `read_auth: bool` input: `let bind_is_loopback = local_addr.ip().is_loopback();` and `let require_read_token = [REDACTED] || !bind_is_loopback;`. Thread a `read_auth: bool` parameter down from `serve_on_listener` / `serve_with_shared_host` / `serve_with_shutdown` (add the parameter; keep the existing off-loopback-forces-read-token behaviour intact when `read_auth == false`).
3. Migrate the existing test `ws_lan_mode_accepts_ip_origin_and_native_clients_with_token` in `crates/server/tests/server_test.rs`, which currently leans on the conflation by calling `router_with_shared_host_and_bind(..., require_read_token = true)` to simulate a LAN bind. Update its constructor call to pass `bind_is_loopback = false` explicitly so it still exercises the genuine LAN posture.
4. Keep the middleware behaviour (`require_mutation_token`) as-is: it already honours the `x-kranz-token` header, honours `?token=` for reads only, and exempts `/api/health`.

Add tests named with a `read_auth_` prefix (so `cargo test -p kranz-server read_auth_` selects them) driving the router via `tower::ServiceExt::oneshot` and/or a local listener (follow the patterns already in `server_test.rs`), covering:
- `read_auth_loopback_rejects_tokenless_get`: read-auth ON + loopback bind (`bind_is_loopback = true`, `require_read_token = true`) → GET `/api/missions/:id/state` without a token → 401; the same GET with the correct `x-kranz-token` header → 200; a WS upgrade with a valid `?token=` succeeds and without it is rejected.
- `read_auth_health_exempt_without_token`: read-auth ON → GET `/api/health` without a token → 200.
- `read_auth_post_rejects_query_only_token`: read-auth ON → a POST to a mutating route carrying the token ONLY as `?token=` (no header) → 401 (mutation authority stays header-only).
- `read_auth_off_loopback_reads_tokenless`: read-auth OFF + loopback (`bind_is_loopback = true`, `require_read_token = false`) → GET without a token → 200 and a WS upgrade with an allowed loopback Origin succeeds without a token (default localhost UX unchanged).
- `read_auth_loopback_keeps_strict_loopback_origin`: read-auth ON + loopback bind still applies the STRICT loopback WS-origin/Host allowlist (a `http://192.168.1.5:...` Origin or a missing Origin is rejected on the WS upgrade), proving the mode does not flip into LAN origin behaviour.

Run `cargo fmt --all` before finishing (CI enforces `cargo fmt --all --check`). Do NOT touch the CLI in this feature — the `--read-auth` flag and its wiring are a separate feature that will consume the new `read_auth` parameter you add to the serve entry points.

Done when:
- `router_with_multi_repo_host_and_addr` takes `bind_is_loopback` as an independent parameter and no longer derives it from `require_read_token`; all wrapper constructors compile and thread it.
- `serve_multi_on_listener`/`serve_on_listener` accept a `read_auth: bool` and compute `require_read_token = [REDACTED] || !bind_is_loopback` from the real bound address.
- `cargo test -p kranz-server read_auth_loopback_rejects_tokenless_get` passes (tokenless read 401, header-token read 200, WS `?token=` gated).
- `cargo test -p kranz-server read_auth_health_exempt_without_token` passes.
- `cargo test -p kranz-server read_auth_post_rejects_query_only_token` passes.
- `cargo test -p kranz-server read_auth_off_loopback_reads_tokenless` passes.
- `cargo test -p kranz-server read_auth_loopback_keeps_strict_loopback_origin` passes.
- `cargo test -p kranz-server` is green (existing origin/host/ws tests, including the migrated `ws_lan_mode_...`, still pass).

### 1.2 Add the `--read-auth` flag to `kranz serve` and wire it to the server

Expose read-auth mode on the CLI and wire it into the server entry point extended by the previous feature.

In `crates/cli/src/cli.rs`, add a `read_auth: bool` field to the `Serve` subcommand (`#[arg(long)]`, default false) with help text: forces the mutation token to be required on `/api` GETs and the WS upgrade (as well as POSTs) on ANY bind class, including loopback — the deployment-ready read-auth mode; off-loopback binds already require it. Off-loopback still requires `--insecure-lan` (unchanged); `--read-auth` is orthogonal and simply forces read-token enforcement on loopback too.

In `crates/cli/src/commands.rs`, thread the new flag from the `Serve` match arm at the call around line 230 into `cmd_serve`, then into the `serve_multi_with_token_cleanup` → `kranz_server::serve_multi_on_listener` chain as the new `read_auth` parameter added by the server feature. Update the operator-facing output in `cmd_serve`: when `--read-auth` is set, print a line making clear reads now require the token (e.g. that GETs and the WS upgrade require the mutation token too), so the operator understands the posture. Ensure `--open`'s `#token=<t>` fragment and the printed `mutation token: <t>` still work — the same token now authenticates reads.

Extract a small pure, unit-testable helper (mirroring the existing `refuse_non_loopback_without_insecure_lan`) that maps `(bind_is_loopback, read_auth_flag)` to the effective `require_read_token` boolean, and add `read_auth_`-prefixed CLI unit tests asserting: (a) `read_auth=true` on a loopback bind → require_read_token true; (b) `read_auth=false` on a loopback bind → require_read_token false; (c) a non-loopback bind → require_read_token true regardless of the flag. Do NOT loosen `refuse_non_loopback_without_insecure_lan`: a non-loopback bind still needs `--insecure-lan`.

Run `cargo fmt --all` before finishing. Verify `cargo build -p kranz-cli` succeeds and `cargo test -p kranz-cli read_auth_` passes.

Done when:
- `kranz serve --read-auth` parses (a `read_auth` field exists on the `Serve` subcommand) and is threaded into `serve_multi_on_listener` as the `read_auth` argument.
- A unit-testable helper maps `(bind_is_loopback, read_auth)` → `require_read_token`, and `cargo test -p kranz-cli read_auth_` covers loopback-on, loopback-off, and non-loopback cases.
- `refuse_non_loopback_without_insecure_lan` is unchanged: a non-loopback bind without `--insecure-lan` still errors.
- `cargo build -p kranz-cli` succeeds and the serve output signals read-token-on-reads when `--read-auth` is set.


## Milestone 2 — Dashboard/Tauri verified against the mode, and it is documented

### 2.1 Prove the dashboard authenticates reads and the WS against read-auth mode

The dashboard client already attaches the token to GETs and parks read 401s on the token gate, and `ws.ts` already appends `?token=`. This feature LOCKS that behaviour in with tests against the read-auth mode; make only minimal source changes if a test reveals a genuine gap.

In `apps/dashboard/src/lib/` add/extend vitest coverage (files run under `npm --prefix apps/dashboard test`, i.e. `vitest run`):
1. In `api.test.ts` (or a sibling): a `getJson`/`api.*` read call whose first `fetch` resolves 401 must call `awaitToken()` (open the token gate → `tokenGateSnapshot().needed === true`), and after `provideToken('<t>')` the request must retry with the `x-kranz-token` header set to that token and resolve with the body. Assert the retried request carried the header (inspect the mock `fetch` calls). This proves reads — not just mutations — drive `<TokenPrompt/>`.
2. In `ws.test.ts` (or `token`-aware ws coverage): constructing a `MissionSocket` when a token is resolvable produces a URL containing `token=<t>` in its query string, and produces no `token=` when none is resolvable. Reuse the existing token-resolution seams (`setToken`/`resolveToken`) rather than inventing new ones.
3. Confirm no read path leaks the token into a POST URL — POSTs send the header only. If any assertion exposes a real defect, fix it in the corresponding `lib/*.ts` source with the smallest change; otherwise leave the source untouched. A `TokenPrompt` copy tweak is permitted ONLY if the read-context 401 wording is actively misleading — the token is the same one, so churn is discouraged.

Do not change server code. Run `npm --prefix apps/dashboard test` and `npm --prefix apps/dashboard run lint` before finishing; both must pass.

Done when:
- A vitest test proves a GET/read that returns 401 opens the token gate (`tokenGateSnapshot().needed` becomes true) and, after `provideToken`, retries with the `x-kranz-token` header and resolves.
- A vitest test proves the `MissionSocket` URL includes `token=<t>` when a token is resolvable and omits it otherwise.
- `npm --prefix apps/dashboard test` passes and `npm --prefix apps/dashboard run lint` is clean.
- No read path places the token in a POST request URL (POSTs remain header-only).

### 2.2 Document the shipped read-auth mode in docs/deploy.md

Update `docs/deploy.md` so its security guidance references the SHIPPED read-auth mode instead of an unbuilt one.

In §5 "Security notes", the "Token on reads too" and "Never expose the raw server" bullets currently say read-auth is not built ("(today) no read-auth", "Until read-auth ships, do not expose…"). Rewrite them to: (a) state that `kranz serve --read-auth` is the shipped mode that requires the mutation token on `/api` GETs and the WS upgrade (via the `x-kranz-token` header, or `?token=` for the browser WS upgrade which cannot set headers), with `/api/health` exempted; (b) keep the standing rule that the raw server has no TLS and must sit behind a reverse proxy terminating TLS; and (c) for the persistent-host sketch in §4, note that a TLS proxy fronting a loopback `kranz serve` must forward a loopback `Host`/`Origin` (the server keeps the strict loopback origin/Host allowlist on a loopback bind by design — it does not trust arbitrary public origins), and add `--read-auth` to the `docker run … serve` example command. Update the §4 step 5 / §5 wording so it no longer implies read-auth is missing. Preserve the existing `> PREVIEW` banner and the never-push / scoped-push guidance unchanged.

Only `docs/deploy.md` (and, if it references read-auth as unbuilt, a one-line note in `docs/operator-gates.md` — but DO NOT check the `M6 live deploy` box; that is a human gate). Do not touch code. The literal string `--read-auth` must appear in `docs/deploy.md`.

Done when:
- `grep -q -- "--read-auth" docs/deploy.md` succeeds.
- The §5 "Token on reads too" / "Never expose the raw server" bullets no longer state read-auth is unbuilt and instead describe the `--read-auth` mode (reads + WS token, `/api/health` exempt).
- The persistent-host guidance notes the TLS proxy must forward a loopback Host/Origin, and the `docker run … serve` example includes `--read-auth`.
- The `M6 live deploy` operator-gate checkbox remains unchecked.
