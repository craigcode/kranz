# Governed ACP failure and repair fixtures

This checkpoint continues PR #69 and the S7 acceptance ticket. The workers are
deterministic Python ACP peers in the real Docker boundary; controller and
reviewer responses are scripted. No provider calls, real credentials or Keychain
access are involved. A fresh scripted reviewer is a protocol fixture, not an
independent model's endorsement of this implementation.

## Failures found and fixed

The Linux evaluator job at `e1ec633` repeated the earlier Docker-create timeout.
Creation had a fixed five-second control cap inside an evaluation with a
120-second wall budget. Creation now consumes the remaining evaluation budget;
inspection and cleanup keep their separate five-second caps. There is no retry,
automatic pull, relaxed verdict or extension of the overall acceptance deadline.
CI logs establish the timeout, not the daemon's underlying reason for taking
longer. Cold image setup is one reason creation needs a different budget from a
metadata inspection.

A real-Docker regression delays the create client's return for six seconds.
It fails on the old cap and succeeds within the evaluation budget. Separate
deadline and cancellation cases interrupt a thirty-second delay after Docker
has created the namespace, require rejection, and confirm cleanup.

The contained interruption fixture exposed a second bug. The engine stopped a
worker waiting for permission, closed its request and recorded `mission.paused`,
then the inner feature loop judged the aborted run and spawned another worker.
The sequential feature path now returns to the outer idle loop immediately
after applying Pause. Existing commits remain attributed; dirty work remains
available for explicit resume. No checkpoint or model turn runs while paused.
The test keeps polling after the pause event to catch a continuing retry loop,
rejects a queued late click, and verifies the paused state after restart.

## Acceptance coverage

The required `acp_containment_v1_profile_*` Docker tests now cover:

- Ordinary approval, one-call consent, shell delivery, host checkpoint, fresh
  scripted review, external stages, offline local merge and evidence export.
- A worker that writes `defect` while claiming success. The engine's real
  command check fails, a scripted functional reviewer reports the defect, a fix
  feature runs in a fresh worker session with fresh consent, and validation sees
  the real command pass. The external merge subject must name the tree actually
  merged. This does not claim autonomous defect discovery by a live model.
- Pause during a pending permission, a queued stale approval, and restart without
  restoring a response capability or starting a replacement worker.
- A sealed post-approval pack-policy change that refuses merge before any merge
  command executes or the primary ref moves.
- An external checker that exits unsuccessfully after the worker delivered a
  real commit. The mission blocks; the error is retained and never consumed.
- The existing credential-echo refusal and private-home cleanup.

The repair fixture verifies every exported payload against its manifest hash,
folds the exported events, then removes runtime evidence and reassembles the
bundle. Missing files must become unresolved entries; neither the event log nor
the integrated tree changes during export. Export hashes are integrity checks,
not independently verified human identity or a portable signing authority.

The permission restart test separately covers durable request, decision and sent
receipt checkpoints. Each preserves the recorded facts and deadline, closes the
old capability and rejects another click without appending events. Existing
expiry, foreign-session, lost-response-channel, peer-death and namespace-lease
proofs remain part of the workspace suite. This is a combination of integrated
fixtures and targeted state-transition tests, not a claim of killing a live
provider at every instruction boundary.

## Review and validation

Five-axis author review: the production changes enforce the existing pause and
deadline contracts; no event schema, credential scope or backend admission is
expanded. The runner, permission broker, evaluator, merge and exporter remain
the existing implementations. Creation can wait longer within the existing
evaluation cap, with unchanged cleanup uncertainty handling. Test-only scenario
selection adds no production worker behavior or dependencies.

Local validation passed:

- `cargo test --workspace`: 3,057 passed, zero failed, 10 ignored, with the ACP,
  external-evaluator and mount Docker proofs enabled. All six required profile
  scenarios and the delayed-create regression ran successfully.
- `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check` and `cargo build --workspace` passed.
- Engine secret scan, domain lint (1,015 files), staged gitleaks, actionlint,
  local Markdown links and whitespace checks passed. Knowledge refresh verified
  all 10 notes; its existing report-only Slack command citation remains skipped,
  while the workspace Slack tests passed.

These are local results. Current-revision GitHub CI and independent branch
review remain separate requirements; the earlier failed CI run is not a pass.

Intermediate failures remain recorded: the first interruption fixture exposed
the paused respawn; a later oversized test future overflowed its test-thread
stack and was changed to hold one boxed mission future. A subsequent local run
hit a mount-helper timeout with cleanup initially unconfirmed while host free
space was about 1 GiB. Only this task's generated Cargo target was cleaned;
the owned namespace was confirmed absent and its leftover relay, network and
volume were removed. Final gates use nonincremental builds without debug symbols
to preserve disk headroom. The timeout is not asserted to prove disk pressure
was its cause. After the final suite, no test containers or private profile homes
remained, and this run's scratch worktrees were removed. Colima was restored to
its original stopped state. Recovery records from the interrupted attempt remain
available locally alongside the failed-run logs.

## Remaining S7 boundary

At this checkpoint, live governed missions for the pinned Claude and Codex
profiles still needed a prepared workload, exact call/time budget and fresh
operator authorization.
Earlier approved provider slots are consumed; these tests do not authorize new
spend. Independent branch review and landing the stacked PRs also remain. S7
stays open; no default promotion, release or remote mission push is implied.

The subsequent [live-worker record](2026-09-20-acp-governed-live-preparation.md)
tracks the newly authorized attempts, including the passing Codex mission and
the failed Claude attempt caused by fixture errors. It does not replace the
synthetic defect/repair evidence above or establish live model judgment.
