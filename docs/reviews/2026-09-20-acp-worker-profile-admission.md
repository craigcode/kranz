# Qualified ACP ordinary-worker admission

This change carries the reviewed Claude/Codex image and startup policy into the
ordinary worker runner. It does not change the default backend. The two explicit
profiles are documented in [ACP containment](../acp-containment.md). The live
qualification remains the exact workloads and hosts recorded in
[native-tool-proof-v2.json](../compatibility/acp/native-tool-proof-v2.json) and
[linux-native-tool-proof-v1.json](../compatibility/acp/linux-native-tool-proof-v1.json).
No additional provider call or Keychain access was made for this implementation.

## contractChangeRequest

Authorized scope: the operator's go-ahead to integrate qualified ACP workers
following the bounded native Linux checks. `RoleConfig` gains optional
`acpProfile: { id, credentialFile }`, appended after existing fields, with
`serde(default)` and omission when absent. Existing configs, mission-created
records and snapshots retain their serialized shape without it. No new event
kind or state transition is introduced. Profile data is carried by the existing
mission policy digest and immutable configuration rules. Repository config and
runtime patches cannot select or replace a profile or credential source.

The generic ACP enforcement capability remains false. Validation admits only
worker + worktree + a known profile + the exact image, fs+net, configured egress
and no extra writable root. Runtime checks repeat the actual boundary before
opening the selected file. Windows, x86_64 production workers, ACP validators,
custom profile commands, resume and dispatch-pool ACP remain refused. A profile
cannot be combined with any dispatch-pool candidates, including native Claude
candidates that would otherwise inherit its worker settings.

## Integration and review

The worker uses the ordinary runner and durable permission broker. The profile
selects guest argv, reads one bounded private file, and owns a disposable HOME.
The writable HOME sits inside an unmounted engine-owned private parent,
preventing guest chmod from widening host-user access. The Docker peer fixture
tries that attack before initialization.
Codex receives only the selected OAuth auth file and fixed file-auth/plugin-off
settings. Claude receives only the selected OAuth token and the traffic-control
flag. Neither falls back to ambient credentials, a native configuration directory
or Keychain. Completion and abort confirm namespace cleanup and remove the home;
Drop is a fallback. An abrupt engine death stops execution through the existing
lease but can leave private disk state for recovery.

The full mission fixture exposed two integration requirements beyond direct
probe success. The mount preflight must use the configured image, with missing
pinned images refused without pulling. Engine-run contract/merge commands
need a stricter network policy: they keep the qualified image and filesystem
wrap but run offline, without the worker's selected provider credential. Mac
mission worktrees and gate scratch must also be on Docker-shared paths; setting
only the ACP scratch root does not move ordinary mission worktrees.

A concurrent full-suite run also caught fixture interference: a fake-runtime
test temporarily replaced PATH while the ordinary mission reached its merge
checker. The new Docker fixtures now hold the existing environment-test lock
through cleanup; the production gate still fails closed on invalid inspection.

Five-axis author review:

- Correctness: legacy configuration remains unchanged; profile admission and
  runtime refusal are separately checked. A real fixture mission must produce
  a nonempty feature commit through the existing host checkpoint. No alternate
  commit or approval path was added.
- Readability: the profile owns version pins, startup and credential handling
  in one module. Readiness reports authentication as unprobed instead of asking
  for a host-side adapter command that the profile deliberately omits.
- Architecture: existing worktrees, sandbox masks, egress relay, permission
  broker, checkpoint, evaluator and evidence-export machinery remain in use.
  Profile-based gate policy is shared by approval, validation, server merge
  and the external merge checker's retained command receipts.
- Security: private source permissions, owner, link count, file size, unique JSON
  keys and root separation are checked before spawning. Unexpected caller env
  and boundary drift fail closed. Known whole credential echoes are refused or
  redacted before retention; transformed/split secrets are not claimed covered.
  Normal cleanup failure remains a failure with a private recovery-path log.
- Performance: no dependency, discovery agent or provider preflight was added.
  Each worker allocates one private home and copies at most 48,000 credential
  bytes; existing bounded container setup and cleanup remain authoritative.

This is an author self-review, not an independent review verdict. The synthetic
reviewer is scripted test evidence, not a model's independent endorsement of
this implementation.

## Validation and remaining acceptance

Final local gates passed on macOS ARM64:

- `cargo test --workspace`: 3,052 passed, zero failed, 10 ignored. ACP, gate and
  mount Docker opt-ins were enabled; both new profile proofs ran successfully.
  `RUST_TEST_THREADS=4` and VM-shared mission/scratch paths were used.
- `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check` and `cargo build --workspace`: passed.
- Engine secret scan and domain lint: 26 changed files, no findings or skips.
  Staged gitleaks found no leaks. Workflow actionlint, changed-document local
  links and whitespace checks passed.
- Cleanup: no containers, volumes, extra Docker networks or private profile
  homes remained. Colima was restored to its original stopped state.

The full-suite failure from test environment interference was followed by a
passing isolated mission and the final passing workspace run after the fixture
lock fix. No production timeout or refusal was weakened to obtain a pass.

The preceding `ff56bd6` [Linux evaluator CI job](https://github.com/craigcode/kranz/actions/runs/35480155086/job/105996400246)
failed when Docker creation exceeded its bounded control deadline. That failure
remains retained; local results do not reclassify it or stand in for this new
revision's CI.
The test-only Python profile is absent from production builds and uses dummy
credentials. CI requires the ordinary-mission and credential-echo Docker proofs
by exact test name; skipping Docker cannot count as a containment pass.

The ordinary-mission fixture connects approved scope, one-call human-capability
consent, a contained shell write, host checkpoint, fresh scripted review,
contained external evaluators, an offline exact-tree local merge gate and
portable evidence export. The primary stays unchanged until the explicit local
merge; there is no push. The separate echo case refuses the credential-bearing
frame and verifies private-home removal.

S7 remains open: connect a seeded defect and repair to this same integrated
flow, assert interruption/policy-drift cases in that flow, review the portable
record, and prepare a separately authorized bounded live mission for each
supported adapter. The earlier live shell proofs do not certify that complete
workflow. External command-permission evaluators remain deliberately refused;
the existing live human-consent broker is the authoritative permission path.
The container profile also retains the existing refusal for process-only
negative-control snapshots. No default promotion, merge of PR #69 or release
is implied by this implementation review.
