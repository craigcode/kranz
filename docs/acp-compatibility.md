# ACP adapter compatibility

S2 has deterministic transport tests, released-source inspection and live
native-login text/report passes for both Claude and Codex on macOS arm64. The current
ACP backend remains an opt-in worker backend. Validator use, same-feature resume,
client filesystem/terminal services and enforced containment in ordinary missions
remain unavailable. The [one-call consent guide](acp-live-permissions.md) covers the separate S4 broker
and operator controls.

The [S6 container proof path](acp-containment.md) now exercises the direct
backend API with deterministic peers and basic contained Claude/Codex report
passes using private authentication on the pinned Linux ARM64 image. Ordinary
mission configuration still refuses enforced ACP; these narrow checks do not
certify arbitrary images, Linux hosts or governed missions.

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

Before starting either Codex authentication path, the probe writes its own
`CODEX_HOME/config.toml` with file-only credential storage and both `plugins`
and `remote_plugin` disabled. These startup settings suppress plugin warmups
outside the bounded fixture; session-level settings arrive too late. This
policy is specific to the compatibility probe, not ordinary missions. The
preflight and start receipt include the exact TOML as `codexStartupConfig`;
`codexStartupConfigWritten` distinguishes the preview from a completed write.
The receipt records intended startup policy, not independently verified runtime
enforcement. See the [source trace and offline checks](reviews/2026-09-19-codex-contained-egress.md).

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

`--check` never starts an adapter, writes startup configuration or reads a
credential value. It reports credential presence, receipt availability and the
intended startup configuration. File presence is not authentication verification.
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

## Contained Codex follow-up (2026-09-19 UTC)

With plugins disabled in its private startup configuration, the pinned Linux
ARM64 Codex image passed the same one-prompt fixture under Docker on the macOS
Colima host. It used an isolated copy of the existing CLI login and returned the
exact report with no tools, denied connections or workspace changes, in 8.5
seconds. Cleanup was confirmed. The earlier attempt remains recorded as failed
on denied egress; the successful follow-up did not broaden the allowlist or
access Keychain. See the [contained proof record](compatibility/acp/codex-contained-plugins-disabled-proof.json)
and [containment guide](acp-containment.md) for both receipts and the remaining
containment, configuration-admission and mission-acceptance work. This verifies report delivery and
authentication; full S6 certification and Linux-host proof remain open.

## Contained Claude follow-up (2026-09-19 UTC)

The same pinned Linux ARM64 image passed one authorized Claude report prompt
under Docker on macOS/Colima using an explicitly provisioned
`CLAUDE_CODE_OAUTH_TOKEN`. Claude ACP 0.77.0 / Agent SDK 0.3.270 returned the
exact fixture in 5.2 seconds: no tool events, denied connections or workspace
changes. Cleanup passed, and a separate owner-label query and daemon inventory
confirmed absence. No retry, allowlist expansion or Keychain access occurred.
The [proof summary](compatibility/acp/claude-contained-oauth-proof.json) links the
[redacted receipt](compatibility/acp/claude-acp-contained-oauth.jsonl) and hashes
both its private source and public export. Account-limit metadata and available
command inventories are removed; the token and private workspace paths were
scrubbed by the probe before persistence.

The adapter initially emitted `kind: none` / `Not logged in`, despite the later
successful environment-token-backed request. That notification is retained as
observed, not treated as an authentication verdict. Model selection was the
peer's `default`; usage named both `claude-sonnet-5` and
`claude-haiku-4-5-20251001`, with an adapter-reported cost of USD 0.0426852.
One ACP prompt is the enforced call budget; the underlying provider request
count is not independently verified. The 120-second prompt and 180-second
session limits remain time bounds, not a hard monetary cap.

This closes the basic Claude contained report/authentication check. The
[mount-helper cleanup follow-up](reviews/2026-09-19-mount-helper-cleanup.md)
addresses the separate preflight lifetime gap. Broader containment qualification,
production admission and S7 mission acceptance remain open.

## Contained one-command qualification fixture

`"mode": "shell-once"` is an explicit opt-in to the next S6 probe. Omitting `mode`
keeps `report-only`, which still refuses tool activity. Tool mode requires the
pinned Docker wrapper; it cannot run natively or use macOS Keychain. `--check`
prints the complete fixed prompt, command, one-permission limit and session
bounds without opening credentials, starting an adapter or creating a receipt.

