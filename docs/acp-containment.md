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
daemon inventory must confirm absence. If Docker's automatic deletion is still
pending, cleanup polls within a five-second confirmation budget; a removal
acknowledgement alone is insufficient. A name collision cannot confer ownership.
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

## Mount preflight ownership

The bind-mount sentinel helper has separate ownership from the ACP worker.
Its private recovery intent is written before Docker creation, and its random
owner label identifies the full container ID used for removal. The host command
has the existing 90-second proof deadline; cleanup has a separate 15-second
budget, including up to five seconds for confirmed daemon absence. A timed-out
or cancelled proof remains failed even when cleanup succeeds.

The helper runs a fixed shell script as PID 1 in a private Linux namespace,
with no network, no capabilities and a read-only image. Only its fresh sentinel
directory is writable. A separate 85-second guest watchdog interrupts PID 1
without reading the mount being tested, bounding the sentinel script even after
engine death or blocked guest filesystem I/O. The inspected image ID is frozen
before creation; the helper overrides the image entrypoint. The ordinary
preflight still pulls a missing configured image within its deadline; the ACP
worker's separate requirement for an already installed pinned image is unchanged.

A crash between create and start can leave a stopped container. An interrupted
create can also complete late. These cases retain the private
`kranz-mount-owner-*/owner.json` intent and probe directory; an empty early
inventory does not prove a cancelled create will never appear. A failed inventory
or uncertain removal never grants admission. Recovery uses the original Docker
endpoint, the recorded owner label and inspected full IDs, followed by a successful
empty inventory. Never remove by name or use a broad prune. The ledger contains
no provider credentials and is not mounted in the guest. After engine death it
remains even if the guest watchdog and Docker `--rm` have removed the helper.

This helper ownership path is supported for Docker on macOS/Linux. Calls that
require a mount proof refuse Podman, nerdctl, Apple Container and unsupported
hosts before creating a probe. Ordinary Linux paths that rely on the existing CI
mount contract rather than runtime preflight are unchanged.

Run the deterministic and real Docker preflight proofs with:

```sh
KRANZ_MOUNT_CONTAINER_TESTS=1 cargo test --workspace mount_helper_v1 -- --nocapture
```

The CI proof job requires every real test by name and rejects its skip marker.
See the [mount-helper review](reviews/2026-09-19-mount-helper-cleanup.md) for the
retained test results and limitations.

## Proofs and next acceptance boundary

`acp_containment_v1` tests use the real ACP backend and a deterministic Python
peer. Required Docker tests cover:

- normal completion and an actual feature commit in an isolated worktree;
- protected token, audit, Git and outside-workspace I/O, including shell and
  nested detached children;
- a detached stdio MCP tool reached through initialize, initialized, tools/list
  and tools/call, exercising those same thirteen denials without ACP callbacks;
  positive controls require the tool to write its private HOME and the actual
  workspace deliverable, with Docker control variables absent;
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

The MCP fixture pins the [2025-03-26 lifecycle](https://modelcontextprotocol.io/specification/2025-03-26/basic/lifecycle)
and [tool-call envelope](https://modelcontextprotocol.io/specification/2025-03-26/server/tools).
It proves a descendant boundary, not vendor MCP integration or general MCP
protocol conformance. The engine commits the actual tool-written file and checks
that the primary checkout and protected base ref stayed unchanged.

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
That invocation made no retry or allowlist expansion. The export replaces the authentication
account and its label; it preserves the failed terminal result. The additional
endpoint remains blocked. The [pinned-source investigation](reviews/2026-09-19-codex-contained-egress.md)
identifies automatic account-plugin downloads as a plausible cause and verifies
the plugin-disable configuration offline. Exact request attribution
remains unproven. The probe now writes that configuration into its private
startup home before launching Codex; synthetic key/login peers verify it before
ACP initialization in native and contained runs.

After the operator's new go-ahead, one further contained Codex run used the same
image, login channel, prompt, time bounds and allowlist with that startup policy.
It **passed** in 8.5 seconds: exact report, no tool events, zero denied
connections, unchanged workspace and confirmed cleanup. The peer reported
`gpt-6-astra`. The [redacted receipt](compatibility/acp/codex-acp-contained-plugins-disabled.jsonl)
and [proof summary](compatibility/acp/codex-contained-plugins-disabled-proof.json)
retain the result, source-receipt digest and separate owner-label absence check.
This supports the plugin-startup hypothesis but does not attribute the first
run's five requests. The original failure remains a failure.

One newly authorized Claude run then used an operator-provisioned OAuth token
with the same image and the configured Anthropic/Claude endpoint allowlist.
It **passed** in 5.2 seconds: exact report, no tools, zero denied connections,
unchanged workspace and confirmed cleanup. A separate owner-label query and
final daemon inventory confirmed absence. No Keychain access or retry occurred.
The [Claude proof summary](compatibility/acp/claude-contained-oauth-proof.json)
links the receipt with account-limit and command-inventory metadata redacted.
Its initial `Not logged in` notification is retained alongside the successful
request, and usage names both Sonnet 5 and Haiku 4.5. See the
[compatibility notes](acp-compatibility.md#contained-claude-follow-up-2026-09-19-utc)
for authentication telemetry and prompt-budget limits.

For a provider-free end-to-end check of the probe itself:

```sh
cargo build --workspace --example acp_compat_probe
python3 scripts/check-acp-probe.py /absolute/target/debug/examples/acp_compat_probe sha256:<image-id>
```

The [MCP descendant follow-up](reviews/2026-09-19-acp-mcp-descendant.md) adds a
synthetic tool-boundary proof and retains the later Linux gate-startup failure.
The [second native shell batch](compatibility/acp/native-tool-proof-v2.json)
passed for pinned Claude and Codex on macOS ARM64 / Colima: one grant, successful
native tool completion, exact file bytes and one host checkpoint per provider,
with unchanged primary state, zero denied egress and confirmed cleanup. See the
[review](reviews/2026-09-19-acp-native-tool-pass.md). The first batch's failures
remain retained; a subsequent pass does not rewrite them or certify all tools.

Remaining S6 work is:

1. Retain equivalent adapter/runtime/image/platform receipts on native Linux
   hosts before admitting that combination. Linux synthetic CI proves the
   wrapper boundary; it does not launch or qualify the vendor adapters.
2. Wire only the qualified combinations through ordinary worker configuration,
   preserving pre-spawn refusal for every unsupported pair and current defaults.
   The direct-backend fixture does not by itself prove normal mission dispatch.

The [native Linux preparation review](reviews/2026-09-19-acp-linux-preparation.md)
records a Linux ARM64 engine build and passing provider-free fixtures, the
recovered physical-disk exhaustion during a later test build, and the concrete
credential/startup/admission work still required. Its live batch is prepared but
not authorized or run; it does not close either item above.

Mount-helper cleanup has its own proof and review above; it does not close S6.
The observed synthetic cleanup flake and subsequently reproduced recovery and
deletion races are recorded in the [concurrent cleanup review](reviews/2026-09-19-acp-concurrent-cleanup.md).
The historical failure remains retained; the race fixes do not establish broader
provider qualification. S7 then exercises the full mission,
one-call consent, independent defect detection/repair, exact-tree merge and
portable audit export. These fixtures do not authorize provider calls, certify
arbitrary images, enable client filesystem/terminal RPCs or change defaults.
