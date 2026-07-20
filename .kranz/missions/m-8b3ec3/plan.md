# Mission plan — m-8b3ec3

**Goal:** Port crates/slack's socket client from tokio-tungstenite 0.24 to 0.29, regenerate the lockfile, and unify the workspace on 0.29 with all gates green.

Branch `kranz/mission-m-8b3ec3` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$2.61 – $13.06** (expected ~$6.09). Rough estimate — live usage is authoritative; based on 46 completed mission(s).

## Considered alternatives

**Chosen approach:** A single milestone with one feature: the dep bump, the bridge.rs compile fixes, and the lockfile regeneration are inseparable — the crate does not compile between them, so they are one atomic worker session, and the behavioral safety net (pump_connection tests) plus the full-workspace gates prove equivalence.

Rejected shapes:
- **Split into two features: (a) bump the dep + port production code, then (b) adapt the tests.** — Feature (a) alone leaves `cargo test -p kranz-slack` failing to compile because the in-source tests still construct `Message::Text(String)`; the intermediate state is untestable and the 'safety net stays green' guarantee cannot hold across the boundary.
- **Keep slack on 0.24 and shim, or pin two majors in the lockfile.** — The mission explicitly forbids downgrading or splitting the version requirement, and the MSRV job's `cargo check --workspace --locked` plus the dependabot bump both require unification on 0.29.
- **Add a milestone that merges PR #10 on GitHub to 'close the loop'.** — Kranz's inviolable never-push invariant means the engine only advances local refs; a GitHub merge is unreachable and untestable inside the mission, so it is left as a human follow-up rather than a contract assertion.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The whole workspace compiles against tokio-tungstenite 0.29. 
  `cargo build --workspace`
- **[a2]** The full workspace test suite passes. 
  `cargo test --workspace --no-fail-fast`
- **[a3]** The slack crate's pump/routing/health safety-net tests pass. 
  `cargo test -p kranz-slack`
- **[a4]** No clippy warnings across the workspace and all targets. 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a5]** All code is rustfmt-clean. 
  `cargo fmt --all --check`
