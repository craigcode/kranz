# M7 Windows containment — accepted plan and production AppContainer boundary

Status: accepted 2026-08-15. Phases 1–4 and the hosted part of phase 5 are
implemented. Protected Windows CI proves the production AppContainer launcher,
ordinary Node/Rust gates, and retained-posture overhead before the process
provider is enabled. The dedicated operator-controlled Windows 11 receipt
remains open, so this document does not yet call the full parity ticket closed.

## Outcome first

Kranz does not equate Windows process-tree supervision with sandboxing. Job
Objects remain the correct timeout/kill primitive, but less-privileged
AppContainer (LPAC) tokens and SID-scoped ACLs provide the filesystem/network
boundary. LPAC is required here because it opts out of the broad
`ALL APPLICATION PACKAGES` principal that would otherwise expose resources
shared with ordinary AppContainers. The shipped posture is now:

- `provider: process` + `enforce: fs | fs+net` resolves the stable
  `SandboxBackend::AppContainer` launcher;
- `provider: container` + enforcement also refuses on Windows, even when
  `docker.exe` is present, because the current provider assumes POSIX guest
  paths and `/dev/null` authority masks and has no Windows hostile-host receipt;
- `enforce: off` remains the explicit unsandboxed operator choice.

The process boundary applies to workers, validators, validation/final commands,
and merge gates. There is no path where a requested enforced posture silently
runs native code with the operator's ordinary user privileges. `fs` grants the
AppContainer internet-client capability; `fs+net` supplies no network
capability and is hard offline. Both postures carry only the explicit
read-only `registryRead` capability that LPAC requires for ordinary Windows
tools to create descendant processes; the same capability list is fixed at
profile creation and supplied again at each launch.

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

The current container builder is release-supported on Linux. A macOS operator
receipt exists, but hosted macOS cannot renew it as a CI gate; the public
support matrix therefore refuses the provider on macOS too. Its authority
masks bind `/dev/null`, and its mount destinations reuse host path spellings.
Windows containers lack that device/path contract; Linux containers under
Docker Desktop require an explicit host-to-guest mapping. Accordingly, finding
`docker.exe` does not enable the provider on Windows.

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
toolchain executable/read access and path-specific ACL grants without broad
inherited or content-access ACEs. A minimal host-wide, non-inheriting drive-root
metadata grant is acceptable only when it matches Microsoft's documented tuple
and is installed by a separate elevated preparation step. A full-trust MSIX
package or restricted token is not an acceptable fallback.

Primary references:

- [Microsoft AppContainer isolation](https://learn.microsoft.com/windows/win32/secauthz/appcontainer-isolation)
- [Microsoft Launch an AppContainer](https://learn.microsoft.com/windows/win32/secauthz/implementing-an-appcontainer)
- [Microsoft MXC AppContainer host preparation](https://github.com/microsoft/mxc/blob/main/docs/host-prep.md)
- [Microsoft Create Process in Sandbox (experimental)](https://learn.microsoft.com/windows/win32/secauthz/createprocessinsandbox)
- [GitHub Actions container-runner requirement](https://docs.github.com/actions/tutorials/use-containerized-services/use-docker-service-containers)

### D-WIN-4 — CI proves production safety; a dedicated host closes parity later

The normal Windows CI leg runs the exact production hostile receipt and two
resolution contracts:

```text
windows_production_appcontainer_helper_enforces_and_restores_boundary
sandbox::tests::windows_enforced_session_process_resolves_appcontainer_while_container_fails_closed
command_exec::tests::windows_enforced_gate_process_resolves_appcontainer_while_container_fails_closed
```

They prove the exact launcher/ACL lease through the built `kranz.exe`, positive
process-provider resolution, and continued container-provider refusal with a
synthetically detected Docker runtime. The full Windows workspace suite then
keeps consumer behavior and serialization green.

The stable AppContainer path is not tied to the Windows-11-only experimental
API. Protected hosted CI proves the normal Node/Rust gate and overhead on
Windows Server; full parity still needs an operator-controlled Windows 11 host
(self-hosted CI or an equivalent disposable VM) to repeat the hostile and
normal-gate brief as a host-specific receipt. A permanently queued self-hosted
job without a managed runner would be ceremony, not evidence.

## Delivery sequence

1. **Shipped in this phase:** refuse the unproved Windows container provider in
   session and gate resolution; retain the existing process-provider refusal;
   name both contracts in Windows CI.
2. **Shipped in this phase:** probe `processmodel.dll` and the experimental
   export without calling it. Windows CI records the real OS build and API
   availability; absence is a supported result, never a degrade.
3. **Shipped:** a unique, disposable AppContainer profile launches a copy of
   the engine test executable from a read/execute-only toolchain root under a
   kill-on-close Job Object. The child proves its token is an AppContainer,
   reads but cannot write the toolchain, writes the disposable worktree and
   private scratch, cannot write a sibling root, and cannot connect to a live
   loopback listener because the launch supplies no network capability. No
   real-checkout ACL is changed.
4. **Shipped:** integrate the stable launcher behind
   `SandboxBackend::AppContainer`, preserving `env_clear`, bounded output, Job
   Object tree kill, validator real-checkout read denial, and engine-gate
   wrapping. The protected receipt proves exact DACL restoration after the
   child exits.
5. **Shipped on protected hosted Windows:** run the full hostile brief plus
   normal Node/Rust gate and retained-posture overhead measurements against a
   five-second minimum representative gate payload, retaining the `10%` target.
   Repeat that receipt on the operator-controlled Windows 11 host before
   marking the Windows ticket and M7 parity complete.

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
child. The report now carries `productionEnabled: true` on Windows because the
separate stable AppContainer backend is shipped; experimental API availability
still never controls that decision.

The exact Windows CI probe prints its structured report and asserts a real
Windows version while keeping experimental API availability independent of the
stable production decision. Each exact containment/resolution test is captured
and checked for a nonzero passing-test count, so a renamed or unmatched filter
cannot produce a vacuous green safety proof.

This phase records whether a runner exposes the experimental candidate without
guessing Microsoft's unpublished FlatBuffer schema or treating an unstable API
as a production security boundary.

## Phase-4 production receipt, phase-5 hosted gates, and constraints

The engine launches a trusted copy of its host executable, which creates the
hostile child suspended with `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` and
`PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY` set to opt out, assigns
it to a kill-on-close Job Object, and only then resumes it. Agent sessions use
one unique disposable profile SID per launch; a resolved engine-gate posture
uses one SID across its validation/final-gate command batch, matching the other
providers' once-per-resolution setup contract. That SID receives inherited
ACLs for the worktree/private scratch and targeted read/execute access for the
executable, toolchain, and shared Git metadata. The shared Git root is derived
from the trusted repository and every writable worktree pointer must resolve
back to that same common directory.
Node, Cargo, and cmd also probe their local drive root and open `NUL` during
startup. LPAC's dual-principal access check means the package SID alone is
insufficient.
[`scripts/prepare-windows-appcontainer.ps1`](../../scripts/prepare-windows-appcontainer.ps1)
is therefore a separate, elevated host step: on every relevant local drive
root it installs the exact non-inheriting `0x00120088` metadata mask
(`FILE_READ_ATTRIBUTES | FILE_READ_EA | READ_CONTROL | SYNCHRONIZE`) for
`ALL APPLICATION PACKAGES` (`S-1-15-2-1`) and
`ALL RESTRICTED APPLICATION PACKAGES` (`S-1-15-2-2`). It refuses conflicting
explicit ACEs instead of merging rights. The mask grants no root listing,
content read, or write access, and does not propagate below the root. The
same command reapplies Microsoft's documented `\Device\Null` security
descriptor once per boot; Windows resets it at restart, and without both
package ACEs ordinary redirected tools fail during startup with access denied.
The script invokes the public
`kranz sandbox-prepare --target <drive-root>` command; that compiled path
follows Microsoft's `GetNamedSecurityInfoW` →
`SetEntriesInAclW` → `SetNamedSecurityInfoW` sequence and verifies the resulting
tuples instead of using the managed `Set-Acl` path. The
ordinary Kranz launcher verifies the root tuples and null-device package ACEs
read-only; missing preparation fails before the hostile child starts and names
the elevated command to run. This follows Microsoft's AppContainer
host-preparation contract instead of trying to substitute a per-profile
capability for a well-known restricted-package principal.
Rustup's multiplexing proxy is also treated as read-only toolchain state. At
lease creation the trusted parent runs `rustup which cargo` with automatic
installation disabled, accepts only an already-installed standard toolchain
under `RUSTUP_HOME/toolchains`, and supplies its absolute root as
`RUSTUP_TOOLCHAIN` to contained descendants. Hosted toolchain roots can carry
a protected DACL that does not inherit the `RUSTUP_HOME` grant, so the selected
root receives a direct recursive read/execute grant and its real `bin`
directory leads the contained `PATH`. That bypasses the rustup proxy and
prevents a floating channel refresh from attempting to write the operator's
`RUSTUP_HOME`; it does not add a writable root or permit installation from the
network.
Windows additionally rewrites `TEMP` and `TMP` beneath the AppContainer profile
location. Since Kranz redirects `LOCALAPPDATA` into the session-private scratch
after the trusted parent creates the profile, the launcher materializes the
documented `<LOCALAPPDATA>/Packages/<profile>/AC/Temp` tree before launch and
refuses unless the canonical redirected root is already inside the sandbox's
writable set.
Authority files, mission metadata, shared Cargo caches, and validator
real-checkout sources receive explicit deny entries. Original DACL bytes and
no-follow object handles are retained before mutation. Cleanup removes only the
unique profile SID's ACEs, root-first, through those handles; overlapping
launches therefore keep their live grants. A bounded host-local mutex
serializes the DACL read/modify/write batches across Kranz processes, while an
uncontended descriptor returns byte-for-byte to its baseline. The profile and
all private per-command launch plans are then deleted.

Both shipped mission hosts route that private re-entry before ordinary
initialization: the `kranz` CLI and the embedded-server Tauri desktop binary.
Other binaries embedding `kranz-engine` must provide the same early dispatch
before they can host enforced Windows missions.

The protected hosted-Windows receipt verifies an AppContainer token and LPAC
behavior, toolchain read with write denial, worktree/private-scratch writes,
sibling-root write denial even after a separate sibling is granted to
`ALL APPLICATION PACKAGES`, authority and real-checkout read denial, shared-Git
read access, rejection of a tampered worktree Git pointer, hard-offline
`fs+net`, safe overlapping leases, and exact uncontended worktree plus local
drive-root DACL stability after the explicit host-preparation baseline. The
resolver is enabled only behind that exact production path.

Honest constraints remain. Agent backends must resolve to a native `.exe`;
batch shims are refused because forwarding model-generated arguments through
`cmd.exe` would add a command-injection surface. `fs+net` is fully offline,
while `fs` grants ordinary internet-client access rather than per-host egress
filtering. Preparation rejects writable roots that overlap authority material,
mission metadata, or shared Cargo caches. A hard engine crash may leave an
inert ACE for a unique deleted profile SID; normal exits, errors, and aborts
restore the captured descriptors.
