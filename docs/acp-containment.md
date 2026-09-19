# ACP containment proof path

Ordinary mission configuration still refuses `backend: "acp"` with enforced
sandbox settings. The direct backend API now has a Docker proof path on macOS
and Linux. This is S6 implementation work, not completion of vendor certification
or S7 governed-mission acceptance. Native Seatbelt, bubblewrap, Windows and
non-Docker container runtimes remain refused by this path.

## Boundary

The caller supplies a resolved Docker sandbox and an absolute **guest** command.
The installed Linux image must be immutable (`repository@sha256:…` or an image
ID `sha256:…`), declare no anonymous volumes, and provide the trusted interpreter
`/usr/local/bin/python3`. The interpreter and image are part of the trusted
boundary: a digest identifies bytes; it does not certify arbitrary image contents.
There is no automatic image installation or vendor credential selection.

The wrapper reuses the existing container filesystem and network policy,
including authority masks, protected Git metadata, private scratch HOME and the
filtered egress relay. It verifies host mount sharing before creating the worker.
The cleared worker environment contains a guest PATH/locale, explicit session
variables and the private HOME/TMPDIR. The Docker control environment is separate
and never becomes the worker environment. Native provider homes and the macOS
Keychain are not mounted or queried.

An engine-owned control directory, outside all worker-writable roots, is mounted
read-only. It contains the fixed supervisor, launch data, a lease and a recovery
ledger. The host updates the lease every 500 ms. The guest requires a renewal
observed after startup before launching the peer, and stops after five seconds
without an observed renewal. Missing/unreadable lease data never extends that
deadline. Host suspension or shared-filesystem trouble can therefore stop a
healthy session; it cannot silently extend authority.

