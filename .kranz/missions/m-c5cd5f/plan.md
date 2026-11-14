# Mission plan — m-c5cd5f

**Goal:** Confirm and close two reported gaps in the Sentry signal intake path: Sentry replay protection must rely only on values the signature covers and remember an accepted delivery for at least 24 hours on the server clock, and the endpoint and credential-source fields of signals.sentry must be settable from the global config layer only.

Branch `kranz/mission-m-c5cd5f` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Write paths** (6; glob patterns the mission may change):
- `crates/engine/src/signals/**`
- `crates/engine/src/config.rs`
- `crates/engine/tests/**`
- `crates/server/src/hooks.rs`
- `crates/server/tests/**`
- `docs/scoping/signal-intake.md`

## Cost estimate

Estimated **$7.76 – $38.81** (expected ~$17.87). Rough estimate — live usage is authoritative; based on 49 completed mission(s).

Context-fit check (corpus p90 over 53 plan(s): 4185 chars / 22 files):
- **f-1-1** (Body-derived replay key and 24-hour server-clock replay memory) — spec is 6020 chars (corpus p90: 4185)
- **f-2-1** (Operator-only trust rules for signals.sentry endpoint and credential fields) — spec is 4350 chars (corpus p90: 4185)
- **f-2-2** (Sentry route config loader refuses project-layer endpoint fields and merges sentry field by field) — spec is 4812 chars (corpus p90: 4185)

## Considered alternatives

**Chosen approach:** Two milestones, one per reported item, each confirmed against code first. Replay: rekey the Sentry cache on the SHA-256 of the signed body and give the shared ReplayCache a 24-hour server-clock retention, separate from the 300-second freshness check. Config: close both doors, the general loader's CONFIG_TRUST_RULES (covered by the requested test beside the other refusals) and the Sentry route's own SignalsConfig::load, which bypasses those rules, using the existing load_hooks refusal pattern and a field-by-field sentry merge.

Rejected shapes:
- **Only add CONFIG_TRUST_RULES entries for signals.sentry fields** — SignalsConfig::load reads raw JSON independently, so the Sentry route would still let a repository replace endpoint and credential fields
- **Silently ignore project-layer endpoint fields instead of refusing** — Inconsistent with every other operator-only key (load_hooks, config::load refuse with a named error), and hides a repository's attempt to redirect write-back
- **Bind the timestamp into the replay key or require a signed timestamp** — Sentry's signature covers only the body, so any timestamp-derived key is attacker-controlled; requiring a signed timestamp would reject every real Sentry delivery
- **Persist the replay cache to disk so the 24-hour window survives restarts** — Beyond the operator decision, which accepts and asks us to document that a restart or a full cache can shorten the window

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A Sentry delivery whose body was already accepted is refused as a replay (DropReason::ReplayedDelivery) when re-sent with a fresh Sentry-Hook-Timestamp, a different Request-ID or X-Sentry-Delivery header, or a differently spelled but valid signature (uppercase hex or a sha256= prefix). It stays refused at 301 seconds, 1 hour and 86,399 seconds after first receipt, measured on the server clock. Two different signed bodies that carry the same Request-ID are both accepted. A timestamp more than 300 seconds from the server clock is still refused as TimestampSkew.
  `out=$(cargo test --workspace signed_replay_ 2>&1); rc=$?; printf '%s\n' "$out" | tail -n 60; [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9]'`
- **[a2]** Over HTTP, POST /api/hooks/sentry with a byte-identical, correctly signed body that was already accepted, re-sent with a new Request-ID and a fresh Sentry-Hook-Timestamp, returns 409 Conflict and does not grow .kranz/signals/ledger.jsonl.
  `out=$(cargo test --workspace sentry_http_replay_ 2>&1); rc=$?; printf '%s\n' "$out" | tail -n 60; [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9]'`
