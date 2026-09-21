# Native Linux shell qualification

Both newly authorized provider attempts passed with the Kranz engine running
directly on Ubuntu 24.04.4 ARM64, kernel 6.8.0-100-generic, in the local Colima VM.
The [projected receipts](../compatibility/acp/linux-native-tool-proof-v1.json)
retain approval, pinned artifact hashes, permission decisions/delivery, tool
completion, host checkpoints, final results and independent daemon inventories.
The engine, source and disposable worktrees were on Linux; the worker was in
the pinned Linux ARM64 vendor image. This establishes the observed fixture on
that combination, without extending the claim to bare-metal hosts or x86_64.

Claude ACP 0.77.0 / Agent SDK 0.3.270 passed in 23.330 seconds. Codex ACP 1.11.0 /
Codex 0.153.4 passed in 13.230 seconds. Each received one ACP prompt, one synced
`allow_once` decision and one separately recorded `sent` response. Each completed
the fixed native shell command, returned the exact report and file bytes, left
the primary state unchanged and shut down without denied egress. Only then did
the host create one checkpoint containing `fixture-result.txt`. These are host
checkpoints, not native Git tool invocations or full governed missions.

The approval covered the existing explicit Claude OAuth token and a minimal
Codex CLI login copy transferred over local SSH stdin. The temporary Linux
credential source was removed. Independent inventories before the batch, after
each provider and at the end found no containers or volumes and only the three
default networks. Colima was restored to stopped. No Keychain access, retry,
egress expansion or startup-policy change occurred. Both approved slots are
consumed; the receipts authorize no further provider calls.

Claude announced the default model selection; its usage reported
`claude-sonnet-5` and USD 0.0667802. Codex announced `gpt-6-astra[medium]` and
reported no cost. Model defaults were observed, not pinned. The amount is
adapter-reported, not independently verified billing; absent cost is not zero.
Underlying provider request counts are unavailable. Both attempts stayed within
the approved 120-second prompt, 180-second session and 240-second outer limits.

The projection omits text chunks, account/quota metadata apart from reported
model names, and unrelated session data. Original event indices and receipt
hashes remain. Permission digests name the original action before root-path
scrubbing, so the projected action is not a digest preimage. The engine's
`providerCompatibilityCertified: false` field remains unchanged: a reviewed
fixture pass does not enable normal mission configuration.

## Five-axis author review

- Correctness: both terminal results agree with exact deliverable checks, one
  grant/delivery/checkpoint each, ordered evidence and independent cleanup.
  Approval and artifact hashes match the prepared batch. Earlier failed
  attempts and the preparation's disk-exhaustion failure remain separate.
- Readability: current compatibility/containment notes and the S6 ticket now
  distinguish completed Linux ARM64 shell proof from pending mission admission.
  The preparation review is marked historical rather than rewritten as a pass.
- Architecture: this update adds evidence and documentation. Backend admission,
  persisted schemas, defaults and release version do not change. The existing
  runner credential/startup/checkpoint integration remains the next S6 slice.
- Security: credentials followed the approved private channels and the same
  pinned image and egress policy. The publication excludes credential values
  and private host paths. A cooperative ACP permission exchange is not proof
  that every command must ask permission; Docker provides containment.
- Performance: both invocations completed within their existing bounds, with
  no retry or production overhead. No larger local compilation was attempted
  after the retained physical-disk failure.

This is an author self-review, not an independent validator verdict. The native
Linux shell receipt completes the prepared live batch, not S6 or S7. Remaining
work is qualified ordinary worker admission, minimal credential and startup
preparation in that path, then governed acceptance with independent defect
rejection/repair, exact-tree merge judgment and portable audit evidence. The
[admission breakdown](2026-09-19-acp-linux-preparation.md#ordinary-mission-admission-concrete-remaining-work)
still applies. No production enablement or release is requested here.

## Validation

The Linux probe binary is SHA-256
`bfed0b3ea6e1185b8f948d0b8c173aa5f4d07ca5f6e9192f865822643c8d0769`,
built at source `e7e83ab`. The preceding preparation commit `a7455cc` has
[successful full-workspace CI](https://github.com/craigcode/kranz/actions/runs/35477348723),
including Linux, macOS, Windows and the wrapped macOS suite. Its runtime source,
lockfile, scripts and workflows are unchanged from the probe source. The native
Linux provider-free preparation already passed all 29 tool cases, startup
fixtures and dummy-credential runner checks. The later local workspace test
build failed from disk exhaustion and remains failed; CI and this successful
provider fixture do not rewrite that result.

This publication changes only documentation/evidence, so it does not repeat
the Rust suite. Receipt assertions verify both successful results, exact
grant/delivery/checkpoint counts and order, primary invariants, no denied
egress, approved hashes and clean inventories. Secret/domain scans, JSON and
local-link checks, formatting and post-commit knowledge freshness cover the
published files. CI on the new evidence commit is a separate result.