The workload is exactly one native shell invocation, in the disposable worktree:

```sh
echo kranz-acp-tool-fixture-v1 > fixture-result.txt
```

The probe accepts only an exact command proposal with one unambiguous
`allow_once` option. It rejects a different working directory, extra authority,
unknown input fields, durable/ambiguous choices, repeated requests and action
drift. The request and fixture decision are flushed and synced before the
response is queued; the transport's separate `Sent` event and matching successful
tool completion are required. In addition to the bare fixture command, the
permission matcher accepts exactly `/usr/bin/bash -lc 'echo kranz-acp-tool-fixture-v1 > fixture-result.txt'`,
the wrapper observed from pinned Codex ACP. Other legacy wrappers are accepted
only in tool notifications, not permission requests. No shell normalization or
prefix matching is performed. This decision is attributed to the
operator-authorized probe fixture, not to an invented human gate actor.

A pass also requires the exact report and file bytes, no extra worktree paths,
unchanged primary source/configuration/index/base ref, clean session shutdown and
no denied egress. Only after shutdown does the host use hardened Git to commit
the one expected file. The receipt must show one feature commit on the fixture
branch; the primary branch remains at its base. This is a host checkpoint of a
synthetic deliverable, not a completed mission or a human merge.

Pinned Codex ACP 1.11.0 calls its initial mode `read-only`, but that preset uses
`workspaceWrite` and `on-request`; it does not request permission for every
workspace command. See its [mode definition](https://github.com/agentclientprotocol/codex-acp/blob/51d6247ac7448485bfcf534b813196fafc26df59/src/AgentMode.ts)
and [shell approval tests](https://github.com/agentclientprotocol/codex-acp/blob/51d6247ac7448485bfcf534b813196fafc26df59/src/__tests__/CodexACPAgent/e2e/acp-e2e-shell-approval.test.ts).
The fixture therefore asks for explicit escalation with no prefix rule and
fails if consent evidence is missing. A successful cooperative permission
exchange does not prove that every native command must use that exchange;
the Docker boundary remains the enforcement mechanism.

The fixed prompt allows one invocation and no retry. It keeps the 120-second
prompt and 180-second session bounds, 512-event / 2 MiB capture limits, and no
hard dollar cap. Local Git preparation/checkpoint verification is outside the
session timer (reported by `hostGitOutsideSessionBudget`); use a 240-second outer
process deadline for the whole attempt, retaining its timeout/failure receipt.
One ACP prompt is not a verified count of underlying provider API requests.
The earlier report-only authorizations do not authorize this different workload.
Authorize the reviewed one-Claude/one-Codex batch before accessing credentials or
running either vendor. Keep each attempt's new receipt and never auto-retry,
expand egress, switch modes or broaden the allowed command to obtain a pass.

Run the provider-free harness check with an installed pinned Python image:

```sh
cargo build --workspace --example acp_compat_probe
python3 scripts/check-acp-tool-probe.py /absolute/target/debug/examples/acp_compat_probe python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d
```

It runs 29 real-container cases, including positive controls, malformed or
missing consent, unexpected commands/paths/output, tool failure and duplicate
requests. Each refusal must match its intended reason; every case checks daemon
absence independently. Linux CI runs this script without credentials and retains
its log. These are synthetic fixture proofs; native vendor tool-use qualification,
Linux vendor receipts and ordinary enforced-ACP admission remain separate work.
See the [fixture review](reviews/2026-09-19-acp-tool-probe.md) and
[live-attempt follow-up](reviews/2026-09-19-acp-native-tool-attempts.md).

The first approved live batch at `0d41f41` failed for both providers; its
[projected receipts](compatibility/acp/native-tool-attempts-v1.json) retain the
failures. Claude delivered one grant and reported tool completion, then failed
on denied Datadog telemetry before host file/commit verification. Codex's wrapped
permission command was rejected before any grant. Both namespaces were removed.

The probe now sets `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` before Claude
starts, for either mode and every supported credential channel. `--check` shows
this fixed setting without applying it; the start receipt records its application.
This follows Claude's [documented traffic policy](https://code.claude.com/docs/en/env-vars)
and the pinned SDK's telemetry guard. It does not add a network destination or
ignore denied traffic. Synthetic startup peers verify delivery of the setting;
only a separately approved new live batch can verify vendor behavior after these
fixes. The first batch's authorization is consumed; it is never retried.