- **[a3]** The general config loader refuses a project config layer that sets signals.sentry.apiBaseUrl, signals.sentry.tokenEnv or signals.sentry.allowedHosts, with an error that names the key. The same content is accepted from the global layer, and a project layer that sets only signals.sentry.secret is still accepted. The test lives in crates/engine/tests/config_trust_test.rs beside the other project-layer refusals.
  `out=$(cargo test -p kranz-engine --test config_trust_test sentry_global_only_ 2>&1); rc=$?; printf '%s\n' "$out" | tail -n 40; [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9]'`
- **[a4]** SignalsConfig::load, the loader the Sentry webhook route uses, returns a config error naming the file and the key when the project layer sets signals.sentry.apiBaseUrl, tokenEnv or allowedHosts, or sets signals.sentry to a non-object value. A project layer that sets only signals.sentry.secret is merged field by field: the global layer's apiBaseUrl, tokenEnv and allowedHosts survive and the project secret is used. A Sentry webhook POST to a repository whose project config sets a global-only field is refused with a non-2xx status and creates no .kranz/signals tree.
  `out=$(cargo test --workspace signals_sentry_layer_ 2>&1); rc=$?; printf '%s\n' "$out" | tail -n 60; [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9]'`
- **[a5]** Contract files change additively only: no line is removed from crates/engine/src/events.rs or crates/engine/src/types.rs relative to the mission base.
  `d=$(git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs) && ! printf '%s\n' "$d" | grep -qE '^-[^-]'`
- **[a6]** The whole Rust workspace test suite passes.
  `cargo test --workspace`
- **[a7]** The workspace is clippy-clean with warnings denied.
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a8]** The workspace is rustfmt-clean.
  `cargo fmt --all --check`
- **[a9]** The workspace builds.
  `cargo build --workspace`
- **[a10]** docs/scoping/signal-intake.md and the rustdoc on ReplayCache and the Sentry adapter accurately state all of the following: the Sentry replay key is derived only from the signed raw body; an accepted delivery is remembered for at least 24 hours from first receipt on the server clock, bounded by the existing 4,096-entry cache capacity; the 300-second timestamp window is a freshness check only; and a process restart or a full cache can shorten the replay window. No stale description of a (source, delivery_id_or_sig_hash, timestamp) key with skew-window retention remains in those places. *(agent judgement)*
- **[a11]** Each fix carries a test that would fail against the pre-mission implementation at $KRANZ_BASE_SHA: the signed_replay_ and sentry_http_replay_ tests fail there because unsigned headers or skew-window eviction let a replay through, and the sentry_global_only_ and signals_sentry_layer_ tests fail there because the project layer could set or replace endpoint and credential fields. Existing tests changed by the mission were adjusted only where the new behaviour intentionally differs (a byte-identical body is now a replay), not weakened. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a5] d=$(git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs) && ! printf '%s\n' "$d" | grep -qE '^-[^-]'
base-expected-to-fail (benign): [a1] out=$(cargo test --workspace signed_replay_ 2>&1); rc=$?; printf '%s\n' "$out" | tail -n 60; [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9]'; [a2] out=$(cargo test --workspace sentry_http_replay_ 2>&1); rc=$?; printf '%s\n' "$out" | tail -n 60; [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9]'; [a3] out=$(cargo test -p kranz-engine --test config_trust_test sentry_global_only_ 2>&1); rc=$?; printf '%s\n' "$out" | tail -n 40; [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9]'; [a4] out=$(cargo test --workspace signals_sentry_layer_ 2>&1); rc=$?; printf '%s\n' "$out" | tail -n 60; [ "$rc" -eq 0 ] && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9]'; [a6] cargo test --workspace
not linted (never run on the base): [a7] cargo clippy --workspace --all-targets -- -D warnings; [a8] cargo fmt --all --check; [a9] cargo build --workspace

named contract gates (defect classes):
- vacuous-filter: PASS
- wrong-polarity: PASS
- passes-on-base: FAIL
  [a5] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `d=$(git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs) && ! printf '%s\n' "$d" | grep -qE '^-[^-]'`
- env-sensitive: PASS


## Milestone 1 — Sentry replay protection relies only on signed values and remembers accepted deliveries for 24 hours

### 1.1 Body-derived replay key and 24-hour server-clock replay memory

