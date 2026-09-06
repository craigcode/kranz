# Adversarial security review — `crates/server` (kranz @ 33732c27)

Scope: `crates/server/src/{error,hooks,host,lib,multi,rest,tickets,ws}.rs` + `crates/server/tests/`,
`crates/cli/src/host_bridge.rs` and the `serve` paths in `crates/cli/src/{cli,commands}.rs`,
`crates/engine/src/auth_verify.rs` (does not exist on this commit; the equivalent verification lives
in `kranz_engine::hooks::verify_signature` and `kranz_engine::hook_status::record_signal`, both read).

## Summary (5 lines)

1. The auth plane is unusually well built: constant-time compare, per-serve uuid tokens stored 0600 and removed on shutdown, a read-only token that is structurally refused on mutations, an Origin allowlist pinned to the *actual* bound IP and port, and a Host gate that closes DNS rebinding. I traced all 40 routes from registration to side effect and found no route that mutates state without either the mutation token, a per-repo HMAC, or a per-run capability token.
2. The one real bug is a **symlink-follow asymmetry**: `mission_paths()` proves no path *component* is a symlink, but the leaf-file reads (`plan.json`, `plan.md`, `report.md`, `runs/<id>.jsonl`) go through plain `std::fs::read_to_string`, while the engine's own log reads use `paths::open_read_nofollow`. Where a session can write into the mission dir (`worker.isolation: "checkout"`), that turns a tokenless loopback GET into an arbitrary-file read — including `.kranz/serve.token`, which the sandbox explicitly read-denies.
3. `http://tauri.localhost` is approved for CORS and the WS upgrade unconditionally — no bind-IP or port pinning — which is exactly the co-resident-process scenario `origin_allowed`'s own doc comment defends against for every other origin.
4. The Tauri shell hands the webview the **mutation** token and arms `read_auth`, so the mutation token ends up in the WS URL query — contradicting the invariant `require_mutation_token`'s doc states, and leaving the read-only token the CLI mints unused by any shipped consumer.
5. Almost every handler does blocking fs / git / process work directly on the tokio runtime (only `/api/repos` and `merge` use `spawn_blocking`), and `run_transcript` reads a whole transcript into memory unbounded.

---

## MEDIUM — Leaf-file reads follow symlinks, defeating the sandbox's `serve.token` read-deny

**Severity:** MEDIUM (HIGH impact, non-default preconditions)
**Confidence:** CONFIRMED for the server-side symlink follow and the missing write-deny; PLAUSIBLE for the end-to-end chain (I read the sandbox allow/deny sets but did not run a sandboxed session).

**Refs**
- `crates/server/src/rest.rs:1084-1097` — `mission_paths()` calls `paths.require_no_follow()`.
- `crates/engine/src/paths.rs:247-258` — `open_mission_dir_nofollow` walks only `[".kranz", "missions", <id>]`. The leaf file is never checked.
- `crates/server/src/rest.rs:1255-1267` — `read_file_or_404` → `std::fs::read_to_string(path)`, which follows symlinks.
- Callers: `mission_plan` `rest.rs:608`, `mission_plan_md` `rest.rs:623`, `mission_report_md` `rest.rs:689`, `run_transcript` `rest.rs:852`.
- Contrast: `crates/engine/src/event_log.rs:484,588` — `EventLog::read_events` / `read_events_after` use `crate::paths::open_read_nofollow`, and `crates/engine/src/paths.rs:1331` has a regression test `read_events_refuses_a_symlinked_log`.
- `crates/engine/src/sandbox.rs:793-799` — `MISSION_METADATA_FILES` = `events.jsonl`, `events.jsonl.lock`, `state.json`, `state.json.tmp`, `estimate.json`. **`plan.json`, `plan.md`, `report.md` are not write-denied.**
- `crates/engine/src/sandbox.rs:760-766` — `write_allowlist` = `session_cwd` + `tmpdir` + `extra_write`; for `WorkerIsolation::Checkout` the session cwd is the repo root (`crates/server/src/rest.rs:378-381`, `crates/engine/src/orchestrator.rs:1367-1370`).
- `crates/engine/src/sandbox.rs:856-861,876` — `authority_read_deny_paths` denies `.kranz/serve.token`, with the comment "a read of `serve.token` IS mutation authority over `kranz serve` (**loopback is reachable from every sandbox tier**)".

**Attacker + preconditions.** A worker agent session (the untrusted party kranz is built to govern), running under `worker.isolation: "checkout"` with `worker.sandbox.enforce` set to `fs` or `fs+net`, against a repo served by a default `kranz serve` (loopback bind, GETs tokenless).

