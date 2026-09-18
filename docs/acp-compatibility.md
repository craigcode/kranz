# ACP adapter compatibility

S2 has deterministic transport tests, released-source inspection and live
native-login text/report passes for both Claude and Codex on macOS arm64. The current
ACP backend remains an opt-in worker backend. Validator use, same-feature resume,
client filesystem/terminal services and enforced containment remain unavailable. The
[one-call consent guide](acp-live-permissions.md) covers the separate S4 broker
and operator controls.

## Released baseline

The inspected releases are:

| Adapter | Underlying runtime | Release source |
| --- | --- | --- |
| `@agentclientprotocol/claude-agent-acp@0.77.0` | `@anthropic-ai/claude-agent-sdk@0.3.270` | [`dfe823b`](https://github.com/agentclientprotocol/claude-agent-acp/tree/dfe823b9581979cd22db40272d4469cc7e42b77e) |
| `@agentclientprotocol/codex-acp@1.11.0` | native Codex `0.153.4`, pinned by its release lock | [`51d6247`](https://github.com/agentclientprotocol/codex-acp/tree/51d6247ac7448485bfcf534b813196fafc26df59) |

The [Claude source receipt](compatibility/acp/claude-agent-acp-source.json) and
[Codex source receipt](compatibility/acp/codex-acp-source.json) retain inspected
file hashes, immutable URLs and adapter package integrities. These are
version-specific source observations, not claims about later releases.
Claude uses ACP SDK 1.4.0 and requires Node 22 or newer. Its SDK dependency is
exact; Codex's package manifest uses a range, so installation must preserve the
native runtime pin rather than resolve a newer version silently. Installation
receipts must record the complete resolved dependency lock, package integrities,
native platform package and actual Node version before a live run.

Protocol version 1 does not mean optional fields and capabilities are frozen.
Both releases expose model/session configuration; a configured Kranz model label
is not proof that the peer selected it. `Init.model` uses peer-reported model
configuration, with `unreported` when absent. The raw `Init` keeps the configured
label separately and records `configuredModelSelectionApplied: false`.
The peer's returned mode and capabilities remain in that same raw receipt.

Both adapters document optional permission presentation metadata. Standard action
fields and the exact offered option ID remain the wire contract. Claude's common
one-time IDs are `allow-once`/`reject`; Codex's are `allow_once`/`cancel`. Kranz does
not hard-code those spellings. It selects only a well-formed one-time option and
never infers a durable effect from its label. Durable-only, duplicate-ID,
missing-ID and unknown-kind option sets cancel. Unknown action kinds and mode
changes are refused. Current raw commands supersede earlier tool announcements;
display titles are not executable arguments.

See the released [Claude permission extension](https://github.com/agentclientprotocol/claude-agent-acp/blob/dfe823b9581979cd22db40272d4469cc7e42b77e/docs/permission-extension.md)
and [Codex permission extension](https://github.com/agentclientprotocol/codex-acp/blob/51d6247ac7448485bfcf534b813196fafc26df59/docs/permission-extension.md).
A selected `allow_once` is still an adapter-owned mapping, not an OS security
boundary. A peer that does not request permission can bypass this cooperative
policy; no compatibility result removes the enforced-sandbox refusal.

## What the tests establish

`crates/engine/tests/backend_acp_test.rs` exercises the real backend with local
synthetic peers. New cases use the `acp_compat_v1` prefix. They cover engine/peer
identity separation, current command checks, one-time permission options,
malformed/duplicate/oversized/invalid-UTF-8 frames, handshake rejection and timeout,
foreign updates, bounded stdin writes, peer death and same-group descendant
cleanup. The actual worker runner parses a `partial` fixture report from a peer
that stays alive after answering, without declaring a delivered feature.

A single-shot turn closes the adapter after its terminal response. macOS/Linux
retain the leader's process identity until group cleanup, then reap it. Observed
nonzero exits remain failures. Windows requires a Job Object; the new POSIX peer
fixtures do not establish Windows adapter compatibility. Escaped process groups
and real containment are S6 work.

Only natural `end_turn` produces a successful result. Context-window token counts
are not an input/output billing split. USD totals become per-turn cost only when
the necessary adjacent totals exist. Missing or invalid telemetry, or a telemetry
gap, does not become an estimated charge.

## Explicit live probe

The example `crates/engine/examples/acp_compat_probe.rs` runs one fixed prompt
through `AcpBackend` and the engine's `WorkerReport` parser. Build it with:

```sh
cargo build --example acp_compat_probe
python3 scripts/check-acp-probe.py target/debug/examples/acp_compat_probe
```

Before `--run`, the operator must authorize the fixture and a call budget. The
prepared batch is one Claude prompt and one Codex prompt, two total, no retries.
Each prompt has a 120-second limit and each session a 180-second overall limit.
These are request/time limits, not a hard monetary cap. A provider-enforced
account budget is required if a hard dollar limit is needed.

Use a reviewed, pinned local adapter installation and a private configuration
file. This Claude example contains names and paths only, never a key value:

```json
{
  "provider": "claude",
  "program": "/absolute/path/to/node",
  "args": ["/absolute/pinned-install/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js"],
  "credentialEnv": "ANTHROPIC_API_KEY",
  "receipt": "/absolute/private-output/claude-attempt-1.jsonl"
}
```

For Codex, use `provider: "codex"`, its pinned adapter entry point, and exactly one
of `CODEX_API_KEY` or `OPENAI_API_KEY`. The probe supplies a non-secret
`DEFAULT_AUTH_REQUEST` selecting `api-key`, `NO_BROWSER=1`, and initial mode
`read-only`.

For an explicitly authorized existing CLI login, replace `credentialEnv` with
`"nativeLoginHome": "/absolute/operator/home"`. Codex copies only
`.codex/auth.json` into the disposable private home; it does not copy settings,
hooks or history, and does not start an interactive login if that state fails.
Claude defaults to an existing `.claude/.credentials.json` file through this
probe. An absent file fails before adapter launch. On macOS only, an operator who
explicitly authorizes Keychain access can also set `"allowKeychain": true` with
`nativeLoginHome`. This reuses the native backend's scratch-HOME recipe: only
allowlisted credential files and a `Library/Keychains` link cross; settings,
hooks and history do not. `CLAUDE_CONFIG_DIR` remains unset on this path so it
does not change the CLI's credential lookup. No browser login is started by the
probe; any required sign-in remains an operator step. Without that opt-in the
probe does not query or link the Keychain. An explicitly supplied
`CLAUDE_CODE_OAUTH_TOKEN` is also accepted as a `credentialEnv` channel, with
exact-value receipt redaction. API-key/token and native-login inputs are mutually exclusive;
neither restores ambient HOME or configures authentication for ordinary missions.

```sh
target/debug/examples/acp_compat_probe --check /absolute/private-config.json
target/debug/examples/acp_compat_probe --run /absolute/private-config.json
```

`--check` never starts an adapter or reads a credential value. It reports only
whether the configured variable or native credential file is present and the
receipt path is unused. File presence is not an authentication verification.
`--run` requires a fresh receipt path and refuses to overwrite an earlier attempt.
The driver sends exactly one prompt, uses a new empty workspace/private HOME,
limits capture to 512 events and 2 MiB, and aborts on observed tool activity.
It applies the existing cooperative deny policy; this is not proof that no
unreported tool or network activity occurred. The adapter's provider request is
necessarily network activity.

The fixed prompt asks for this report and prohibits tool use or task execution:

```json
{"result":"partial","summary":"kranz-acp-live-fixture-v1","filesTouched":[],"testsAdded":[],"dependenciesAdded":[],"knownGaps":["Protocol fixture only; no feature implemented or mission completion claimed."],"commits":[],"commandsRun":[],"escalation":null,"questions":[]}
```

The receipt captures IDs, environment key names, capabilities, reported model/mode,
updates, result and observed cost. The injected credential is replaced before the
ordinary secret scrubber runs; private workspace paths are redacted too. Treat
receipts as private until reviewed: a scrubber cannot guarantee removal of every
sensitive value. Provider login state and raw credential values are not fixtures.
A failed attempt consumes its authorized attempt; rerunning requires another
budget decision. The `fixture` provider is for a local deterministic test peer,
not a live provider attestation.

A live pass establishes only the exercised authentication, basic text report and
normal completion path for the pinned release/platform. Explicit probe injection
does not configure ordinary missions: `backend_acp` currently has no automatic
provider credential selection or native-state seeding. Both live receipts and
that readiness limitation have been reviewed for this compatibility slice;
ordinary governed-mission acceptance remains S7 work.

## Live results (2026-09-16 UTC)

The [installation receipt](compatibility/acp/live-install.json) and complete
[resolved dependency lock](compatibility/acp/live-install-lock.json) pin Node,
adapters and native runtimes for this batch.

- Codex: [retained redacted receipt](compatibility/acp/codex-acp-native-login.jsonl).
  Existing CLI account login, one prompt, 10.3 seconds, `end_turn`, the expected
  partial report and successful cleanup. No API key was injected. Account display
  information is removed from the public receipt; its header hashes the private
  source. Missing cost remains unavailable.
- Claude: [retained redacted receipt](compatibility/acp/claude-agent-acp-native-login.jsonl).
  After the operator explicitly authorized Keychain use, the native CLI login
  passed one prompt in 5.7 seconds: `end_turn`, the exact partial report, an
  unchanged empty workspace, no observed tool calls and successful cleanup.
  No fresh browser OAuth was needed. The peer reported model selection
  `default`, mode `default` (Manual), and USD cost telemetry of 0.07431; that is
  reported usage, not proof of an account charge. The receipt retains the peer's
  model usage details rather than treating Kranz's configured label as selected.

The initial Claude credential/Keychain-link setup reached the adapter but
returned `Authentication required` after 4.8 seconds. A subsequent local
experiment accessed the operator's Keychain and prompted macOS approval. The
operator prohibited further Keychain access and that path was removed. The
later explicit authorization applies to the retained successful test above;
it is not a new default. No further provider run is scheduled.

These receipts complete the basic live-provider proof for S2. They do not prove
client terminal routing, live permission behavior, contained ACP execution or
the dependent S4/S5/S6/S7 acceptance requirements.
