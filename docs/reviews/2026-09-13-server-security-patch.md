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

- Full `cargo test --workspace`: 2,940 passed, zero failed, 10 existing ignored,
  across 66 test summaries (including documentation tests).
- Workspace Clippy with warnings denied, formatting, locked build, and strict
  public Rust documentation: passed.
- Dashboard clean install, TypeScript, 238 tests, build, embedded sync/check,
  lint, and npm audit: passed. Lint retains warnings in untouched components.
- Standalone Tauri locked compilation and configured audit: passed; six
  existing allowed audit warnings remain. Dependency versions were unchanged.
- Experimental Even G2 client: 22 tests, build/package, lint and npm audit passed.
- Root Cargo audit/deny, regenerated Rust notices, four package license checks,
  two packaging tests, 12 operator-marker tests, and knowledge freshness passed.
- Package inventories inspected; the engine crate's packaging/build dry-run
  passed. Dependent crate registry dry-runs must follow engine publication.
- The CLI served a disposable repository with read authentication: header
  exchange, read-token access, mutation-query rejection, dashboard response,
  license output, and graceful token-file cleanup passed.
- Staged tree audit and reachable-history secret scan passed. The initial
  release workflow also required an owner-supplied private vocabulary; the
  owner's subsequent decision below makes that additional check optional.

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

## Follow-up review

The supplied follow-up found that the CLI printed and stored pinned read
credentials before the router normalized empty or duplicated values. `serve`
now rejects invalid token transport and equal mutation/read credentials before
binding, starting workers, printing credentials, or writing token files. Errors
identify the flag/environment setting without displaying its value. Flag
precedence over environment variables remains unchanged.

`serve_rejects_invalid_read_credentials_before_binding_or_publishing` failed
before the fix and passes afterward for flag and environment input. The live
CLI test `serve_published_read_credential_matches_live_server` verifies that the
stored read token matches the exchange response and cannot authorize a POST.

The two independently authenticated hook handlers now live in a named router,
merged separately from the protected route list. The private unit-test build
registers synthetic suffix-collision POSTs in that protected list;
`registered_hook_suffix_posts_require_mutation_authority` exercises the actual
composition, including a successful authorized call to each handler. The token
middleware documents its API-only placement and nested-path assumption.

The distinct status for a development dashboard connected to a v0.2.0 server
remains deferred. Such a mixed-version deployment retries the failed exchange;
the embedded production dashboard is version-aligned with its server and no
mutation-credential URL fallback is introduced.

During follow-up validation, one parallel workspace run failed the unchanged
`git_config_protection_resolves_linked_worktree_config_and_rename_ancestors`
test with an authority-directory error. It passed in isolation and in the next
full parallel workspace run. An existing Kimi discovery test temporarily
changes process-global `HOME`, which is a possible concurrency cause, not a
confirmed diagnosis. No sandbox implementation or assertion was changed.

The completed Rust CodeQL scan resurfaced existing alerts #21, #24, #43 and
#52 in unchanged `hook_status.rs`. Inspection of SARIF analysis 1769314126
shows that every trace begins by treating Axum `State<Arc<ServerState>>` as
HTTP input, then follows `server.repo_root` to filesystem calls. That root is
constructed from the operator's repository context in `repo_context_router`,
not supplied by the request. Mission and run IDs from the JSON body separately
pass `MissionPaths::is_safe_id` before any path join. The existing endpoint and
engine traversal/capability regressions pass. These four traces are false
positives; the same alerts exist on baseline `05778ae`. Their individual GitHub
triage records preserve this reasoning; no scanner rule or workflow is weakened.

## Release workflow and Windows follow-up

The owner explicitly selected the existing secret and domain-policy checks,
with no additional confidentiality word list. Release verification now skips
only the unconfigured extra vocabulary scan and says so in its output. A
configured vocabulary still must be valid and pass both tree/history checks.
Gitleaks remains mandatory, and release verification now also runs the committed
domain-policy check directly after building the CLI. The private domain seed
was not copied into GitHub or repurposed as a blanket confidentiality ban: its
matches in the two governance documents have existing, path-specific waivers.
The updated audit fixtures prove that optional mode still rejects configured
matches and that required mode still rejects missing or empty input.

Windows CI on `0a60100` passed the new credential tests and production
containment receipts, then failed the unchanged
`pause_resume_and_user_message_flow`: after a fixed 900 ms sleep the snapshot
was still `Approved`, rather than `Paused`. The test now waits, within the
existing timeout, for the observed paused state and then for the pending user
message before enqueuing Resume. Completion, event order, paused-time reporting,
and inbox cleanup assertions remain. No engine runtime behavior changed.

macOS CI on `274bfd0` reproduced the earlier Git-protection failure. The Kimi
fallback-discovery fixture now re-executes only itself in a child process, so
its temporary `HOME` and `PATH` cannot affect the parallel unit suite. The
parent verifies both successful exit and one executed test. The linked-worktree
test also holds the shared environment-test lock while reading authority masks,
which excludes the other fixtures using that lock to change `HOME`. A mutex
only around the Kimi writer would not protect unrelated readers. All discovery
and Git-protection assertions remain; no sandbox runtime behavior was changed.
The failing CI log does not identify the exact concurrent writer, so the earlier
Kimi attribution remains a hypothesis rather than a confirmed diagnosis.

## Release verification and v0.2.2

Linux PR #53 CI on `34ebb24` failed the synchronized Git-config race fixture:
bubblewrap could no longer find a temporary Cargo registry directory captured
while another unit fixture changed `CARGO_HOME`. This live sandbox test now
holds the existing environment-test lock from profile construction through
child completion, excluding the guarded Cargo-home writers and their teardown.
The exact concurrent writer is not identified by the log. All three repository
layouts, prohibited-write attempts, normal commit, and synchronization
assertions remain; runtime containment and its fail-closed behavior are unchanged.

PR #52 and merged commit `0542e80` passed every CI and CodeQL check. The
annotated v0.2.1 tag points to that commit. Release run 34788843394 then failed
`sandbox_preflight_emits_no_macos_profile_issue_on_linux`: the release runner
did not have `bwrap`, and plan approval correctly refused to run enforced
engine gates without containment. Regular CI installs and probes bubblewrap;
the release workflow had omitted that host preparation. No binary archive,
GitHub release, or v0.2.1 crate was published.

The corrected release is v0.2.2, preserving the public v0.2.1 tag. Its release
runner uses the same bubblewrap/user-namespace setup, mold linker, and nested
build disk preparation as Linux CI, along with its fixture Git identity and
required-capability policy. Recorded skips remain visible in the job log.
The failing test and runtime refusal stay
intact. Product behavior is the reviewed server patch above; this follow-up
changes release infrastructure and aligned version metadata only. The owner's
decision to use the existing secret and domain checks also applies to v0.2.2.
The workflow now also accepts a manual rehearsal: it verifies the candidate
version against current `main` and builds all artifacts, while publication
remains limited to tag-push runs. This lets the actual release pipeline be
checked before another public tag is created.

Local v0.2.2 validation passed: 2,940 workspace tests (10 existing ignored),
Clippy with warnings denied, formatting, locked build, strict Rustdoc, Cargo
audit/deny, knowledge freshness, domain lint (865 files), package license checks,
and the live CLI smoke test. The clean dashboard gates passed with 238 tests
and the same embedded bundle; the standalone Tauri locked check also passed.
Actionlint validates the workflow. The actual Linux release rehearsal is run
after this change merges, before the v0.2.2 tag or publication.