**Impact.** The session replaces `<repo>/.kranz/missions/<id>/plan.md` with a symlink to `<repo>/.kranz/serve.token` (permitted: the mission dir is under the writable repo root and `plan.md` is not in the deny set), then issues `GET http://127.0.0.1:<port>/api/missions/<id>/plan.md` over loopback. The server process is unsandboxed, so it follows the link and returns `{"markdown":"<mutation token>"}`. The agent now holds operator mutation authority: approve its own pending plan, approve its own parked command grants, `POST /missions/:id/merge`, create and start new missions. This is a complete bypass of the consent surface, and it defeats a control the codebase deliberately built (`authority_read_deny_paths`). The same primitive reads any file the serve process can read (`~/.ssh/*`, `.kranz/config.json` with its Slack tokens).

**Evidence**
```rust
// rest.rs:1093 — the check that exists
if paths.require_no_follow().is_err() { return Err(unknown_mission(id)); }
// rest.rs:1259 — the read that ignores it
match std::fs::read_to_string(path) { Ok(content) => Ok(content), ... }
```

**Suggested fix.** Route every server-side repo file read through `kranz_engine::paths::open_read_nofollow` (make it `pub`), the same helper `EventLog` already uses — `read_file_or_404`, `read_missions_index` (`rest.rs:169`) and `Ticket::load` in `tickets.rs:43,99`. Independently, add `plan.json`, `plan.md`, `report.md` to `MISSION_METADATA_FILES` so checkout-mode sessions cannot rewrite the plan the operator reviews either.

**Existing test coverage.** None for the leaf file. `rest.rs:2114` (`list_missions_surfaces_a_symlinked_mission_dir_as_an_error_row`) and `rest.rs:2165` (`mission_state_refuses_a_symlinked_mission_dir`) cover the symlinked *directory* only; `grep -rn symlink crates/server/tests/` returns nothing.

---

## LOW — `http://tauri.localhost` is CORS/WS-approved on every bind, unpinned to IP or port

**Severity:** LOW (MEDIUM if the operator runs Windows)
**Confidence:** CONFIRMED by code reading; the exploitability step (holding loopback:80 and having `tauri.localhost` resolve) is PLAUSIBLE and platform-dependent — I did not test a browser.

**Refs**
- `crates/server/src/lib.rs:588-593` — `origin_allowed` returns `true` before any bind check for the two Tauri origins.
- `crates/server/src/lib.rs:662-673` — `ws_origin_allowed` delegates to `origin_allowed`, so the WS live feed inherits it.
- `crates/server/src/lib.rs:594-628` — every *other* localhost origin is pinned to the exact bound IP and port, precisely because "on Linux an unprivileged co-resident process can bind ANOTHER loopback address (127.0.0.2) on kranz's own port and serve a hostile page".

**Attacker + preconditions.** A local process that can serve HTTP at the origin `http://tauri.localhost` (i.e. port 80 — the origin string must match byte-for-byte, so `http://tauri.localhost:1420` is correctly rejected) plus a browser that maps `*.localhost` to loopback (Chrome and Firefox do). Binding port 80 needs root on Unix but not on Windows, where Tauri's `http://tauri.localhost` origin is the reason this entry exists. Alternatively, an attacker who controls DNS for a real `tauri.localhost` name. The operator then has to load that page.

**Impact.** On a default loopback serve, GETs are tokenless, so that page can read every mission state, event log, transcript, plan, report, ticket, and the queue cross-origin, and open the WS live feed. It cannot mutate: POSTs still require the mutation token, and CORS does not leak it. So this is confidentiality (agent transcripts and repo internals), not consent bypass.

**Evidence**
```rust
pub(crate) fn origin_allowed(origin: &str, bind_addr: Option<SocketAddr>) -> bool {
    if origin == "tauri://localhost" || origin == "http://tauri.localhost" { return true; }
```
(`tauri://localhost` is fine — no web page can hold a custom-scheme origin. `http://tauri.localhost` is an ordinary http origin.)

**Suggested fix.** Approve `http://tauri.localhost` only when the process was actually launched as the Tauri shell (thread a flag through `router_with_*`), or drop it on the CLI `serve` path entirely. It is only needed for the Windows webview, which never goes through `kranz serve`.

**Existing test coverage.** `crates/server/tests/server_test.rs:1350` (`cors_allows_localhost_and_tauri_origins`) asserts this origin *is* approved — i.e. the behaviour is pinned as intended, not overlooked.

---

## LOW — Tauri shell puts the mutation token in the WS URL and never uses the read-only token

