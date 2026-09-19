# Native Linux preparation and remaining admission work

The engine probe now builds and runs directly on Ubuntu 24.04.4 ARM64, kernel
6.8.0-100-generic, inside the local Colima VM. Its source, scratch directories,
disposable repositories and Docker socket are on Linux. The worker runs in the
same pinned vendor image used by the earlier macOS-host proof. This qualifies
neither bare-metal Linux nor Linux x86_64 by inference.

Source is `e7e83aba7cdb8a194ede8344cf139449f7f8b44a`. The Linux probe's SHA-256 is
`bfed0b3ea6e1185b8f948d0b8c173aa5f4d07ca5f6e9192f865822643c8d0769`.
Compilation used the already installed Rust 1.94 build image
`sha256:c932e0f511ff0c152807ecc8d8789849bc94f4a4fff0ebd22f2a00bfa196f4a0`,
the committed lockfile and a source archive containing tracked files only.
The build container had no Docker socket or provider credentials. The resulting
ELF binary executes directly in the VM; the build container is not the engine
host. The [preparation receipt](../compatibility/acp/linux-preparation-v1.json)
records the artifact and log hashes, checks and limits.

## Provider-free checks and retained failure

All 29 contained synthetic tool cases passed with the native Linux engine and
the pinned vendor image: five accepted cases and 24 required refusals. The
report-only fixture and native/contained Claude key, OAuth and file-login startup
checks passed, as did Codex key and file-login startup checks. These use dummy
credentials and a deterministic Python peer, never a vendor adapter.

A later `cargo test --locked --workspace --no-run` build failed with storage I/O
errors. The VM advertised free virtual capacity, but its growing disk exhausted
the physical Mac filesystem. No test pass is claimed for that build. After
retaining its log, recovery removed this task's compilation output and an earlier
temporary vendor-binary inspection copy, restarted the VM and trimmed freed
blocks. No repository, unrelated image or user file was removed. The prepared
probe and runner still matched their hashes. The new runner was then exercised
with dummy credentials after recovery. The existing Linux CI containment proofs
remain separate evidence; this preparation does not replace the full workspace
gates or claim an additional native full-suite pass.

The private runner binds approval to a manifest, binary, configurations and
runner hash. It records an exclusive attempt before credential ingestion, uses
one prompt and one permission at most, and rejects repeat attempts. Dummy-peer
checks cover both credential paths, absent approval, changed configuration,
successful cleanup and repeat refusal. The host reader rejects permissive files,
symlinks and oversized credential files. Real credential contents were not read
to prepare or test this batch.

## Prepared live batch

This batch remains pending operator approval. It contains one attempt each for
Claude ACP 0.77.0 / Agent SDK 0.3.270 and Codex ACP 1.11.0 / Codex 0.153.4, in
image `sha256:5d0f56837d3b506013d47da6f3294cf90f24b4e4dbaf2079828d75522167b743`.
Each receives the existing fixed shell fixture:

```sh
echo kranz-acp-tool-fixture-v1 > fixture-result.txt
```

Bounds remain one ACP prompt, one `allow_once`, 120 seconds for the prompt,
180 seconds for the session and 240 seconds for the probe plus host verification.
The transport has a 300-second deadline. There are no retries or hard dollar
caps. Model selection is the peer default, recorded from initialization rather
than falsely presented as pinned. An ACP prompt is not a promise of one underlying
provider API request. The host creates the single checkpoint only after exact
report/file, primary-state and cleanup checks; this is not a native Git proof.

The proposed credential scope is the existing explicit Claude OAuth token and a
minimal private copy of the existing Codex login. Values travel over local SSH
stdin rather than argv; only the Codex auth file is staged in an owner-only Linux
source directory, removed in the runner's cleanup path. The probe seeds its own
private session home. Host settings, history and Keychain are excluded. Abrupt
loss of the runner or VM still requires reconciliation; a cleanup claim requires
receipts and daemon inventory. The provider startup policies and endpoint lists
are unchanged. A denied connection fails the probe.

New approval is required under the explicit workload/budget rule in
[the scoping document](../scoping/acp-worker-gate-contract.md#d-x--selected-implementation-decisions):
the previous two provider slots were consumed, and this batch introduces a Linux
engine host and temporary Linux credential placement. Preparation supplies no
approval and makes no provider call.

## Ordinary mission admission: concrete remaining work

The current refusal is in `config::validate`; the direct `AcpBackend` Docker
path already exists. Removing that refusal alone would leave ordinary missions
without the probe's tested credential and startup preparation. The normal worker
builder starts with contract environment and the existing Claude-specific
`AuthVerdict` path; ACP currently receives an inconclusive verdict. The probe's
Claude traffic policy and Codex plugin/login settings are not automatically part
of that path.

Keep this work within S6, followed by S7 acceptance:

1. Define an explicit worker qualification profile binding provider, host
   OS/architecture, Docker runtime, immutable image, guest executable/argv,
   startup policy and reviewed proof reference. A caller-supplied certified
   boolean or an arbitrary image digest is insufficient. Validate at ordinary
   configuration admission and again at spawn; keep unsupported pairs refused.
2. Add explicit credential-source selection through the operator-owned
   configuration layer, without persisting secret values in mission state.
   Prepare only the minimal private session home and tested startup policy.
   Refuse unavailable credentials; do not add ambient inheritance, browser or
   Keychain fallback. Cover source validation, cleanup and log redaction.
3. Exercise the ordinary runner's sandbox/egress setup and permission relay
   with a deterministic ACP peer. Keep the role worker-only and defaults
   unchanged. Do not widen validators, orchestrator, dispatch-pool candidates,
   native sandbox backends or unqualified platforms through a blanket capability
   switch. Permission callbacks remain consent evidence, not containment of
   every possible command.
4. Verify normal post-worker integrity and existing dirty-tree checkpointing in
   an isolated mission worktree. The real engine already supports checkpointing
   dirty delivery; prove that path rather than introducing a second commit
   mechanism. Empty delivery must fail, protected primary state must stay fixed,
   and neither worker nor engine may push.
5. Complete [S7](../../.kranz/tickets/acp-governed-mission-acceptance.md): approved
   plan, one-call consent, independent seeded-defect rejection and repair,
   exact integration-tree merge judgment, interruption/drift cases and portable
   audit. A provider compatibility receipt does not substitute for these checks.

## Five-axis author review

- Correctness: source/artifact pins and live/synthetic distinctions are explicit;
  the unsuccessful workspace build remains failed. No qualification or ticket
  completion is inferred from preparation.
- Readability: one receipt carries the preparation evidence, with the remaining
  implementation split above and the existing S6/S7 tickets retained.
- Architecture: documentation/evidence only; runtime admission, defaults and
  persisted schemas stay unchanged. Credential preparation belongs at the
  provider boundary, not in writer prompts.
- Security: no real credential values, Keychain or provider calls were used.
  The prospective batch has immutable inputs, explicit approval and no retries;
  the public record excludes private host paths and credential values.
- Performance: no production cost is introduced. The retained disk failure
  shows why future local compilation needs a physical-host capacity check,
  even when a VM reports ample space.

This is an author self-review, not an independent validator verdict. Publication
checks cover JSON, links, secret/domain scans, formatting and post-commit
knowledge freshness. The probe source's full workspace CI remains a separate
result; S6, ordinary enforced-ACP admission and release acceptance remain open.