The supervisor runs as guest PID 1, disables dumpability before launching the
peer, and forwards bounded chunks through separate I/O threads. A peer blocking
stdin or output cannot block lease checks. When PID 1 exits, Linux kills the
namespace's remaining processes, including detached sessions and nested children.
The dumpability restriction prevents a same-uid peer from changing supervisor
memory or using its `/proc` handles. See the Linux documentation for
[PID namespace termination and signals](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html)
and [dumpability](https://man7.org/linux/man-pages/man2/PR_SET_DUMPABLE.2const.html).

## Cleanup and receipts

Normal completion, cancellation and dropped futures also request daemon removal.
A random immutable owner label identifies the container; cleanup deletes the
inspected container ID, never an arbitrary caller-supplied name. A successful
daemon inventory must confirm absence. A name collision cannot confer ownership.
Unconfirmed creation or cleanup retains the private ledger and fails visibly;
it does not become a successful run. The ledger includes image, supervisor hash,
owner identity and container name, without recording host control credentials.

Engine `SIGKILL` cannot run a Rust destructor. Lease expiry stops the worker in
that case, while the private ledger remains as recovery evidence. This does not
claim immediate removal of every daemon resource: the existing filtered-egress
relay/network/credential-volume recovery remains owned by `container_egress`,
and a failed or unreachable daemon leaves cleanup unconfirmed. A stopped guest
and a confirmed clean daemon are distinct facts.

The engine-generated ACP initialization receipt records the image, supervisor
hash and lease posture separately from the peer's capabilities/model report.
`providerCompatibilityCertified: false` is deliberate until the corresponding
live adapter/runtime/image pass has been reviewed.

## Proofs and next acceptance boundary

`acp_containment_v1` tests use the real ACP backend and a deterministic Python
peer. Required Docker tests cover:

- normal completion and an actual feature commit in an isolated worktree;
- protected token, audit, Git and outside-workspace I/O, including shell and
  nested detached children;
- read-only lease and supervisor-memory attacks;
- cancellation, Drop and engine `SIGKILL` with detached children, ignored stdin,
  a full input pipe and an attempted `SIGSTOP` against the supervisor;
- delayed daemon startup after the host owner has died;
- failed cleanup retaining evidence and colliding names preserving unrelated
  containers;
- allowed proxy traffic, recorded denied traffic and failed direct egress.

Run the proof with an installed pinned fixture image and a working Docker daemon:

```sh
KRANZ_ACP_CONTAINER_TESTS=1 cargo test --workspace acp_containment_v1 -- --nocapture
```

On a Mac whose VM shares only the home directory, set `KRANZ_SCRATCH_ROOT` to
an existing, shared, private scratch base. The fixtures and control directories
are created beneath it. CI requires each real proof by name and rejects the
explicit skip marker; a green default suite alone does not establish containment.

The same tests can be run against a prepared vendor image by setting
`KRANZ_ACP_PROOF_IMAGE` to its immutable image ID. This changes only the test
fixture's image, including the separate owner process used for SIGKILL tests.
It does not invoke a vendor adapter.

## Prepared vendor-image probe

The first Linux ARM64 candidate has a recorded
[build recipe](compatibility/acp/container-arm64.Dockerfile) and
[image/version receipt](compatibility/acp/container-arm64-image.json).
It contains Node 22.23.1, Claude ACP 0.77.0 / Agent SDK 0.3.270, and
Codex ACP 1.11.0 / Codex 0.153.4. The eight daemon proofs pass on this image
on a macOS ARM64 host with Colima. Those fixtures alone do not establish
authentication, vendor tool behavior, Linux-host certification or production
readiness.

To reproduce the build, use a fresh context containing the recipe as
`Dockerfile`, the named Node archive, `live-install-lock.json` copied as
`package-lock.json`, and a `package.json` containing that lock's `packages[""]`
object. Verify the Node archive and lock against the receipt's SHA-256 values
before building. Use an empty Docker auth configuration, no build secrets and
no host npm configuration. The recipe installs only locked packages and disables
lifecycle scripts. Each local build records its own image ID; do not substitute
the mutable local tag in a probe configuration.

The existing `acp_compat_probe` now accepts an optional `container` object:

```json
{
  "image": "sha256:<installed-and-proven-image-id>",
  "egress": ["chatgpt.com:443", "auth.openai.com:443", "api.openai.com:443"]
}
```

In container mode, `program` names an absolute guest executable, such as
`/opt/acp/node_modules/.bin/codex-acp`. The proxy uses the existing effective
allowlist, which also includes Kranz's default Anthropic endpoint floor.
An empty list gives a deterministic fixture no network. A denied connection
fails the probe; it never broadens the allowlist automatically.

`--check` starts no adapter and reads no credential values. `--run` makes one
fixed report-only prompt, rejects tools and workspace changes, allows 120 seconds
for the prompt / 180 seconds overall, writes a new redacted receipt and never
retries. These time and call bounds are not a hard dollar cap. A provider pass
still does not certify the full containment or mission acceptance contract.

Minimal native Codex login-file seeding remains available with explicit
`nativeLoginHome`. Claude needs an explicit API/OAuth environment token or an
existing file credential; container mode refuses `allowKeychain`. The native
home and Keychain are never mounted. Live provider runs require the bounded
workload and credential authorization described in the
[approved scoping document](scoping/acp-worker-gate-contract.md).

The operator authorized one contained Codex prompt using a private copy of the
existing login. The [redacted receipt](compatibility/acp/codex-acp-contained-denied-egress.jsonl)
records successful authentication, an `end_turn` report and no tool events.
An offline comparison confirms the exact fixed report. The overall probe
**failed**: five connection attempts to
`sdmntprsouthcentralus.oaiusercontent.com:443` were denied. The probe confirmed
relay cleanup, and a separate daemon inventory confirmed the worker was absent.
No retry or allowlist expansion followed. The export replaces the authentication
account and its label; it preserves the failed terminal result. The additional
endpoint needs a policy decision before another live attempt. Claude still needs
a Linux-compatible credential and its own authorized probe.

For a provider-free end-to-end check of the probe itself:

```sh
cargo build --workspace --example acp_compat_probe
python3 scripts/check-acp-probe.py /absolute/target/debug/examples/acp_compat_probe sha256:<image-id>
```

Remaining S6 work is pinned vendor-image/runtime/authentication proof and the
corresponding narrow configuration admission. S7 then exercises the full mission,
one-call consent, independent defect detection/repair, exact-tree merge and
portable audit export. These fixtures do not authorize provider calls, certify
arbitrary images, enable client filesystem/terminal RPCs or change defaults.