**Severity:** LOW
**Confidence:** CONFIRMED (traced from the Tauri setup through `token.ts` to `ws.ts`).

**Refs**
- `apps/dashboard/src-tauri/src/lib.rs:82-91` — `serve_multi_on_listener(..., authority = Some(token), read_authority = None, read_auth = true, ...)`.
- `apps/dashboard/src-tauri/src/lib.rs:141-146` — `window.__KRANZ_TOKEN__ = <mutation token>` injected into the webview.
- `apps/dashboard/src/lib/token.ts:70-86` — `resolveToken()` returns that same token.
- `apps/dashboard/src/lib/ws.ts:14-15` and its `?token=` append — the WS upgrade carries `resolveToken()` in the query.
- `crates/server/src/lib.rs:829-834` — the stated invariant: "POSTs are header-only, so mutation authority never rides in a URL that can land in shell history or an intermediary's access log."
- `crates/cli/src/commands.rs:2395-2401,2424-2426` — `kranz serve` mints and prints a read token, but `--open` handed the browser the mutation token in the URL fragment; nothing ever delivers the read token to a dashboard.

**Attacker + preconditions.** No remote attacker. This is a defence-in-depth regression: the mutation token appears in a URL (webview navigation state, any future request logging, crash reports) in exactly the case the design tried to avoid, and the "read token safe for dashboards and agents" (`commands.rs:2401`) has no consumer.

**Impact.** Widens where mutation authority can be observed. The server-side mitigation holds — `require_mutation_token` honours `?token=` only on reads (`lib.rs:886-901`), verified by `read_auth_post_rejects_query_only_token` — so a leaked query token cannot itself mutate; but it *is* the mutation token, and anything that recovers it from a URL gains full authority.

**Suggested fix.** Have the Tauri shell mint two tokens and inject the read-only one for reads/WS (`window.__KRANZ_READ_TOKEN__`), keeping the mutation token header-only; and have `kranz serve --open` pass the read token in the fragment, prompting for the mutation token on first mutation (the `TokenPrompt` gate already exists).

**Existing test coverage.** `crates/server/tests/server_test.rs:2026` covers the server-side refusal. Nothing asserts which token the shipped dashboard actually carries.

---

## LOW — Blocking and unbounded work on the async runtime, reachable tokenless on a loopback serve

**Severity:** LOW
**Confidence:** CONFIRMED by reading each handler; `grep -n spawn_blocking crates/server/src/*.rs` returns only two production uses.

**Refs**
- `crates/server/src/rest.rs:40-89` — `list_missions` opens the repo, folds every mission's full event log, and runs a git ancestry probe per row, all inline.
- `crates/server/src/rest.rs:703-729` — `mission_diff_stat` runs `git rev-parse` + `git diff --stat` inline.
- `crates/server/src/rest.rs:769-781` — `mission_readiness` spawns a backend probe process inline.
- `crates/server/src/rest.rs:852-870` — `run_transcript` does `read_to_string` of the whole run transcript, then builds a `Vec<Value>` of every line — no size cap. The transcript's size is controlled by the agent.
- `crates/server/src/ws.rs:96-105,116-146` — the WS session folds and re-folds the full log on the runtime thread.
- Only `crates/server/src/lib.rs:241` (`/api/repos`) and `crates/server/src/host.rs:786` (`merge`) use `spawn_blocking`, so the correct pattern is known and applied inconsistently.

**Attacker + preconditions.** Any local process on a default loopback serve — including a sandboxed agent session, since the codebase itself states loopback is reachable from every sandbox tier (`crates/engine/src/sandbox.rs:858`). Off-loopback the read token gates these, so the reachable attacker is local.

**Impact.** Concurrent `GET /api/missions/<id>/runs/<r>/transcript` requests against a large transcript drive memory to several times the file size each; a handful of `list_missions` / `readiness` requests stall tokio worker threads and the WS live feed the operator is watching. This degrades the consent surface's liveness rather than bypassing it.

**Suggested fix.** Wrap the fs/git/process handlers in `spawn_blocking` (matching `/api/repos`), and stream or cap `run_transcript` (a `?tail=` bound, or a byte cap mirroring `EventLog::read_tail_events`).

**Existing test coverage.** None; no load or size-limit tests in `crates/server/tests/`.

---

## INFO — Token-gate exemptions match by path suffix and one is not method-scoped

**Severity:** INFO (no current bypass)
**Confidence:** CONFIRMED

