# v0.2.1 server security patch

Baseline: public `05778ae` (v0.2.0). The supplied external review was a static
read without a Rust toolchain; its authorship and independence are not recorded
here. This response is implemented and locally reviewed by the coding agent.
No broader orchestration or containment audit is claimed.

| Finding | Change | Regression evidence |
|---|---|---|
| Missing Content-Length bypassed the JSON POST check | Use the body end-of-stream signal without consuming or buffering it. Unknown-length bodies require JSON; known-empty bodies still reach authentication. | `security_patch_unknown_length_body_requires_json` and `security_patch_chunked_http_post_requires_json`, including a real HTTP/1.1 chunked request. |
| Path suffixes implicitly granted hook authentication exceptions | Apply token middleware to protected routes before registering the two separately authenticated POST handlers. Catalog, unavailable-repo and API-miss routes remain gated. | `security_patch_hook_suffix_does_not_exempt_other_routes`, existing signed GitHub hook tests, and hook-status capability tests. |
| Mutation tokens could enter read/WS URLs | Query authentication accepts only read authority. Header-authenticated `GET /api/read-token` supplies the dashboard with a distinct read token; the client exchanges before reconnecting and cancels superseded work. | `security_patch_header_exchange_never_grants_mutation_authority`, real WebSocket upgrade tests, and `ws.readauth.test.ts` credential transport, failure, close, and replacement regressions. |

All four new Rust regressions failed against the baseline. Four of the five
new/updated client exchange regressions failed against the baseline; anonymous
loopback behavior already passed. The implementation adds no dependency and
changes no persisted event or state schema.

Compatibility: native clients keep header authentication. Custom browser
clients that put mutation authority in a WebSocket query must adopt a read
token or the exchange. The bundled dashboard includes the coordinated change.
Read tokens are still bearer credentials in WebSocket URLs; this patch reduces
their authority, not the need to protect them. The exchange is header-only
on both loopback and LAN, uses the existing Host/CORS policy, and is not cached.

The orchestrator extraction and repository-command consent proposal remain
separate design work. They are not included in this maintenance release.

## Local validation

Validated on macOS with Rust 1.97.1 and Node 22.23.1:

- Full `cargo test --workspace`: 2,937 passed, zero failed, 10 existing ignored,
  across 65 test summaries (including documentation tests).
- Workspace Clippy with warnings denied, formatting, locked build, and strict
  public Rust documentation: passed.
- Dashboard clean install, TypeScript, 238 tests, build, embedded sync/check,
  lint, and npm audit: passed. Lint retains warnings in untouched components.
- Standalone Tauri locked compilation and configured audit: passed; six
  existing allowed audit warnings remain. Dependency versions were unchanged.
- Experimental Even G2 client: 22 tests, build/package, lint and npm audit passed.
- Root Cargo audit/deny, regenerated Rust notices, four package license checks,
  two packaging tests, 11 operator-marker tests, and knowledge freshness passed.
- Package inventories inspected; the engine crate's packaging/build dry-run
  passed. Dependent crate registry dry-runs must follow engine publication.
- The CLI served a disposable repository with read authentication: header
  exchange, read-token access, mutation-query rejection, dashboard response,
  license output, and graceful token-file cleanup passed.
- Staged tree audit and reachable-history secret scan passed. The release-only
  private-vocabulary check still requires the owner's vocabulary; these ordinary
  checks do not substitute for it.

The first full run exposed a stopped Docker daemon and less than 4 GiB free
space, which correctly parked queue fixtures at the disk preflight. Starting
the existing Colima VM and cleaning this task's generated build artifacts
resolved those failures. The final suite used its Docker socket and disabled
incremental caching; no test or runtime safety check was weakened.

Review covered correctness (body/auth/reconnect behavior), readability and
architecture (route-local exceptions, unchanged public signatures), security
(header-only exchange, no redirect/fallback of credentials), and performance
(no body buffering or disk work; one small exchange per authenticated socket
connection). This is a self-review, not an independent audit. Remote platform
checks and publication are recorded by the release PR and tag workflow.


## First CI review

The trusted secret scanner flagged dummy fixture credentials and source
expressions, rather than live secrets. Fixtures now use explicit dummy names;
response construction separates the value from its serialized field. No
scanner rule or allowlist was changed. CodeQL modeled a client helper named
as authentication as a security check controlled by browser input. The helper
is now named for its actual operation, exchange-and-connect; server middleware
remains the authority. The real WebSocket tests prove tokenless gated reads
and mutation-query credentials are rejected independently of any client branch.