Context: kranz is a Rust workspace (crates/engine = kranz-engine, crates/server = kranz-server). A Sentry webhook adapter lives in crates/engine/src/signals/sentry.rs. Its SentrySignalSource::verify (about lines 105-150) verifies Sentry-Hook-Signature, which is lowercase hex HMAC-SHA256(secret, raw_body) and therefore covers the raw body ONLY. A 2026-11-10 review reported, and planning confirmed, that the replay protection relies on values the signature does not cover: (1) the replay key is taken from the unsigned Request-ID / X-Sentry-Delivery headers, falling back to sha256 of the signature header string as sent, so uppercase hex or a sha256= prefix yields a new key for the same body; (2) ReplayCache in crates/engine/src/signals/source.rs (about lines 62-119) stores (source, key, msg_ts) using the unsigned Sentry-Hook-Timestamp and evicts entries whose msg_ts is older than now - 300s. As a result, anyone holding one captured delivery can replay it indefinitely with a fresh timestamp and a new Request-ID.

Operator decision (2026-11-13), binding: a delivery that was accepted once stays refused as a replay for AT LEAST 24 hours after first receipt, measured on the SERVER's clock (the `now` argument), bounded by the existing cache capacity (DEFAULT_REPLAY_CACHE_CAPACITY = 4_096, unchanged). The 300-second timestamp window (MAX_TIMESTAMP_SKEW_SECS) remains a freshness check only and must not decide how long a replay is remembered.

Do this:
1. In crates/engine/src/signals/source.rs, add `pub const DEFAULT_REPLAY_RETENTION_SECS: i64 = 86_400;`. Give ReplayCache a retention field, separate from skew_secs. Keep the existing `ReplayCache::new(capacity, skew_secs)` signature working (callers exist in tests); it and `Default` must use the 24-hour retention. Add an additive constructor that takes an explicit retention (e.g. `with_retention(capacity, skew_secs, retention_secs)`), with retention clamped to at least skew_secs. In check_and_insert: keep the skew check on the delivery timestamp exactly as is (TimestampSkew). Store the server `now` timestamp as the entry's insertion time, evict entries whose insertion time is older than now - retention, refuse a key already present with ReplayedDelivery, and when the cache is full pop the oldest entry, as today. Update the struct and method rustdoc to describe this, including that a restart (the cache is in-memory) or a full cache can shorten the window.
2. In SentrySignalSource::verify, the replay key passed to check_and_insert must be derived ONLY from the signed raw body: use the SHA-256 hex of `body` (crate::standards_waiver::sha256_hex already exists and is used here). Do not use Request-ID, X-Sentry-Delivery, the timestamp header or the signature header string in the key. Leave VerifiedDelivery.delivery_id as it is today (the informational Request-ID / X-Sentry-Delivery that the ledger records), EXCEPT change its fallback from the hash of the signature header to the body hash. Update the module header doc comment (line 13 mentions `(source, request_id_or_sig_hash, timestamp)`).
3. Do NOT change how GitHubSignalSource or StubSignalSource (in source.rs) derive their keys; they simply inherit the longer retention from the shared cache.
4. Add a NEW test file crates/engine/tests/signal_replay_test.rs. Every test name must start with `signed_replay_`, and no other test anywhere may use that prefix. Drive SentrySignalSource::verify directly with a ReplayCache and an explicit `now`, signing with SentrySignalSource::sign_body. Tests:
   a. signed_replay_fresh_headers_do_not_bypass: accept body B at now=t0 (timestamp header t0, request-id r1). Then re-send B with request-id r2 and timestamp header t0+10 at now=t0+10; with x-sentry-delivery r3; with the signature uppercased; and with a `sha256=` prefix. Each must be Err(DropReason::ReplayedDelivery).
   b. signed_replay_remembered_for_24h_on_server_clock: accept B at t0; re-send B at now = t0+301, t0+3600 and t0+86_399, each with a fresh timestamp header equal to that now and a new request-id. Each must be ReplayedDelivery.
   c. signed_replay_distinct_bodies_same_request_id_both_accepted: two different bodies with the same request-id are both Ok.
   d. signed_replay_skew_still_refused: a timestamp header 301s away from now gives Err(TimestampSkew).
   e. signed_replay_full_cache_shortens_window: with ReplayCache::new(16, 300), accept 17 distinct bodies; the first body is then accepted again (this documents capacity bounding).
   Tests a and b must fail against the old code. Reason it through or confirm it.
