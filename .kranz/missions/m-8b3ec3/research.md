# Research — m-8b3ec3

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/slack/Cargo.toml
- crates/slack/src/bridge.rs
- crates/slack/src/client.rs
- Cargo.toml
- Cargo.lock
- .github/workflows/ci.yml
- .kranz/merge-gates.json
- docs/knowledge/decisions/inviolable-invariants.md
- crates/server/tests/server_test.rs

## External sources

- .kranz/lessons/m-73ada5.md
- .kranz/lessons/m-9dc8c1.md
- .kranz/lessons/m-7820b9.md

## Facts

- bridge.rs is the only source file in crates/slack that references tokio-tungstenite; client.rs is pure reqwest despite the goal naming it. — `grep tungstenite|Message:: over crates/slack/**/*.rs returns only crates/slack/src/bridge.rs`
- kranz-slack is the sole remaining consumer of tokio-tungstenite 0.24; server dev-deps already resolve 0.29, so bumping the one workspace dep drops 0.24 from the graph. — `Cargo.lock: kranz-slack depends on 'tokio-tungstenite 0.24.0' (line ~1469); crates/server and axum stanzas depend on 'tokio-tungstenite 0.29.0' (lines ~184, ~1448)`
- The version lives on the workspace dependency line; slack inherits it, so crates/slack/Cargo.toml needs no edit. — `Cargo.toml:47 tokio-tungstenite = { version = "0.24", ... }; crates/slack/Cargo.toml:20 tokio-tungstenite.workspace = true`
- The MSRV CI job runs `cargo check --workspace --locked` on toolchain 1.88, so the lockfile must be regenerated and committed in the same change or that job stays red. — `.github/workflows/ci.yml msrv job (lines 50-60): toolchain 1.88 + `cargo check --workspace --locked`; workspace.rust-version = 1.88`
- A correct 0.29 usage reference already exists in-repo for the worker to mirror. — `crates/server/tests/server_test.rs:26-28 imports IntoClientRequest, connect_async, MaybeTlsStream, WebSocketStream, Message from tokio_tungstenite (0.29 dev-dep)`
- Kranz cannot merge PR #10 on GitHub, so that is a human follow-up rather than a mission assertion. — `docs/knowledge/decisions/inviolable-invariants.md 'kranz never pushes' — engine only writes local refs; sole push exception is the ref-restricted kranz exec --push cloud handoff`
- Validation-contract commands run without a shell, so absence must be proven via grep -L (correct exit code) rather than a leading '!' negation. — `.kranz/lessons/m-73ada5.md: '! grep ...' fails preflight with '\'!\' not found on PATH'`

## Ambiguities & stale docs

- The goal says 'PR #10 merges after the port' but the never-push invariant forbids Kranz from merging on GitHub; the contract is scoped to the local port landing green, with the dependabot/GitHub merge left as a human follow-up.
- The goal names client.rs as part of the connection machinery, but client.rs contains no tungstenite code; the port is confined to bridge.rs.
- tokio-tungstenite 0.29's exact Message payload types (Utf8Bytes/Bytes) and whether the rustls-tls-native-roots feature name persists at 0.29 must be verified against the 0.25-0.29 changelogs at implementation time rather than asserted from memory.