- **[a6]** The lockfile is coherent with the manifests (the MSRV job's locked check), proving Cargo.lock was regenerated for the bump. 
  `cargo check --workspace --locked`
- **[a7]** tokio-tungstenite 0.24 is fully absent from the resolved dependency graph — the version is unified on 0.29, not split or downgraded. 
  `grep -L tokio-tungstenite.0.24 Cargo.lock`
- **[a8]** The pump_connection generic tests keep their behavioral assertions intact — the Ok(false)/Ok(true) outcomes, envelope dedup across reconnects, idle-timeout reconnect, disconnect-frame short-circuit, and stream-error reconnect — with test-body edits limited to Message payload-type adaptation; no pump/routing/health test was deleted or loosened. *(agent judgement)*

## Milestone 1 — Slack socket client runs on tokio-tungstenite 0.29, workspace unified

### 1.1 Port bridge.rs connection machinery to tokio-tungstenite 0.29 and regenerate the lockfile

Port the Slack Socket Mode client from tokio-tungstenite 0.24 to 0.29 and regenerate the lockfile so the workspace resolves a single major. Dependabot PR #10 makes this exact bump and fails compilation across all rust jobs; you are landing the equivalent change locally. Do NOT downgrade the bump or split the version requirement — unify on 0.29.

FILES YOU MAY TOUCH (and only these):
1. `Cargo.toml` (workspace root, line ~47): change `tokio-tungstenite = { version = "0.24", features = ["rustls-tls-native-roots"] }` to version `0.29`. VERIFY against the 0.29 docs/changelog that the `rustls-tls-native-roots` feature name still exists at 0.29; if it was renamed, use the current equivalent so the TLS wiring is preserved. The slack crate inherits this via `tokio-tungstenite.workspace = true`, so `crates/slack/Cargo.toml` needs NO edit.
2. `Cargo.lock`: regenerate (e.g. `cargo update -p tokio-tungstenite --precise 0.29.0` then let cargo resolve, or `cargo build`). After regeneration, tokio-tungstenite 0.24 must be entirely gone — `kranz-slack` is its sole remaining consumer (crates/server dev-deps already resolve 0.29). Confirm with `grep -L tokio-tungstenite.0.24 Cargo.lock` (exit 0 == absent). The lockfile change MUST be committed, or the MSRV job's `cargo check --workspace --locked` stays red.
3. `crates/slack/src/bridge.rs`: the ONLY source file that touches tungstenite. `crates/slack/src/client.rs` is pure reqwest — do not touch it. Adapt these call sites:
   - Import at line ~46: `use tokio_tungstenite::tungstenite::Message;`
   - `tokio_tungstenite::connect_async(&url)` at line ~917 (`connect_once`).
   - The pump loop at lines ~1000-1119 (`pump_connection_context`): `Message::Close(None)` (~1003), `Message::Text(text)` match arm (~1024), `Message::Text(json!({...}).to_string())` ack send (~1041), `Message::Ping(payload)` / `Message::Pong(payload)` (~1104-1105), `Message::Close(_)` (~1109).
   - The in-source tests at lines ~7062-7304: `Message::Text(frame.to_string())` (~7073) and `Message::Text(r#"..."#.into())` (~7279).

KNOWN CHURN (verify against the 0.25-0.29 changelogs; treat as hypotheses, not gospel): the `Message::Text` / `Ping` / `Pong` payload types changed from `String` / `Vec<u8>` to bytes-backed types (`Utf8Bytes` / `Bytes`) somewhere in 0.25-0.26 — adapt every construction site (`.into()` conversions from `String`/`&str`/`Vec<u8>` typically work). The read-side pong echo must remain a direct move of the ping payload (`Message::Ping(payload) => write.send(Message::Pong(payload))`), which stays valid since both sides are the same bytes type. `connect_async` (now takes an `IntoClientRequest`; `&String`/`&str` should still work) and handshake error types are the other declared risk areas — the only use of the connect result/error here is `.context(...)`, so an error-type change should not require signature changes.

AUTHORITATIVE 0.29 REFERENCE ALREADY IN-REPO: `crates/server/tests/server_test.rs` (lines ~26-28 and ~1382) already uses tokio-tungstenite 0.29's `IntoClientRequest`, `connect_async`, `WebSocketStream`, and `Message` correctly. Mirror its usage instead of guessing.

SAFETY NET — DO NOT WEAKEN: the `pump_connection` generic tests (generic over Stream/Sink, driven by in-memory `futures_util::stream::iter` / `sink::drain`) are the port's proof of behavioral equivalence: `pump_connection_dedups_envelopes_across_reconnects`, `idle_timeout_triggers_reconnect`, `stream_error_triggers_prompt_reconnect`, `disconnect_frame_triggers_reconnect`, plus the routing/health tests. You may ONLY change how a `Message` value is constructed in these tests (payload type adaptation). Do NOT change any assertion, expected outcome, timeout, or the set of tests. All must still exist and pass.

WRITE/RUN TESTS FIRST: before editing production code, run `cargo build -p kranz-slack --tests` to see the compile errors, then adapt. When done, `cargo build -p kranz-slack`, `cargo test -p kranz-slack`, `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, and `cargo check --workspace --locked` must all pass. If tokio-tungstenite 0.29's own MSRV turns out to exceed the workspace `rust-version` (1.88 in Cargo.toml), STOP and report it as a blocker rather than bumping the MSRV.

Done when:
- `cargo build -p kranz-slack` and `cargo test -p kranz-slack` both pass.
- `cargo build --workspace` and `cargo test --workspace --no-fail-fast` both pass.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` are clean.
- `cargo check --workspace --locked` passes, proving Cargo.lock was regenerated and committed.
- `grep -L tokio-tungstenite.0.24 Cargo.lock` succeeds (0.24 fully removed from the resolved graph).
- The workspace Cargo.toml declares tokio-tungstenite version 0.29 with the TLS-roots feature preserved.
- Every pre-existing pump_connection*, idle_timeout*, stream_error*, and disconnect_frame* test still exists and passes with its assertions unchanged.