5. Fix existing tests that intentionally relied on the old behaviour. crates/engine/tests/signal_intake_test.rs (about lines 340-354) re-sends the SAME body with request-id req-sentry-2 and expects a deduplicated Proposed with dedupe_count 2. Now that is a replay, so build a second, DIFFERENT body for the same Sentry issue: keep the same issue id/shortId so triage still dedupes it, but add or alter a field the adapter ignores, e.g. a top-level "kranzTestDelivery": 2. Re-sign it. The assertion that an identical re-send is ReplayedDelivery (about line 329) must stay. Check crates/engine/tests/signal_injection_suite_test.rs (ReplayCache uses at about lines 216 and 404) for the same assumption. In crates/server/tests/hooks_test.rs the server keeps ONE process-wide static replay cache (SENTRY_REPLAY_CACHE in crates/server/src/hooks.rs), so byte-identical sentry_payload() bodies posted by different tests, or repeatedly within one test, will now collide as replays. Make each POSTed body unique, e.g. have the payload helper add a per-call unique top-level field the adapter ignores. Do not weaken any assertion.
6. Run `cargo fmt --all`, then `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace`. Read the bare exit codes; never pipe gate commands. Do not modify crates/engine/src/events.rs or crates/engine/src/types.rs. Do not touch docs in this feature (a later feature does).

Done when:
- cargo test --workspace signed_replay_ runs at least 5 tests from crates/engine/tests/signal_replay_test.rs and all pass
- Re-sending an accepted Sentry body with a fresh timestamp header and a new Request-ID, X-Sentry-Delivery, uppercased signature or sha256=-prefixed signature returns DropReason::ReplayedDelivery
- An accepted body is still ReplayedDelivery at server-clock now = t0+301, t0+3600 and t0+86399
- Two different signed bodies sharing one Request-ID are both accepted
- A timestamp header 301s from now is still refused with TimestampSkew
- With a 16-entry cache, the 17th distinct insert evicts the oldest, which is then accepted again
- ReplayCache::new(capacity, skew) and ReplayCache::default() still compile, and default retention is 86_400 seconds
- cargo test --workspace, clippy -D warnings, fmt --check and build all pass

### 1.2 HTTP-level replay regression test and replay-window documentation

Context: kranz is a Rust workspace. The Sentry webhook route is POST /api/hooks/sentry, implemented in crates/server/src/hooks.rs (fn sentry_hook). It maps DropReason::ReplayedDelivery to 409 Conflict and keeps a process-wide static replay cache (SENTRY_REPLAY_CACHE). A previous feature in this mission changed Sentry replay protection: the replay key is now the SHA-256 of the signed raw body (the Sentry signature covers only the body), and ReplayCache (crates/engine/src/signals/source.rs) remembers an accepted key for at least 24 hours (DEFAULT_REPLAY_RETENTION_SECS = 86_400) from insertion on the server clock, bounded by DEFAULT_REPLAY_CACHE_CAPACITY = 4_096. The 300-second MAX_TIMESTAMP_SKEW_SECS is now a freshness check only. Read those two files first to confirm the current state.

