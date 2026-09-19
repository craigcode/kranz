# Contained Codex endpoint investigation

Keep `sdmntprsouthcentralus.oaiusercontent.com:443` blocked. The first contained
Codex probe completed its fixed text response while that destination was denied,
so access was not needed to produce that response. It still failed qualification:
the five denied requests remain in the [original receipt](../compatibility/acp/codex-acp-contained-denied-egress.jsonl).

Automatic downloads of account-installed plugins are a plausible explanation.
This is source-supported inference, not attribution of those five requests. The
CONNECT receipt identifies a hostname, not an HTTPS path or requesting runtime
component; it cannot identify five plugins, distinguish retries, or prove that
all five requests had the same purpose. No authenticated provider or model call
was made during this investigation.

## Pinned source trace

The inspected runtime is Codex `0.153.4`, release commit
`3d2ee51ca2d5db578f328aa75e20aa22c0197c9a`, behind Codex ACP `1.11.0`.
The [source receipt](../compatibility/acp/codex-runtime-plugin-source.json)
records immutable source URLs and SHA-256 digests; the runtime source blobs were
also checked against Git's release tree.

- Both `plugins` and `remote_plugin` default to true in
  [the runtime feature table](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/features/src/lib.rs#L1322).
- App-server schedules plugin warmups at startup in
  [message_processor.rs](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/app-server/src/message_processor.rs#L525).
  [The startup implementation](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core-plugins/src/manager.rs#L2735)
  runs installed-plugin bundle sync under `plugins_enabled`. Turning off
  `remote_plugin` alone changes catalog scope; it does not prevent that sync.
- [Bundle sync](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core-plugins/src/remote/remote_installed_plugin_sync.rs#L147)
  fetches account-installed plugins with download URLs, then materializes bundles
  missing from the local cache. A fresh private HOME therefore does not imply
  an empty account plugin inventory. Download failures are recorded and the
  loop continues. These actions do not require a model tool call.
- [The downloader](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core-plugins/src/remote_bundle.rs#L301)
  uses the service-supplied URL. This supports the CDN hypothesis but does not
  establish that the particular denied hostname came from a plugin response.
- The separately inspected
  [cloud configuration loader](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/cloud-config/src/backend.rs#L61)
  consumes configuration fragments returned by the backend. That path does not
  provide evidence identifying the denied hostname either.

The ACP adapter's
[connection factory](https://github.com/agentclientprotocol/codex-acp/blob/51d6247ac7448485bfcf534b813196fafc26df59/src/CodexJsonRpcConnection.ts#L15)
spawns `codex app-server` without config arguments.
[Its entry point](https://github.com/agentclientprotocol/codex-acp/blob/51d6247ac7448485bfcf534b813196fafc26df59/src/index.ts#L78)
passes `CODEX_CONFIG` to its client state separately from process creation.
Do not rely on that session configuration to suppress startup activity.

## Offline configuration evidence

The [offline receipt](../compatibility/acp/codex-plugin-config-offline.json)
records three `codex features list` checks against the same immutable Linux ARM64
image as the failed probe. The container had no network, host bind mounts or
credentials; it used a read-only root and disposable homes under `/tmp`.
No app-server, ACP adapter or model prompt was started. Cleanup confirmed that
the container was absent.

| Private startup configuration | Effective `plugins` | Effective `remote_plugin` |
| --- | --- | --- |
| Defaults | true | true |
| Only `remote_plugin = false` | true | false |
| Both false | false | false |

Each invocation used file-only credential storage and the pinned bundled CLI,
with an explicit `CODEX_HOME`. This verifies configuration recognition by that
runtime. Source inspection supplies the startup guard; this offline check does
not reproduce the authenticated downloads or prove the next live run will pass.
The current [OpenAI configuration reference](https://developers.openai.com/codex/config-reference/)
documents the global feature switches and cautions that disabling an individual
plugin does not prevent marketplace refresh from installing or refreshing it.

## Proposed probe change and next acceptance

Before the next Codex adapter starts, the probe should write the following into
its own disposable `CODEX_HOME/config.toml`, after minimal credential seeding:

```toml
cli_auth_credentials_store = "file"

[features]
plugins = false
remote_plugin = false
```

This configuration has been checked offline; it is **not yet written by
`acp_compat_probe`**. Apply it to both explicit-key and copied-login Codex probe
paths, record the intended startup policy in the receipt, and test that it exists
before spawn without copying or editing the operator's settings. Keep this
restriction scoped to the no-tools compatibility probe; ordinary missions may
need separately approved plugins. It is a workload reduction, not a replacement
for the container network boundary.

After that change passes review and regression checks, prepare one newly
authorized run with the same image, credentials channel, fixed prompt, allowlist
and time bounds. Preserve the original failure and write a new receipt. Require
zero denied connections, the exact report, no tool use or workspace changes,
and confirmed cleanup. If denials persist, stop and collect narrowly scrubbed
component diagnostics before considering any endpoint policy change. Avoid
publishing raw plugin download URLs, which may contain signed query strings.

This investigation changes neither production ACP admission nor the S6/S7
completion state. Claude still needs its own Linux-compatible credential and
authorized contained proof.

## Review

Correctness: observed failure, offline configuration behavior and inferred cause
are separate claims. Architecture: the proposed control belongs to probe startup,
outside session prompts. Security: the endpoint remains denied and no additional
authority was granted. Readability: source and runtime receipts are linked from
the operator guide. Performance: the investigation made three bounded offline
configuration queries and no provider calls. This is a self-review.
