---
state: done
state-note: "Done: the dedicated operator-controlled Windows 11 receipt is captured on a non-CI endpoint (Windows 11 build 26200, aarch64), closing the last hole. Phase 4 production LPAC hostile receipt returned all fifteen fields true (tokenIsAppcontainer, allApplicationPackagesDenied, toolchainRead, toolchainWriteDenied, worktreeWrite, scratchWrite, outsideWriteDenied, authorityReadDenied, realCheckoutReadDenied, sharedGitRead, overlappingLeaseSafe, tamperedGitPointerRefused, networkDenied, daclRestored, volumeRootDaclRestored). Phase 5 gate receipt: enforcement fs+net, provider process/AppContainer-LPAC, seven retained interleaved pairs per gate against a five-second payload - node 5148.40ms off vs 5234.27ms contained = 1.67%, rust 5233.45ms off vs 5291.00ms contained = 1.10%, both withinTarget against the 10% ceiling. Capturing it required three real fixes, none observable from hosted CI: the lease walk climbed into SYSTEM-owned C:/Users so the gate wrap failed closed for any profile-hosted repository; host preparation now also grants the derived profile parent the same non-inheriting 0x00120088 metadata ACE because Node realpathSync lstats every ancestor (bypass-traverse permits passing through a directory, not stat-ing it); and the self-test asked for WRITE_DAC merely to read a DACL, which forced needless elevation and masked the accurate preparation message. CAVEAT for the release record: the boundary assertions are OS mechanisms and hold for x86_64, but the 1.67%/1.10% overhead figures are aarch64 measurements while the shipped Windows artifact is x86_64-pc-windows-msvc only."
title: Define and prove a fail-closed Windows containment path for enforced sessions
priority: 1
schedule: once
---

## Goal

Close the Windows gap in M7 without pretending that Job Objects or restricted
tokens provide filesystem isolation. Choose and implement the supported Windows
boundary for `sandbox.enforce: fs | fs+net`, then prove on a Windows host that a
hostile child cannot escape it.

## Decision direction

The process provider stays fail-closed on Windows unless a native primitive can
enforce the same declared writable roots. AppContainer is the primary native
candidate because it can express filesystem and network capabilities; the
existing OCI provider also stays refused until Windows guest paths, authority
masks, and container mode have a concrete hostile-host proof. Restricted-token
or runtime-discovery claims are not accepted from API-level inference alone.

Accepted design: [`docs/scoping/m7-windows-containment.md`](../../docs/scoping/m7-windows-containment.md).

## Phase 1 — fail-closed proof

- Session resolution rejects both Windows process enforcement and the unproved
  Windows container mount contract before spawn.
- Engine-run validation/final/merge gates apply the same refusal.
- The Windows CI leg runs exact named session and gate tests before the full
  workspace suite.
- `enforce: off` is unchanged and remains explicitly unsandboxed.

This closes the unsafe inference gap; it does not close the availability ticket.

## Phase 2 — native capability probe

- `kranz sandbox-probe [--json]` records the Windows version and checks
  `processmodel.dll` for Microsoft's experimental process-sandbox export.
- The DLL is loaded from System32 only. The probe neither calls the export nor
  creates a profile, changes an ACL, or starts a child.
- API presence is reported as capability evidence and never enables production
  containment or relaxes the phase-1 fail-closed behavior.
- The Windows CI leg prints the real-host report before running exact,
  anti-vacuity-checked refusal tests.

This closes the discovery step in the accepted delivery sequence; it does not
prove a stable serialization contract, containment, or availability.

## Phase 3 — stable AppContainer hostile fixture

- A unique disposable profile launches a copy of the engine test executable
  with `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` and no capabilities.
- AppContainer-SID ACLs grant the fixture toolchain read/execute and grant only
  the fixture worktree and private scratch read/write/execute. The real checkout
  and its ACLs are untouched.
- The child is created suspended and assigned to the existing kill-on-close Job
  Object before its primary thread resumes.
- The receipt verifies the AppContainer token, allowed reads/writes, denied
  toolchain and sibling writes, and denied access to a live loopback listener.
- Windows CI runs the parent proof by an exact collision-free test name and
  keeps production session/gate enforcement fail-closed afterward.

This proves the stable native primitive on the hosted Windows runner. It does
not yet integrate agent environment construction, authority masks, bounded
output, validator read denial, or every production spawn site.

## Phase 4 — production AppContainer integration

- `provider: process` with `fs` or `fs+net` resolves
  `SandboxBackend::AppContainer`, implemented as a less-privileged
  AppContainer (LPAC) that opts out of `ALL APPLICATION PACKAGES`; Windows
  container enforcement remains refused until its distinct mount contract has
  a hostile receipt.
- Workers preserve the cleared child environment and bounded stdio path.
  Validators receive the mandatory AppContainer wrap and real-checkout source
  read denies. Validation, final, and merge-gate commands use the same launcher.
- The trusted launcher creates the hostile child suspended and assigns it to a
  kill-on-close Job Object before resume. Both postures explicitly grant
  read-only `registryRead`, required for ordinary LPAC tools to create child
  processes; `fs` additionally grants internet-client access, while `fs+net`
  supplies no network capability and is hard offline. The profile and launch
  use the same capability list.