Do this:
1. In crates/server/tests/hooks_test.rs, add a test whose name starts with `sentry_http_replay_` (no other test may use that prefix), e.g. `sentry_http_replay_with_new_request_id_and_fresh_timestamp_is_conflict`. Use the existing helpers (setup(), init_sentry_repo(Some(SECRET)), hook_app, sentry_post, the payload helper, body_json). Read the existing Sentry tests around lines 740-900 first. Use a body unique to this test, since the replay cache is process-wide. POST it signed with request-id r1: expect 202. Count the lines in <repo>/.kranz/signals/ledger.jsonl. POST the byte-identical body again with request-id r2 and a freshly generated Sentry-Hook-Timestamp: expect 409 CONFLICT, and the ledger line count unchanged. You may extend sentry_post or add a sibling helper to set a request-id header. The test must fail against the pre-mission code, where a new request-id bypassed the cache.
2. Update docs/scoping/signal-intake.md. Principle 4 (about lines 101-106) and section D-B (about lines 158-163) currently describe a bounded replay cache of `(source, delivery_id_or_sig_hash, timestamp)` checked against a 5-minute skew window. Rewrite those passages to state: the replay key is derived only from values the signature covers (for Sentry, the SHA-256 of the signed raw body); an accepted delivery is remembered for at least 24 hours from first receipt, measured on the server clock (operator decision 2026-11-13); the memory is bounded by the existing 4,096-entry cache; the 300-second timestamp window is a freshness check only and does not set how long a replay is remembered; and a process restart (the cache is in-memory) or a full cache can shorten the window. Keep the doc's existing style. Also check that the rustdoc on ReplayCache, check_and_insert, MAX_TIMESTAMP_SKEW_SECS and the module header of crates/engine/src/signals/sentry.rs says the same, and the comment on sentry_hook in crates/server/src/hooks.rs (about lines 222-226). Correct any stale wording there.
3. Run `cargo fmt --all`, then `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace`, reading bare exit codes. Do not modify crates/engine/src/events.rs or types.rs.

Done when:
- cargo test --workspace sentry_http_replay_ runs at least 1 test and it passes
- Re-POSTing an accepted, byte-identical, signed Sentry body with a new Request-ID and a fresh timestamp returns 409, and ledger.jsonl does not grow
- docs/scoping/signal-intake.md states the body-derived key, the 24-hour server-clock memory, the 4,096-entry bound, freshness-only 300s, and that a restart or a full cache can shorten the window
- No stale `(source, delivery_id_or_sig_hash, timestamp)` skew-retention description remains in signal-intake.md, the ReplayCache rustdoc or the Sentry adapter header
- cargo test --workspace, clippy -D warnings, fmt --check and build all pass


## Milestone 2 — Sentry endpoint and credential-source config is settable from the global layer only

### 2.1 Operator-only trust rules for signals.sentry endpoint and credential fields

Context: kranz layers config as compiled defaults, then the GLOBAL layer (~/.kranz/config.json, or $KRANZ_HOME/config.json), then the PROJECT layer (<repo>/.kranz/config.json, which the repository controls). crates/engine/src/config.rs has CONFIG_TRUST_RULES (about lines 400-545) of ConfigTrustRule::operator_only(key, PatchClass, reason) entries. walk_project_layer / check_project_layer_keys refuse a project layer that sets any of them, via project_layer_refusal (the message names the file and the dotted key and says operator-only keys are settable from the global layer only). Parents of a rule key become 'protected containers' that must be JSON objects in the project layer. Examples: "workspace.remote" ("it names a remote workspace URL and the env var holding its token") and "slack". The `signals.sentry` block (struct SentryConfig in crates/engine/src/signals/sentry.rs) has the fields secret, allowedHosts, apiBaseUrl and tokenEnv. apiBaseUrl is the outbound write-back endpoint, tokenEnv names the environment variable the write-back bearer token is read from, and allowedHosts is the host allowlist that write-back (WriteBackClient) may contact and that evidence links are validated against. A repository must not be able to redirect write-back or choose which environment variable is read once write-back is wired.

Planning decisions (taken as the recommended defaults; the operator did not object): apiBaseUrl, tokenEnv AND allowedHosts are global-only. `secret` stays project-settable, matching the documented precedent for the per-repo inbound HMAC key `hooks.secret` (see the comment at about config.rs:463-470 and the test project_layer_still_carries_the_per_repo_webhook_secret in crates/engine/tests/config_trust_test.rs). The project layer setting a global-only field is REFUSED with an error, not ignored.

