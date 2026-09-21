# External evaluators: explicit execution API

The [gate v1 contract](gate-evaluation-contract.md) has a contained engine
library API and mission-stage adapters. The checker API returns evidence;
the adapters retain consent, enforcement and consumption authority. Tracked,
repo-relative packs can register approval, milestone, final and merge evaluators
on macOS/Linux. External command-permission evaluators remain unavailable; the
live one-call permission broker owns that stage.
Existing schema-2/3/4 command gates keep their existing order and advisory posture.

## Registration and trusted bytes

Schema 5 adds `[[evaluator]]` alongside the existing pack sections:

```toml
[pack]
name = "example-checks"
schema = 5

[[evaluator]]
name = "check-change"
image = "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d"
executable = "/usr/local/bin/python3"
args = ["-I", "-S", "/checker/check.py"]
files = ["check.py", "rules.json"]
stages = ["milestone-validation", "final-gate"]
evidence = ["scope", "diff", "check-receipt"]
kind = "mechanical"
enforcement = "blocking"
```

The image is a pinned, multi-platform synthetic Python runtime used in the test
suite, not a recommendation to trust arbitrary Python checkers. A host must
approve its own checker, runtime and dependencies. `kind` is `mechanical` or
`judgment`; `enforcement` is `advisory` or `blocking`. These are trusted registration
facts, not fields the result may choose. `stages` names the supported v1 stages;
`evidence` names additional manifest roles beyond the stage's mandatory inputs.

`PinnedRegistration::at_ref` reads the manifest and declared files from regular
Git blobs at the host's approved ref. It retains a registration object containing
the resolved commit, manifest hash, complete declaration, file hashes and executable
modes. Dirty worker edits do not replace these bytes. The image digest covers the
runtime and image-installed dependencies; `files` covers additional scripts/data.
There is no shell command interpolation, mutable image tag, automatic image pull
or dependency download. Missing or unsupported declarations fail at load.

The caller must establish which ref is approved. Passing an arbitrary worker ref
into this host API does not make that ref trusted. This is the same responsibility
as selecting the pinned base for the existing standards loader. A library caller
is trusted engine code; a subprocess cannot call this Rust API.

## Input and process boundaries

`FrozenEvidence::new` takes owned byte buffers, not paths to read from a worker
manifest. It checks the complete inventory, hashes, lengths, provenance and the
stage's subject/plan/policy/registration bindings. A required authority role must
occur exactly once with engine provenance. Declared additional roles must exist.
Findings bind to supplied artifact hashes and real UTF-8 line bounds.

The caller selects the minimal evidence and establishes its provenance. The
mission-stage adapters assemble source inventories and real command receipts
from engine-observed state; the low-level API cannot establish provenance from
a worker's claims. The snapshot inventory API represents regular-file bytes, executable bits
and deletions; symlinks, submodules and unsupported path encodings fail readiness.
Input bytes are stored by artifact label. A checker that needs a build tree must
materialize the selected files and modes in `build/` itself.

`DockerEvaluator` uses an absolute, trusted Docker CLI and freezes the existing
container client's host environment. The CLI may need Docker context credentials;
none of that environment enters the checker. The container starts with an empty
environment and only fixed HOME, TMPDIR and PATH values. The bootstrap shell
clears the environment again before executing the checker.

Each evaluation gets a fresh mode-0700 directory under a host-chosen, daemon-shared
parent. Docker mounts only these newly created roots:

| Root | Access |
|---|---|
| `/gate/inputs` | Read-only minimal evidence |
| `/checker`, `/driver` | Read-only approved checker and engine bootstrap |
| `/gate/outputs`, `/gate/build`, `/gate/home` | Writable scratch |
| Image filesystem | Read-only trusted runtime |

Source selection excludes conventional private paths at any directory depth
(case-insensitive and directory-bounded), including nested `.npmrc`, provider
homes and private `.kranz` runtime paths. Files containing a PEM private-key
BEGIN line are excluded in full; public certificate PEM files remain eligible.
The selection receipt names excluded paths and binds this policy. This is a
conventional credential exclusion policy, not a complete arbitrary-secret scan.

The real checkout, shared Git directory, provider login state, host home and
Docker socket are not mounted. Network is disabled, Linux capabilities are
dropped, privilege escalation is disabled, and PID/memory/CPU/per-file-size
limits apply. Image-declared volumes are refused. Standard container devices
remain available. Import caps and per-file limits are not an aggregate host disk
quota; a deployment that needs a hard disk budget must provide one for the attempt
parent. Only the Docker backend is implemented here. The image must provide
`/bin/sh`, `cat`, and `/usr/bin/env` for the fixed engine bootstrap.

