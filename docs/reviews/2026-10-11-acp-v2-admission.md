# ACP v2 admission: fixed resources and qualified local host

The approved change admits `codex-acp-1.11.0-arm64-v2` and
`claude-acp-0.77.0-arm64-v2` as explicit operator choices. It builds on
`6f7af155` and the [selected-limit review](2026-10-10-acp-selected-resource-limits.md).
It changes neither the operator's runtime configuration nor the installed binary.
The parent resource-budget ticket stays open for R4/R5 terminal work.

## Admitted contract

Both profiles pin image
`sha256:8da749ff0a9b09d5d4baa41172992eb92ca1beb9e5ed464feb7e608c5984457c`.
Minimum, default and maximum are identical: 2,048 MiB memory without swap,
2,000 millicores, 256 tasks, 1,024 descriptors per process, 128 MiB per file,
128 MiB per writable tmpfs and a 600-second session. Absent override fields use
the selected values; any different value is refused. Existing operator-only
configuration authority applies. No persisted event or state schema changes.

The host policy matches the exact reviewed macOS 26.6.2/build 25G83 ARM64,
Colima 0.10.3 default VZ/virtiofs VM, four CPUs/6 GiB, Docker client
29.7.2/server 29.2.1, Ubuntu 24.04.4 LTS and kernel 6.8.0-100-generic tuple.
It also checks the daemon's memory, runtime, cgroup driver/version, storage,
security options and required accounting capabilities. Native Linux and all
other tuples remain outside v2 admission. Exact version matching deliberately
requires a new review after host upgrades; it makes no compatibility-range claim.

Static profile/resource/boundary checks precede bounded host probes. Host
admission precedes opening or parsing the provider credential source. The engine
resolves installed Docker and Colima outside worker-writable roots, runs them
with cleared environments and bounded output/time, checks an owner-only default
Colima Unix socket and refuses Docker endpoint/config/TLS/API overrides. Only the
current `colima` context is admitted. The checked canonical endpoint is frozen
in the same Docker client passed to mount proof, create, attach, inspection and cleanup.
The operator's later context selection cannot redirect those operations.

These are trusted local runtime checks, not remote attestation. They assume an
operator-controlled host and daemon; they cannot prevent a host administrator
replacing the socket/daemon after admission. They do not add container scheduling
or aggregate limits across multiple simultaneous workers. Engine-run gates and
egress relays retain their existing separate resource policy.

The v2 definition digest binds its host policy, supervisor hash and the exact
three original evidence file hashes from the selected-limit review. The profile
receipt adds a compact `hostAdmission` with only compared fields and pinning
status. It omits the credential and endpoint paths. Original measurement files
are unchanged, including their historical `productionQualified: false` values.
The current receipt continues to state `fullMissionAcceptanceProven: false`.

## Scope of proof

The earlier separately authorized Codex and Claude measurements are carried
forward without another provider attempt. They establish one fixed native
command doing a small dependency-free Rust build/two tests and Node checks/two
tests, with zero downloads. The earlier selected-limit proof exercises actual
600-second expiry and all seven limits on the same image/host. Neither evidence
set establishes large builds, arbitrary feature delivery, concurrent workloads,
longer sessions, native Linux or a complete governed mission. Claude's specific
model identity remains unknown (`default` was reported).

The new ignored real-host test uses only generated synthetic credential files.
For each production profile it first supplies a missing credential source and
an invalid Docker override, requiring a host-selection refusal before credential
I/O. It then admits the real host, seeds the synthetic private home and freezes
the client. After poisoning ambient Docker context and host selection, a local
Python command runs in the pinned resource container with networking disabled,
writes only its worktree and exits. Protected resource evidence and owned
namespace cleanup must succeed, and the private credential home is removed.
No real provider adapter or login is used by this test.

Reproduce that proof on the exact supported host, with the image installed and
private shared scratch roots configured:

```sh
KRANZ_ACP_CONTAINER_TESTS=1 cargo test --workspace --locked \
  acp_worker::tests::acp_v2_admission_real_host_seeds_synthetic_credentials_and_removes_namespace \
  -- --exact --ignored --nocapture
```

Require a bare exit code of zero, exactly one passing test, two
`ACP-V2-ADMISSION-PROOF` records and no skip marker. Ordinary workspace tests
exercise missing, mistyped and drifted host fields; remote/ambiguous/overridden
Docker selection; all seven resource override boundaries; exact v1 definition
bytes; endpoint freezing; and binding to the reviewed evidence/supervisor bytes.
The existing named containment suite covers legacy launches and the shared
resource supervision and removal paths.

## Validation

All four required workspace gates passed: `cargo test --workspace` (3,176
passed, zero failed, 16 explicitly ignored), Clippy across all targets with
warnings denied, `cargo fmt --all --check` after formatting, and workspace build.
The new ignored host proof was run explicitly and passed for both v2 profiles.
All 36 named containment tests and all 12 mount-helper tests passed. Every named
CI-required case was present, with no containment or mount-helper skip marker.
The [credential-free evidence record](evidence/2026-10-11-acp-v2-admission.json)
retains both profile/resource receipts and the successful gate log hashes.
Observed memory/swap, CPU, task, descriptor and file-size limits match the
selected values; both samples are complete and both namespaces were removed.
The prior ten-minute selected-limit proof was retained, not repeated: this
change leaves its supervisor and resource flags unchanged.

The first synthetic retargeting run exposed a mount helper that resolved the
ambient Docker context again. Passing the admitted client through that helper
fixed the failure; the complete synthetic proof then passed. The first full
workspace run failed eight queue tests when the host fell below the existing
4 GiB disk-admission floor. Removing 3.83 GiB of obsolete, regenerable test
executables/static libraries restored headroom; the full suite then passed
without changing or bypassing the disk check. Retained provider evidence and
its original prepared runner were untouched.

Domain lint, the staged secret scan and the public-tree audit passed. The
optional private operator-vocabulary scan was unconfigured and reported its
skip explicitly. No provider session or real provider credential read was added.
The final inventory contained zero containers and zero owned networks; Colima
was restored to its original stopped state.
Work stayed in the isolated admission worktree; no primary checkout source,
operator runtime configuration, installed binary or remote ref was changed.

## Five-axis review

Self-review, not an independent agent review.

- Correctness: v2 admits only the measured limits and host; absent or changed
  host fields fail closed. The checked Docker client reaches every worker
  lifecycle operation. V1 definition bytes and launch arguments stay unchanged.
- Readability: one admission module owns the narrow host policy and probes;
  profile startup orders static validation, host admission and credential use.
  Documentation distinguishes historical observations from current admission.
- Architecture: uses existing bounded control I/O, Docker ownership and resource
  supervision. Optional definition fields preserve legacy hashes. No new worker
  pool, routing, prompt-management, event schema or terminal execution primitive.
- Security: no credential source is opened before host admission. The probe
  commands cannot inherit provider secrets or worker loader settings. The same
  pinned local endpoint handles credential-bearing launch and cleanup; no raw
  control output is copied into the profile receipt.
- Performance: probes are bounded and run once at v2 session preparation. V1
  performs no new host probes. Resource limits and the supervisor stay identical
  to the previously qualified candidate; there is no new polling in a session.
