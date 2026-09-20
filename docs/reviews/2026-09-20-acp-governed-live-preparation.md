# Governed ACP live-worker acceptance record

PR #69's `5dc355d` Linux external-evaluator job passed, including the delayed
create and governed failure/repair fixtures. The separate Ubuntu workspace job
failed before a Claude discovery fixture could run: Linux returned `ETXTBSY`
(`Text file busy`) on its freshly written executable. The same test now executes
in an isolated child process with no concurrent fixture writers. The real failed
and hung probes, exact error checks, fallback sentinel and three-second timeout
remain. Production discovery has no retry or fallback change. Concurrent forks
retaining a writable descriptor explain this failure mode; the CI log does not
identify a particular interfering process.

## Prepared workload

The opt-in `acp_governed_probe` example exercises an ordinary mission with one
real ACP worker. Controller and two reviewer sessions are explicitly scripted;
they do not establish independent model judgment. The worker, permission broker,
host checkpoint, command checks, external evaluator lifecycle, local merge and
audit exporter are the production implementations. This is a bounded acceptance
fixture, not a new production controller or a default backend change.

Each provider gets a separate new directory and repository with no remote.
The plan permits only this native shell invocation:

```sh
echo kranz-acp-tool-fixture-v1 > fixture-result.txt
```

The worker must ask for one-time permission, then return the fixed report. The
fixture uses the existing exact-command permission matcher, including only the
already observed pinned Codex shell wrapper. A changed command, extra effect,
repeated request, stale deadline or foreign worktree is refused. The recorded
proposal and actual resolution time also check the retained tool transcript.

The engine tests exact file bytes before acceptance and merge. External fixture
checkers validate evidence hashes and exercise each lifecycle stage; their
synthetic pass is not a code-quality judgment. The merge subject must identify
the actual integration tree. Exported payloads are checked against manifest
hashes. Source changes outside the fixed file and engine mission documents fail.
The primary source/ref must remain unchanged until the explicit local merge.

| Bound | Prepared value |
| --- | --- |
| Claude | ACP 0.77.0 / Agent SDK 0.3.270; `claude-acp-0.77.0-arm64-v1` |
| Codex | ACP 1.11.0 / Codex 0.153.4; `codex-acp-1.11.0-arm64-v1` |
| Image | `sha256:5d0f56837d3b506013d47da6f3294cf90f24b4e4dbaf2079828d75522167b743` |
| External checker image | `python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d`; offline synthetic checker, separate from worker credentials |
| Host | macOS ARM64 with the existing Colima Docker endpoint |
| Credentials | Existing selected Claude OAuth JSON / Codex CLI `auth.json`, copied into private worker homes |
| Provider work | One worker session and one initial ACP prompt per provider; no repair or retry |
| Worker deadline | 120 seconds from feature dispatch, including adapter startup; a silent peer does not get the longer mission budget |
| Mission run | 240 seconds, including permission and validation waits |
| Overall fixture | 480 seconds for setup, run, local merge and export; external process supervisor must cap at 600 seconds |
| Dollar cap | None; time/session bounds do not cap provider billing or underlying HTTP requests |
| Model | Adapter default, recorded when reported; neither model selection nor absent cost is fabricated |
| Network | Existing exact profile allowlist; no expansion or automatic image pull |

`--prepare` reads no credential bytes and starts no adapter. Its manifest pins
the runner executable, full plan, configuration, credential source path and
bounds. `--run` requires that manifest's digest and consumes a durable,
create-exclusive attempt record before reading a credential or starting work.
An interrupted or failed attempt cannot be repeated with the same preparation.
The controller refuses the findings-to-repair conversion call itself, then
refuses any controller reseed. This stops the engine before it can synthesize
repair features from an empty or malformed response; a missing canned reply is
not a sufficient guard. The configuration's minimum one repair-cycle allowance
does not authorize another worker. The event monitor also rejects a second
feature or worker. No live provider runs are part of the normal workspace suite.

To prepare a new reviewed batch:

```sh
cargo build --workspace --example acp_governed_probe
target/debug/examples/acp_governed_probe --prepare codex /absolute/auth.json /absolute/new-codex-batch
target/debug/examples/acp_governed_probe --prepare claude /absolute/claude-credential.json /absolute/new-claude-batch
```

Run only the reviewed digest after operator authorization, under the external
wall-clock supervisor. Use a cleared launcher environment with only the selected
Docker endpoint, executable search path and shared scratch paths. Do not pass
secrets in argv or forward raw provider stderr to a public transcript. An empty
successful daemon inventory before/after each run must confirm cleanup; also
check denial events and private profile-home removal. The runner deliberately
leaves `cleanupRequiresSeparateInventory: true` in its result.

The private evidence bundle can contain account metadata, local paths and
scripted usage fields. Review and project it before publishing. Only worker-role
provider usage is live; scripted controller/reviewer numbers are not billing.
No Keychain, provider config discovery, remote Git push or repository-global
configuration change belongs to this fixture.

## Review and status