Random sentinels prove that the daemon reads the actual input/checker roots and
writes back to the host's output/build/home roots. A path existing on the host is
not sufficient: macOS Docker VMs often do not share `/private/var` temporary paths.
No mount proof means no accepted result.

## Completion, cancellation and retained evidence

The asynchronous driver sends one compact NDJSON request and closes stdin. It
bounds the write, both output streams, frame size and wall time, then requires
one correlated terminal response, exit zero, an exited container and confirmed
container removal. `judged/fail` is an accepted finding of failure; `escalate` has
no verdict; an RPC/process/protocol error is an unsuccessful attempt. None of
these alone is a stage disposition or human approval.

Container creation consumes the evaluation's remaining wall-time budget;
inspection and cleanup each keep a five-second control limit. A slow create
does not receive a fresh evaluation deadline or an automatic retry.

Mission-stage evaluators share an absolute deadline fixed before the first check:
two minutes per applicable checker plus three minutes for setup, cleanup and the
final subject/authority recheck. Each checker still has a two-minute execution
cap. Earlier decisions remain consumable while later checks run, but expiry,
changed subjects and missing human authority still fail closed.

Duplicate or missing responses, trailing output, forged authority fields,
nonzero exits after pass JSON, overflow, expiry and cancellation cannot pass.
The host CLI's process group is killed before reaping its leader. Container
removal separately kills checker descendants, including children that create a
new session. Dropping the caller future starts a bounded cleanup worker.

A daemon outage or interrupted create can make cleanup uncertain. Such attempts
never yield an accepted result and preserve a private `container.json` recovery
ledger. A killed host process cannot run its Drop handler. Mission replay closes
unconsumed records without rerunning checks or effects; it does not sweep private
recovery directories or certify daemon cleanup. Inspect the retained ledger and
resolve container ownership before retrying an interrupted live attempt.

Output files are opened component-by-component without following links. Import
rejects hard links, FIFOs/devices, missing files, byte/count overflows and incorrect
hashes. This version accepts UTF-8 output artifacts only; binary outputs fail
because their redaction is not implemented. The importer scrubs the bytes and
records separate raw/retained hashes plus the scrub implementation's source hash.
Finding IDs or output paths that themselves match a secret pattern are refused.
Scrubbing is pattern-based; it cannot promise removal of every sensitive value.

With `retain_private_inputs: false`, the private attempt keeps a scrubbed
`receipt.json`; input/checker/scratch data is removed after confirmed cleanup.
With explicit private retention, raw inputs, output files and captured stdout
remain under the private attempt directory. A raw stdout digest without retained
raw stdout is identified as such; it is not a claim that the raw bytes remain
available. Cleanup uncertainty preserves files for recovery regardless of the
requested retention policy.

Mission-stage adapters additionally keep scrubbed inputs/results under the
mission's `runs/gates/<attempt>/` directory and log their retained hashes. After
confirmed cleanup they remove their private temporary parent; interruption or
uncertain cleanup preserves it under `~/.cache/kranz/evaluator-attempts/`.

## Validation and current availability

The synthetic checker is in
`crates/engine/tests/fixtures/gate-evaluator/checker.py`. It invokes no model and
needs no provider credentials. Portable tests cover the schemas, byte binding,
Git pinning and refusal to skip configured evaluators. The Docker tests cover
positive results, denial of host access, malformed responses, artifact attacks,
normal descendant cleanup, cancellation, timeout and dropped caller futures.
Stage proofs cover revised-plan consent, a real synthetic mission and local merge,
blocking failures, interruption, configuration drift and stale merge evidence.

```sh
docker pull python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d
KRANZ_GATE_CONTAINER_TESTS=1 cargo test --workspace gate_subprocess_v1 -- --nocapture
```

The dedicated Linux CI job sets the opt-in and installs the exact image; missing
Docker/image support fails that job. Ordinary workspace runs print an explicit
skip marker when the opt-in is absent. The new runtime API compiles only on
macOS/Linux. Windows remains unavailable for this capability; existing Windows
CLI and gate behavior is unchanged. Local macOS proof uses Docker through Colima,
not a native Seatbelt evaluator. Linux certification requires its dedicated CI
proof to pass before this slice is marked done.
