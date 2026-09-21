# Independent review of PR #69

Three fresh review agents independently reviewed head
`7c6d34fc1a3ca4948f135645acdf2138c1daa81f` against base
`bbd7d6dd45b28a730a2d84a03e6ba46c1a8e1e70`, without the implementation
transcript. Their scopes were containment/cleanup, worker admission/credentials,
and gate/runner integration. The parent separately checked the acceptance
fixture, evidence projections and acceptance criteria. Reviewers were read-only
and made no provider calls or credential, Keychain or Docker accesses.

## Finding and correction

One introduced P2 finding survived verification: the container's Python PID 1
only waited for the ACP peer. Orphaned background-tool descendants therefore
remained zombies until the session ended, consuming the namespace's 512-task
limit across otherwise sequential commands. Docker init is deliberately off,
so no other process could reap them.

The parent reproduced this through the real ACP backend and Docker boundary.
The new regression failed against the original supervisor with 16 orphan
zombies still present after three seconds. The supervisor now has one waiter:
each loop reaps at most 64 exited children without blocking and records the
peer's actual exit status in `Popen.returncode`. This avoids a competing
`Popen.poll()` consuming or fabricating status while keeping lease checks bounded.

The regression passes with the correction. Each of two sessions creates 640
orphan descendants in small batches, checks that no zombies remain, and
confirms namespace removal. One peer exits successfully; the other exits 23,
which must remain a failed ACP session. CI requires this test by name and
rejects the containment skip marker. The containment reviewer reread the fix
and regression and found no remaining actionable issue.

No other introduced findings were verified. Five-axis review covered exit
status and cleanup correctness, the single-waiter implementation's readability,
reuse of the existing supervisor boundary, lease/credential/gate authority, and
bounded process/output work. There are no new dependencies or provider calls.

## Evidence and scope

The parent recomputed the corrected runner source hashes and the retained
artifact hashes for all three governed attempts. The failed Claude attempt's
45 postmortem payloads and both passing runs' 85 payloads match their manifests.
The failed export has two unresolved entries; each successful export has one
missing research memo. No credential source was read during this audit.

The live controller and reviewers remain scripted, as the published receipts
state. Those receipts describe their original runner and supervisor bytes;
they are not rewritten as live runs of this correction. The correction is
validated with deterministic real-container proofs, not another paid session.

Independent offline admission, native ACP, permission lifecycle, evaluator I/O
and cleanup/recovery tests passed. Review scope does not establish live model
judgment, new platform support or every possible workload. The gate reviewer
also identified pre-existing parallel Pause behavior: a parallel batch can
checkpoint/judge before draining queued Pause. This was not introduced by this
PR; the new pause regression covers the sequential path. Additional reviewer
coverage did not include live providers or a separate full workspace run.

## Final validation and landing

The full workspace suite passed with 3,064 tests, zero failures and 10 ignored,
with ACP, external-evaluator and mount Docker proofs enabled. Full-workspace
Clippy with warnings denied, formatting and build also passed. The new orphan
regression separately passed in the pinned vendor image, using only the Python
fixture; no provider adapter or real credential was involved.

Engine staged secret scan, Gitleaks, domain lint (1,019 files), actionlint,
local Markdown file links, whitespace and knowledge refresh passed. Knowledge
refresh retains its existing report-only skipped Slack command citation; the
workspace Slack tests passed. Final daemon inventory had no containers, volumes
or private profile homes and only the three default networks. Colima was
restored to its original stopped state. Private evidence and prior recovery
records remain retained.

The first full run failed eight queue tests when the host had about 3 GiB free:
the existing 4 GiB disk-footprint guard parked their synthetic missions before
the test callback. Only this worktree's generated Cargo target was cleaned.
Rebuilding with incremental compilation and debug symbols disabled, plus symbol
stripping, left about 4.5 GiB free; the full rerun passed without changing the
disk guard or queue tests. The failed run remains a failed validation attempt.

S7 remains open until corrected-revision CI passes and the stacked integration
is reviewed for landing. No release, default promotion or additional live
allowance is implied.