**Refs** `crates/server/src/lib.rs:856-871`
```rust
let is_github_hook = path.ends_with("/hooks/github");
let is_hook_signal_post = request.method() == Method::POST && path.ends_with("/hook-status");
```
`is_hook_signal_post` is correctly method-scoped (so the read-gated `GET /missions/:id/hook-status` still needs a token); `is_github_hook` is not, so `GET /api/hooks/github` is token-exempt. Today that is harmless — the route is POST-only and the `/api/{*path}` catch-all is `api_not_found` — but both exemptions are suffix matches, so any future route whose path ends in `/hooks/github` or `/hook-status` silently becomes unauthenticated. I enumerated `repo_api_routes` (`lib.rs:412-507`) and the multi-repo nest prefixes; no other registered path matches either suffix.

**Suggested fix.** Match the two exemptions against the exact route set (`path == "/api/hooks/github" || path.strip_prefix("/api/repos/").is_some_and(...)`), and method-scope the webhook exemption to POST.

**Existing test coverage.** `crates/server/tests/hooks_test.rs:525` (`ghook_token_gate_still_arms_every_other_post_route`) and `crates/server/src/rest.rs:1600` (`hook_status_signal_post_is_exempt_from_the_mutation_token_gate`) pin the intended behaviour but not the suffix hazard.

---

## INFO — hook-status endpoint is an unauthenticated run-id oracle, and its `detail` is operator-visible worker text

**Severity:** INFO
**Confidence:** CONFIRMED

**Refs**
- `crates/server/src/rest.rs:812-820` — `UnknownRun` → 404 `unknown hook-status run '<id>'`; `TokenMismatch` → 401. An unauthenticated caller can therefore distinguish "this (mission, run) exists" from "wrong token".
- `crates/engine/src/hook_status.rs:536-542` — the recorded `detail` comes from the hook payload, `scrub_and_truncate`d to 200 chars, and is rendered beside the pending-decision chrome and in Slack `/kranz status`.

**Impact.** The oracle leaks run existence to any local process (and, on `--insecure-lan`, to the LAN — the route is exempt from the token gate on every bind). The `detail` string is a UI-spoofing surface: a hostile session holding its own run token can write reassuring text next to the approve buttons. Both are acknowledged in the module's own design notes, the payload is bounded (16 KiB route limit, `lib.rs:456`) and scrubbed, and the lane is labelled `authoritative: false` and cannot touch the fold — so this is a note, not a finding I'd act on.

**Suggested fix.** Collapse `UnknownRun` and `TokenMismatch` into one 401 with a generic message. Leave the `detail` rendering alone, but keep the non-authoritative label prominent.

**Existing test coverage.** `crates/server/src/rest.rs:1600` and the surrounding `#[cfg(test)]` block cover the auth and write-hygiene properties, not the oracle.

---

## INFO — Error bodies and `/api/repos` disclose absolute filesystem paths

**Severity:** INFO
**Confidence:** CONFIRMED

`crates/server/src/error.rs:80-97` forwards `EngineError::to_string()` verbatim, and those messages routinely name absolute paths; `crates/server/src/rest.rs:1262-1265` explicitly formats `path.display()` into a 500 body; `crates/server/src/multi.rs:130-154` returns each repo's absolute `root`. Tokenless on a loopback serve, read-token-gated otherwise. Consistent with the local-tool posture; noted only because it makes the symlink finding above easier to aim.

---

## INFO — The Host gate is skipped when no `Host` header is present