- Each agent launch uses a disposable profile SID; each resolved engine-gate
  posture owns one disposable SID across its validation/final-gate command
  batch, so expensive ACL preparation is once per posture rather than once per
  assertion. Because ordinary Windows tools probe the local drive root and
  open `NUL` during startup, an elevated host-preparation script installs
  Microsoft's exact non-inheriting `0x00120088` metadata ACE for `S-1-15-2-1`
  and `S-1-15-2-2` on each relevant local drive and reapplies the documented
  `\Device\Null` descriptor once per boot. The root ACEs grant no listing,
  content read, or write rights. The ordinary launcher checks both prerequisites
  read-only and fails closed with the preparation command when either is absent.
  Before entering LPAC it also resolves rustup's active, already-installed
  standard toolchain with auto-install disabled and pins that absolute root in
  `RUSTUP_TOOLCHAIN`. Because hosted toolchain roots can protect their DACL
  from parent inheritance, that selected root receives its own recursive
  read/execute grant and its real `bin` directory leads the contained `PATH`;
  Rust gates therefore avoid the rustup proxy without refreshing a channel or
  writing the operator's `RUSTUP_HOME`. Windows also rewrites `TEMP`/`TMP` to
  `<LOCALAPPDATA>/Packages/<profile>/AC/Temp`; because Kranz redirects
  `LOCALAPPDATA` into private scratch after profile creation, the launcher
  materializes that exact tree only after proving it remains under a writable
  sandbox root.
  The ACL lease retains a no-follow handle for every path-specific DACL
  it changes, removes only the unique package SID's ACEs root-first, then
  deletes every private plan and the profile. A bounded host-local mutex
  serializes ACL read/modify/write
  batches, so overlapping launches preserve each other's grants. Authority,
  mission metadata, shared Cargo cache, and validator checkout denies are
  applied before spawn.
- Protected Windows CI performs the same explicit host-preparation step, then
  runs the exact production helper receipt through the built CLI and proves an
  AppContainer token plus LPAC behavior by denying a
  sibling root deliberately granted to regular AppContainers, allow/deny
  behavior, shared-Git read, tampered Git-pointer refusal, network denial, overlap-safe
  no-follow DACL cleanup, an unchanged prepared drive-root DACL, and exact
  uncontended restoration before checking
  positive session/gate resolution and continued container refusal.
- Native `.exe` backends are required; batch shims are refused at preparation
  to avoid forwarding model arguments through `cmd.exe`.

This closes production integration. Protected hosted CI also proves ordinary
Node/Rust contract gates and representative retained-posture overhead against
a five-second minimum gate payload, keeping the `10%` limit while preventing
hosted-runner scheduling jitter from dominating the stated `npm build`/`cargo
test` workload. It does not yet prove per-host `fs` filtering or the final
operator-controlled Windows 11 receipt.

## Acceptance hints

- `provider: process` plus non-off enforcement resolves only the proven stable
  AppContainer boundary; unavailable preparation fails before the hostile child
  starts.
- The selected provider supports `fs`; `fs+net` is either hard-offline or uses
  a non-bypassable egress boundary. Proxy environment variables alone do not
  count.
- Windows CI covers resolution, path/mount encoding, authority masks, and the
  fail-closed negative cases.
- A live Windows receipt runs a hostile brief that attempts an out-of-root
  write and network egress, records both denials, and verifies the allowed
  worktree change still passes its contract gate.
- No completion claim is made from cross-compilation or mocked Win32 calls.

## Operator receipt (Windows 11 build 26200, aarch64, non-CI endpoint)

Captured with `kranz sandbox-prepare --target C:\` applied once from an
elevated shell. Phase 4, production LPAC hostile boundary and exact DACL
restoration:

```json
{"tokenIsAppcontainer":true,"allApplicationPackagesDenied":true,"toolchainRead":true,"toolchainWriteDenied":true,"worktreeWrite":true,"scratchWrite":true,"outsideWriteDenied":true,"authorityReadDenied":true,"realCheckoutReadDenied":true,"sharedGitRead":true,"overlappingLeaseSafe":true,"tamperedGitPointerRefused":true,"networkDenied":true,"daclRestored":true,"volumeRootDaclRestored":true}
```

Phase 5, ordinary Node and Rust gates through the production wrap with
seven retained interleaved warm-cache pairs each:

```json
{"host":{"hostOs":"windows","windowsVersion":{"major":10,"minor":0,"build":26200},"dll":"processmodel.dll","export":"Experimental_CreateProcessInSandbox","dllSearchScope":"system32-only","experimentalSpecVersion":"0.1.0","apiStatus":"experimental-api-available","loadErrorHresult":null,"productionEnabled":true,"decision":"stable LPAC enforcement is enabled; the experimental API is detected but unused"},"enforcement":"fs+net","provider":"process/AppContainer-LPAC","overheadTargetPercent":10.0,"node":{"command":".\\node.exe node-gate.js","repetitions":7,"offSamplesMs":[5146.588333000001,5150.902875,5148.40275,5144.506417,5146.772708,5153.18375,5148.963959],"appcontainerSamplesMs":[5234.9183330000005,5234.26825,5253.132291,5225.501584,5234.97275,5219.784125,5225.992749999999],"offMedianMs":5148.40275,"appcontainerMedianMs":5234.26825,"overheadMs":85.86549999999988,"overheadPercent":1.6678085256636124,"withinTarget":true},"rust":{"command":"cargo test --quiet --manifest-path rust-gate/Cargo.toml -- --nocapture","repetitions":7,"offSamplesMs":[5280.574374999999,5233.449125,5209.941041,5264.922624999999,5147.6565,5249.109417,5173.5374170000005],"appcontainerSamplesMs":[5314.417708999999,5227.378416,5308.610667,5226.75775,5294.339833,5222.246166999999,5291.003833999999],"offMedianMs":5233.449125,"appcontainerMedianMs":5291.003833999999,"overheadMs":57.55470899999909,"overheadPercent":1.0997471767722418,"withinTarget":true}}
```

The overhead figures are aarch64 measurements. The boundary assertions are
OS mechanisms and hold for x86_64, but the released Windows artifact is
`x86_64-pc-windows-msvc` only, so the performance half of this receipt does
not attest the shipped binary's architecture.