Five-axis author review: the discovery change is test isolation only; the new
example reuses the mission, profile, permission, gate and exporter seams. It
does not add production authority or qualify another adapter/platform. Its
single-use preparation and fixed input matcher bound invocation authority;
scripted judgment and unavailable cost remain explicit limitations. The tiny
fixture bounds log polling and snapshot work; it is not a performance benchmark.

Six fixture tests pass without provider calls: preparation mutation and
consumed-slot refusal, the actual worker-report parser, permission replay,
synchronous repair/reseed refusal, and a missing-credential run through real
plan approval. The Docker test additionally checks that the exact-byte command
accepts the intended file and rejects different bytes in the qualified image.

## Live results

The [projected receipts](../compatibility/acp/governed-worker-attempts-v1.json)
retain all three attempts, private artifact hashes and the runner executable digest
for each preparation. Raw transcripts and exports stay private. The current
source hashes describe the corrected runner, not the earlier Claude executable.

Codex passed in 25.451 seconds: one live worker, one delivered permission, the
fixed file and report, host checkpoint, validation, four consumed external gate
stages, exact-tree local merge and an export whose payload hashes were verified
again during projection. Before/after inventory matched with no containers,
volumes or private worker homes; zero denied-egress events were recorded.
Worker cost and provider-selected model were unavailable. The configured role
label in `worker.spawned` is not proof of the provider-selected model. Scripted
controller/reviewer usage is excluded from provider billing claims.

Claude's first governed attempt failed in 19.470 seconds because of two fixture
errors: its prompt requested unsupported report result `complete`, and the
engine command check used `python3` outside the gate PATH. Claude executed the
authorized write and returned what the prompt requested. The resulting partial
report and failed check led to a synthesized repair feature. The monitor stopped
the run at the second feature; that runner's transcript is empty, with no second
Init/prompt recorded. Underlying startup HTTP request counts are unavailable.
This exposed a race in the original monitor-only guard. The typed `pass` report,
absolute `/usr/local/bin/python3` check and synchronous conversion/reseed refusal
above fix the fixture. Cleanup was confirmed, zero denied-egress events occurred,
and the one completed live worker reported $0.0599496. The attempt stays failed;
its event log is preserved without manufacturing a terminal mission event.
A separate postmortem export verified all 45 payload hashes and retained two
unresolved entries; exporting it does not resume or reclassify the mission.

The original allowance authorized one attempt per provider with no retries.
Codex's corrected preparation replaced an unconsumed preparation. The operator
subsequently requested completion of the corrected Claude proof, authorizing
one additional attempt with the previously prepared bounds. Preparation
`d43ae1d32aac8f4ee21149c56ab5acda44841502469abf92becaaa0539b1770e`
and the runner executable were unchanged; no new login or Keychain access was
needed, and the original failed attempt was preserved.

The additional Claude mission `m-74fe75` passed in 19.921 seconds with one live
worker, one feature, one delivered permission, the expected file/report and
four consumed external stages. The merge tree matched the judged integration
tree, and all 85 exported payload hashes were independently recomputed during
projection. The export explicitly retains one unresolved `research.md` entry;
this fixed-plan fixture does not produce a research memo. The completed live
worker reported $0.059583; scripted usage is excluded. No timeout, automatic
retry or denied-egress event occurred. Before/after inventory matched with no
containers, volumes or private worker homes. Colima was restored to stopped
after validation.

Both supported profiles now have a passing bounded governed-worker fixture on
macOS ARM64/Colima. S7 stays open for independent branch review and review of
the combined acceptance evidence. The controller and reviewers remain scripted;
these runs do not establish live independent model judgment or new platform
qualification.

## Final local validation

All four workspace gates were rerun after the additional Claude pass, with
ACP, evaluator and mount Docker proofs enabled for the test suite.

- `cargo test --workspace`: 3,063 passed, zero failed, 10 ignored, with ACP,
  external-evaluator and mount Docker proofs enabled. The six new example tests
  and the isolated discovery regression ran successfully.
- `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check` and `cargo build --workspace` passed.
- Engine staged secret scan, staged Gitleaks, domain lint (1,018 files), local
  Markdown file links and whitespace checks passed. Knowledge refresh verified
  all 10 notes; its existing report-only Slack command citation remains skipped,
  while the workspace Slack tests passed.
- Final runtime inventory contained no containers, volumes or private worker
  homes and only the three default Docker networks. Colima was restored to its
  original stopped state. Private live evidence and
  failed-run recovery records remain retained.

All scheduled CI checks for runner revision `0627086` subsequently passed
([workspace CI](https://github.com/craigcode/kranz/actions/runs/35496472400)),
including Ubuntu, Windows, the wrapped macOS suite and Linux external evaluators
(the conditional smoke job was skipped). The original Ubuntu failure remains
recorded above. This additional live pass changes only evidence and status
documents; the corrected runner is unchanged. Independent branch review and
CI for the evidence commit remain required before landing. No release bump or
default backend change is part of this checkpoint.