Do this:
1. Add three entries to CONFIG_TRUST_RULES: "signals.sentry.apiBaseUrl", "signals.sentry.tokenEnv" and "signals.sentry.allowedHosts". Mirror workspace.remote: ConfigTrustRule::operator_only with PatchClass::Never, each with a one-line reason in the existing style (e.g. "it names the endpoint Sentry write-back sends issue updates to", "it names the environment variable the Sentry write-back token is read from", "it is the host allowlist Sentry write-back may contact"). Add a short comment noting that signals.sentry.secret is deliberately absent, for the same reason as hooks.secret. Confirm path_matches handles these dotted keys and that the protected-container rule makes a non-object `signals` or `signals.sentry` in the project layer an error. Do not break the existing behaviour that a project layer may carry other `signals` keys (projectMap, dedupeWindowSecs, sourceDedupeWindows, rules, sentry.secret).
2. In crates/engine/tests/config_trust_test.rs, directly beside the existing project-layer refusal test (the loop around lines 480-547 that calls config::load_layers_with_roles(&[(path, Layer::Project)]) and asserts the error names the key), add a test whose name starts with `sentry_global_only_` (no other test may use that prefix), e.g. `sentry_global_only_fields_are_refused_from_the_project_layer`. For each of apiBaseUrl ("http://evil.example"), tokenEnv ("GH_TOKEN") and allowedHosts (["evil.example"]) under {"signals":{"sentry":{...}}}: (a) Layer::Project gives Err, and the message contains the field name; (b) the same file as Layer::Global loads Ok. Also assert that a project layer with only {"signals":{"sentry":{"secret":"s3cr3t"}}} loads Ok. Use the file's existing `layer(&dir, name, json)` helper.
3. crates/server/tests/hooks_test.rs has a fixture init_sentry_repo (about lines 757-785) that writes "allowedHosts": ["sentry.io"] into the PROJECT config. Remove that key (the default is already ["sentry.io"]) so the fixture stays valid; keep the secret. Search the workspace tests (crates/**/tests/**) for any other project-layer config that sets signals.sentry.apiBaseUrl, tokenEnv or allowedHosts and fix it the same way.
4. Run `cargo fmt --all`, then `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace`, reading bare exit codes. Do not modify crates/engine/src/events.rs or types.rs. Do not change crates/engine/src/signals/triage.rs in this feature (a sibling feature handles the Sentry route's own loader).

Done when:
- cargo test -p kranz-engine --test config_trust_test sentry_global_only_ runs at least 1 test and it passes
- config::load_layers_with_roles with Layer::Project refuses signals.sentry.apiBaseUrl, tokenEnv and allowedHosts, with errors naming the field
- The same content as Layer::Global loads successfully
- A project layer setting only signals.sentry.secret loads successfully
- The init_sentry_repo fixture no longer sets allowedHosts in the project layer, and all hooks_test tests pass
- cargo test --workspace, clippy -D warnings, fmt --check and build all pass

### 2.2 Sentry route config loader refuses project-layer endpoint fields and merges sentry field by field

Context: the Sentry webhook route (fn sentry_hook in crates/server/src/hooks.rs, about line 229) loads its config with SignalsConfig::load(repo_root) in crates/engine/src/signals/triage.rs (about lines 127-168), NOT through config::load. So the project-layer trust rules in crates/engine/src/config.rs never apply to it. Today SignalsConfig::load reads the global layer (crate::paths::global_config()), then the project layer (<repo>/.kranz/config.json), and for `sentry` it does `if repo_cfg.sentry.is_some() { merged.sentry = repo_cfg.sentry; }`. That lets a repository REPLACE the whole block, including apiBaseUrl (write-back endpoint), tokenEnv (the environment variable the write-back token is read from) and allowedHosts (write-back host allowlist). It also means a repository that sets only `secret` wipes the operator's global apiBaseUrl and tokenEnv. Typed parse failures are silently ignored via `if let Ok(...)`.

Planning decisions: apiBaseUrl, tokenEnv and allowedHosts are global-only. `secret` stays project-settable (per-repo inbound HMAC key, the same precedent as hooks.secret). A project layer that sets a global-only field is REFUSED with a config error, following the pattern in crates/engine/src/hooks.rs fn load_hooks (about lines 111-160). load_hooks refuses project-layer `hooks.autoQueue` with EngineError::Config("{path}: the project config layer may not set \"hooks.autoQueue\" — <reason>. Operator-only keys are settable from the global layer (~/.kranz/config.json) only."). Read load_hooks before starting.

Do this:
1. In SignalsConfig::load, inspect the project layer's RAW JSON before any typed parse, so the refusal cannot be swallowed by `if let Ok`. If signals.sentry is present and not a JSON object, return EngineError::Config naming the file and "signals.sentry". If it contains apiBaseUrl, tokenEnv or allowedHosts, return EngineError::Config naming the file and the dotted key (e.g. "signals.sentry.apiBaseUrl"), in the load_hooks wording style.
2. Merge `sentry` field by field: start from the global layer's SentryConfig (or SentryConfig::default() if absent). If the project layer's signals.sentry has `secret` (detect presence in the raw JSON), override only `secret`. The global apiBaseUrl, tokenEnv and allowedHosts must survive. If neither layer has a sentry block, keep merged.sentry = None so the route's existing 'not configured' 403 behaviour is unchanged. Leave the merge behaviour for the other signals keys unchanged.
3. Add a NEW test file crates/engine/tests/signals_config_layer_test.rs. Every test name must start with `signals_sentry_layer_`, and no other test anywhere may use that prefix outside this feature. The tests need a controlled global layer. crate::paths::global_config honours $KRANZ_HOME (see crates/engine/src/paths.rs about lines 632-660), and crates/server/tests/hooks_test.rs (about line 247) already isolates home; mirror an existing isolation helper if one exists. Env vars are process-global, so serialise these tests (one static Mutex, or a single test function). Tests:
   a. signals_sentry_layer_project_endpoint_fields_refused: for each of apiBaseUrl, tokenEnv and allowedHosts in the PROJECT config (alongside a secret), SignalsConfig::load returns Err, and the message contains the field name and the project config path.
   b. signals_sentry_layer_project_secret_merges_with_global: global config {"signals":{"sentry":{"apiBaseUrl":"https://sentry.io","tokenEnv":"SENTRY_TOKEN","allowedHosts":["sentry.io","sentry.example.com"]}}} plus project {"signals":{"sentry":{"secret":"s"}}} gives merged.sentry with secret "s", apiBaseUrl https://sentry.io, tokenEnv SENTRY_TOKEN and both hosts.
   c. signals_sentry_layer_non_object_project_block_refused: a project signals.sentry given as a JSON array or string returns Err.
   d. signals_sentry_layer_global_may_set_endpoint_fields: the global layer alone with all four fields loads Ok with the values intact.
4. Add a server-level test in crates/server/tests/hooks_test.rs named with the prefix `signals_sentry_layer_`, e.g. `signals_sentry_layer_project_api_base_url_refuses_webhook`. Write a project config with signals.sentry {secret: SECRET, apiBaseUrl: "http://evil.example"} (the shape of init_sentry_repo, but with apiBaseUrl). POST a correctly signed Sentry body to /api/hooks/sentry. Expect a non-2xx status, and that <repo>/.kranz/signals does not exist. Use a body unique to this test (the server replay cache is process-wide and keyed on body hash).
5. Run `cargo fmt --all`, then `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace`, reading bare exit codes. Do not modify crates/engine/src/events.rs or types.rs. Do not change CONFIG_TRUST_RULES in crates/engine/src/config.rs (a sibling feature owns it).

Done when:
- cargo test --workspace signals_sentry_layer_ runs at least 5 tests and all pass
- SignalsConfig::load returns a config error naming the file and key when the project layer sets signals.sentry.apiBaseUrl, tokenEnv or allowedHosts
- SignalsConfig::load refuses a non-object project signals.sentry
- A project-layer secret merged over global apiBaseUrl/tokenEnv/allowedHosts keeps all four values
- The global layer alone may set all four signals.sentry fields
- A signed POST /api/hooks/sentry to a repo whose project config sets signals.sentry.apiBaseUrl gets a non-2xx response and creates no .kranz/signals directory
- cargo test --workspace, clippy -D warnings, fmt --check and build all pass
