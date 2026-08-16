# M7 Windows containment — accepted plan and phase-1/2 proof

Status: accepted 2026-08-15. Phase 1 (explicit fail-closed safety contract) and
phase 2 (non-mutating native capability probe) are implemented and exercised by
the Windows CI leg. Native containment and its hostile live receipt remain
open; this document deliberately does not call the gap closed.

## Outcome first

Kranz will not equate Windows process-tree supervision with sandboxing. Job
Objects remain the correct timeout/kill primitive, but they do not restrict
filesystem or network access. Until a Windows boundary passes the same hostile
brief as Seatbelt/bubblewrap, both enforced session paths fail before spawn:

- `provider: process` + `enforce: fs | fs+net` refuses because Windows has no
  shipped process-sandbox backend;
- `provider: container` + enforcement also refuses on Windows, even when
  `docker.exe` is present, because the current provider assumes POSIX guest
  paths and `/dev/null` authority masks and has no Windows hostile-host receipt;
- `enforce: off` remains the explicit unsandboxed operator choice.

The same refusal applies to workers, validators, validation/final commands, and
merge gates. There is no path where a requested enforced posture silently runs
native code with the operator's ordinary user privileges.

## What parity means

Parity is equivalent guarantees, not identical implementation:

1. writable filesystem roots are exactly the worktree, private scratch, and
   reviewed `extraWrite` entries;
2. mission metadata and Kranz authority material stay unreadable/unwritable;
3. `fs+net` is offline or crosses a non-bypassable egress boundary;
4. the whole descendant tree is supervised and terminated on timeout;
5. unsupported host/runtime combinations refuse before child creation;
6. a hostile live proof and a normal-gate proof run on the target Windows host.

Cross-compilation, command-string tests, restricted-token creation, and a
successful Job Object assignment are useful implementation evidence but cannot
alone satisfy those guarantees.

## Decision record

### D-WIN-1 — Restricted tokens and Job Objects are insufficient

A restricted token can remove privileges and a Job Object can supervise a
tree, but neither narrows ordinary user ACL access to the repository's declared
roots. They remain supporting primitives, never the M7 boundary.

### D-WIN-2 — Do not infer container safety from runtime discovery

The current container builder is live-proven for POSIX container targets on
macOS/Linux. Its authority masks bind `/dev/null`, and its mount destinations
reuse host path spellings. Windows containers lack that device/path contract;
Linux containers under Docker Desktop require an explicit host-to-guest mapping.
Accordingly, finding `docker.exe` does not enable the provider on Windows.

This also keeps CI honest: GitHub documents container jobs and service
containers as Linux-runner features, so the standard `windows-latest` leg is a
valid place to prove the refusal but not a substitute for a real Windows
container boundary.

### D-WIN-3 — Native AppContainer is the primary implementation candidate

AppContainer supplies the kind of dual-principal filesystem and network
isolation M7 needs. Microsoft's June 2026 `Experimental_CreateProcessInSandbox`
API is especially relevant: its specification includes AppContainer,
read-only/read-write filesystem roots, network proxy policy, and Job Object UI
limits. It is not yet a shipping dependency because Microsoft labels it
experimental, supports only Windows 11, requires dynamic loading from
`processmodel.dll`, and does not publish a stable header. Kranz will probe it in
an isolated spike, not bind a production safety claim to version `0.1.0` of an
experimental FlatBuffer contract.

If the experimental API cannot meet the support bar, the fallback is a direct
AppContainer/LPAC launcher using stable Win32 APIs. That route must solve
toolchain executable/read access and path-specific ACL grants without leaving
persistent broad ACEs behind. A full-trust MSIX package or restricted token is
not an acceptable fallback.

Primary references:

- [Microsoft AppContainer isolation](https://learn.microsoft.com/windows/win32/secauthz/appcontainer-isolation)
- [Microsoft Launch an AppContainer](https://learn.microsoft.com/windows/win32/secauthz/implementing-an-appcontainer)
- [Microsoft Create Process in Sandbox (experimental)](https://learn.microsoft.com/windows/win32/secauthz/createprocessinsandbox)
- [GitHub Actions container-runner requirement](https://docs.github.com/actions/tutorials/use-containerized-services/use-docker-service-containers)

### D-WIN-4 — CI proves safety now; a dedicated host proves availability later

The normal Windows CI leg runs two exact named tests:

```text
sandbox::tests::windows_enforced_session_providers_fail_closed_before_spawn
command_exec::tests::windows_enforced_gate_providers_fail_closed_before_spawn
```

They cover process and container providers with enforcement requested and a
synthetically detected Docker runtime. Both must return the operator-visible
refusal before a command is constructed. The full Windows workspace suite then
keeps consumer behavior and serialization green.

Availability needs an operator-controlled Windows 11 proof host (self-hosted CI
or an equivalent disposable VM) because the native candidate is Windows 11
specific and the test must observe real kernel denials. The proof job is added
only after the launcher exists; adding a permanently queued self-hosted job now
would be ceremony, not evidence.

## Delivery sequence

1. **Shipped in this phase:** refuse the unproved Windows container provider in
   session and gate resolution; retain the existing process-provider refusal;
   name both contracts in Windows CI.
2. **Shipped in this phase:** probe `processmodel.dll` and the experimental
   export without calling it. Windows CI records the real OS build and API
   availability; absence is a supported result, never a degrade.
3. Spike one headless fixture under AppContainer: one read-only toolchain root,
   one read/write worktree, one read/write private scratch, no network. Prove an
   out-of-root write and outbound connection fail.
4. Integrate the winning launcher behind a new `SandboxBackend::AppContainer`.
   Preserve `env_clear`, bounded output, Job Object tree kill, validator
   real-checkout read denial, and engine-gate wrapping.
5. Run the full hostile brief plus normal Node/Rust gate and overhead
   measurements. Only that receipt may mark the Windows ticket and M7 parity
   complete.

## Phase-1 receipt

Local review established that the prior container resolver accepted a detected
runtime independently of target OS, while its command builder emitted POSIX
guest masks. The new resolver rejects that unverified Windows pairing for both
agent sessions and engine-run gates. Unit tests run cross-platform; the named CI
step executes them specifically on `windows-latest`, and the ordinary workspace
test leg remains the regression envelope.

This phase proves fail-closed safety. It does not prove Windows sandbox
availability, blocked-attempt telemetry, or overhead; those remain the ticket's
explicit acceptance criteria.

## Phase-2 receipt

`kranz sandbox-probe [--json]` implements the accepted discovery-only seam. On
Windows it asks the loader for `processmodel.dll` with
`LOAD_LIBRARY_SEARCH_SYSTEM32`, records the OS build, and checks for
`Experimental_CreateProcessInSandbox`. It intentionally never transmutes or
calls the export, creates an AppContainer profile, changes an ACL, or launches a
child. Every report carries `productionEnabled: false`.

The exact Windows CI probe prints its structured report and asserts a real
Windows version while retaining that production-disabled invariant. Each of
the two phase-1 exact refusal tests is separately captured and checked for a
nonzero passing-test count, so a renamed or unmatched filter cannot produce a
vacuous green safety proof.

This phase records whether a runner exposes the experimental candidate without
guessing Microsoft's unpublished FlatBuffer schema or treating an unstable API
as a production security boundary. The next phase remains a contained fixture
using a documented serialization contract or the stable AppContainer APIs.