**Severity:** INFO
**Confidence:** PLAUSIBLE (I did not verify hyper's h2c behaviour on this axum version).

`crates/server/src/lib.rs:691-705` allows a request with no `Host` header, documented as the in-process `oneshot` path. HTTP/2 carries the authority in `:authority`, not `Host`, and `axum::serve` uses hyper-util's auto (HTTP/1 + HTTP/2) builder, so an h2c prior-knowledge client would skip the gate. This is not a browser-reachable bypass — browsers refuse plaintext HTTP/2 — and the Host gate is only defence-in-depth against DNS rebinding, which is a browser-only attack. To confirm or dismiss: send an h2c prior-knowledge request to a running `kranz serve` and check whether `require_host` sees a `Host` header. Fix if desired: fall back to `request.uri().authority()` when the header is absent.

---

## Areas checked with no finding

- **Token generation, storage, lifetime.** `generate_token` is uuid v4 simple hex (`lib.rs:53`). `write_token_file` (`commands.rs:2657-2700`) creates with `O_EXCL` + mode 0600, re-chmods, and renames atomically over the destination (so an existing inode or symlink is replaced, not written through); `serve_multi_with_token_cleanup` (`commands.rs:2510-2549`) removes both files on the Ok and Err paths. Operator-catalog tokens go to `~/.kranz/serve/<endpoint>.token` keyed by the full socket address.
- **Constant-time comparison.** `token_matches` (`lib.rs:919-922`) uses `subtle::ConstantTimeEq`; `kranz_engine::hooks::verify_signature` (`hooks.rs:158-165`) and `hook_status::record_signal` (`hook_status.rs:524-528`) do the same for the webhook HMAC and the per-run capability token.
- **Route-by-route authorization.** I enumerated all 40 routes in `repo_api_routes` (`lib.rs:412-507`) plus `/api/health` and `/api/repos`, and traced each to its handler. Every state-mutating route is a POST under `/api/` and therefore covered by `require_mutation_token`; the two deliberate exemptions carry their own authentication, which I traced end to end (HMAC + repo-identity match in `hooks.rs:44-102`; per-run token hash + safe-id + 24 h TTL in `hook_status.rs:503-549`). `release` and `delete` are POSTs specifically so the gate applies (`host.rs:2177,2198`).
- **Read token never accepted on a mutation.** `read_ok` is gated on `is_read` before the comparison (`lib.rs:874-880`); `?token=` is only consulted for reads and only when the read gate is armed (`lib.rs:886-901`). Covered by `read_auth_post_rejects_query_only_token`.
- **Id / slug / repo-id normalization.** `MissionPaths::is_safe_id` (`paths.rs:69-71`) rejects `/`, `\`, `:` and `..`; `Ticket::ensure_valid_slug` (`ticket.rs:286`) restricts to alphanumerics, `-`, `_` with no leading dot; `validate_repo_id` (`multi.rs:544-555`) is equally strict and runs at config load, and repo routes are registered statically at startup — no request-time path construction from a repo id. `run_transcript` re-checks `safe_id(run_id)` (`rest.rs:857`). `plan_md_and_report_md_reject_traversal_ids` (`server_test.rs:924`) covers the URL side.
- **DNS rebinding.** `host_allowed` (`lib.rs:720-729`) accepts only `localhost[:port]` and loopback IP literals on a loopback bind, and only IP literals off loopback; a rebinding page presents the attacker hostname and is refused. Tested at `server_test.rs:1432`.
- **CSRF / drive-by POST.** `require_json_api_posts` (`lib.rs:786-808`) forces any non-empty POST body into the preflighted path, and the empty-body exemption still lands on the token gate. I checked the simple-request content types (`text/plain`, form-urlencoded, multipart) — all rejected — and confirmed the empty-body path reaches only the two self-authenticating routes.
- **Grant / revision / question TOCTOU.** The REST pre-checks fold the log and match the exact parked decision (`rest.rs:1136-1215`), and the engine re-validates at drain time (`orchestrator.rs:2900-2936`, `approve_pending_grant(&command)`), so a stale or swapped approval is dropped rather than applied to a different request.
- **WebSocket.** Origin is checked before the upgrade (`ws.rs:47-52`), `?since=` is parsed strictly rather than silently ignored, replay is bounded to 5000 events, a corrupt or vanished log closes the socket cleanly instead of panicking, and inbound frames are ignored except `{"type":"ping"}`. Missing-Origin upgrades are refused on loopback binds (`lib.rs:668`), tested at `server_test.rs:1845` and `2065`.
- **Static file serving.** The directory case uses tower-http `ServeDir`/`ServeFile` (traversal-safe); the embedded case (`lib.rs:509-530`) indexes a fixed `&'static [EmbeddedFile]` table by exact path with an `index.html` fallback and touches no filesystem. Neither is authenticated, by design — they serve the dashboard shell only, and `cache_response_headers` (`lib.rs:351-372`) keeps `/api` responses `no-store` and HTML `no-cache`.
- **Multi-repo plane.** The catalog is static for the process lifetime, ids and roots are validated and de-duplicated at load, unavailable repos claim their paths with an explicit 503 handler so API misses never fall through to the SPA (`lib.rs:251-302`), and the unscoped alias is mounted only for an explicit default or a single healthy repo.
- **Body size limits.** `/hook-status` is capped at 16 KiB at the route (`lib.rs:456`); every other `Bytes`/`Json` extractor inherits axum's 2 MiB `DefaultBodyLimit`. No unbounded request-body read.
- **Slack bridge glue (`crates/cli/src/host_bridge.rs`).** A pure adapter over the same `MissionHost`; it forwards `force: false` on ticket approval and `then_enqueue: false` on draft, so the Slack path cannot take a weaker consent route than REST. It leaks no status codes or paths beyond `ApiError::message`.
