# kranz-acp

ACP v1 framing and a single-session protocol client shared by Kranz and future
desktop consumers. This crate is source-only until the next Kranz release.

`Client` builds and correlates initialization, session creation, prompt and
cancel messages. `Frame` preserves raw JSON-RPC requests and notifications;
`SessionUpdate` classifies the variants Kranz currently interprets without
dropping other variants or unknown fields. A consumer can interpret additional
variants from the raw update. Session cost is not normalized here.

The library depends on Serde, serde_json and Tokio's `io-util` feature. It has
no engine, process, filesystem-policy, credential or permission-authority
dependency. The consumer owns its runtime and process supervisor, bounds every
write and wait, and keeps reading while permission decisions are pending.
Treat protocol or write errors as terminal: discard the client and clean up its
owned transport. Never retry a partially written frame. Filter credentials
before persisting raw input. Cancellation is a notification, not a process kill.

Filesystem, terminal and session-load capabilities remain disabled. This is the
existing Kranz protocol subset, not an implementation of every optional ACP
capability. Permission requests are handed to the consumer unchanged; no
permission decision or tool execution is built in.

Run the provider-free thread/runtime spike:

```sh
cargo run -p kranz-acp --features conformance --example threaded_client
cargo test -p kranz-acp --all-features --all-targets
```

The example owns a current-thread runtime inside a standard thread. It exchanges
fragmented frames with an in-memory peer, retains five raw updates, completes a
turn, cancels a permission and a second turn, closes its pipe and joins. It uses
the same fixture as Kranz's normalized-event test. It proves dependency/runtime
fit, not a working Sgian daemon integration, process containment or provider
compatibility. The `conformance` feature exposes only synthetic fixtures.

See [the stream design](https://github.com/craigcode/kranz/blob/main/docs/scoping/shared-acp-client.md)
for process ownership and the later terminal-provider qualification work.
