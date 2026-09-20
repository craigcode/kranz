# v0.3.0 ACP/gate integration and release preparation

This candidate integrates the reviewed work from PRs
[#60](https://github.com/craigcode/kranz/pull/60),
[#61](https://github.com/craigcode/kranz/pull/61),
[#62](https://github.com/craigcode/kranz/pull/62),
[#63](https://github.com/craigcode/kranz/pull/63) and
[#69](https://github.com/craigcode/kranz/pull/69), preserving their commits and
review history. A single release PR targets main so the complete tree passes
its required checks together. Main requires current-base checks and merge
commits. No rule is disabled or bypassed.

The S3–S7 lifecycle closures in this changeset take effect on main with that
integration. They describe the bounded acceptance below, not universal provider
certification. A GitHub tag, public archives and registry publication are
separate release operations governed by [releasing](../releasing.md).

## Integration corrections

The previous Windows CI failed the contract-lint overall-budget fixture: its
second command ran instead of being skipped. The fixture used an ICMP timeout
as a delay, which can return early when networking rejects it. The correction
uses the stock noninteractive PowerShell timer without a user profile or inner
command quotes. Positive-control assertions require the first command to
succeed and consume the intended delay before checking that the rest is skipped.
A fresh reviewer checked the Windows argument handling before commit.

The earlier evaluator slice also retained a five-second Docker-create cap.
The new [PR #62 Linux failure](https://github.com/craigcode/kranz/actions/runs/35543954391/job/106166453415)
reproduced that limit. The already-reviewed correction from PR #69 was moved
into PR #61 with its supporting helper, cleanup result field and regression.
Creation consumes the existing evaluation deadline; inspection and cleanup
retain five-second control limits. There is no retry or deadline extension.
The test proves delayed creation, deadline expiry and cancellation cleanup. CI
runs these lifecycle fixtures serially, requires all eight integration tests
and rejects their skip marker. Fresh source review approved the complete backport.

Merging current main required only knowledge-note freshness conflict resolution.
Propagating the evaluator correction preserved the complete top-of-stack tree
`fca464ab53e015b09e9dd2b02f7120e28d437ff9` byte-for-byte. The versioned candidate
at `6a3091b` and its subsequent ancestry-only integration at `b195b1c` likewise
share tree `de28cd3d884969955c769ade205f69c262ff7e90`. Later changes in this
release PR close tickets and correct documentation; they add no runtime behavior.
Superseded CI runs were cancelled, not reclassified as passes. The final PR must
pass its own required checks and the Linux evaluator/containment job before merge.

## Acceptance and review

The [independent implementation review](2026-09-20-acp-independent-review.md)
covered containment, worker admission and gate integration. Its orphan-reaping
finding is fixed and independently re-reviewed. The integrated
[defect/repair fixtures](2026-09-20-acp-governed-fixtures.md) prove nonempty delivery,
fresh one-call consent, interruption, stale decisions, policy drift, checker
failure, exact integration-tree judgment and portable export. Missing artifacts
remain explicit; replay executes no effects.

The [live-worker record](2026-09-20-acp-governed-live-preparation.md) retains passing
Claude and Codex missions and the failed first Claude fixture. Controllers and
reviewers were scripted. These runs establish the stated live worker workload,
not live independent model judgment. Each passing export retains its unresolved
research memo. Earlier failed attempts and their original source hashes remain
historical evidence. Integration preparation makes no provider calls and reads
no provider credential or Keychain data.

The admitted worker tuple remains pinned Linux ARM64 Docker profiles on
macOS/Linux ARM64 hosts, subject to mount and ownership checks. Other worker
platforms, arbitrary images, native ACP containment, ACP validators, fs/terminal
RPCs, session resume and default promotion remain outside this acceptance.
External command-permission evaluators remain refused; the one-call broker owns
that stage. See [qualified profiles](../acp-containment.md) and
[external evaluators](../external-evaluators.md) for operational limits.

Five-axis integration review checked preserved source trees and authority
behavior, clear version/lockfile changes, reuse of existing seams, retained
credential/containment boundaries and bounded process work. No new dependency,
execution pool or prompt-management feature was added. A fresh reviewer approved
the final documentation and proposed ticket closures, checked both preserved
source trees, and found no actionable inaccuracies; that review did not rerun
the parent's runtime or registry checks.

## Local release evidence

- Integrated stack and versioned candidate: each full workspace suite passed
  3,064 tests, zero failures and 10 ignored, with ACP, external-evaluator and
  mount Docker proofs enabled. Full-workspace Clippy with warnings denied,
  formatting and locked builds passed.
- The earlier evaluator branch's backport independently passed its full
  workspace suite: 2,966 tests, zero failures and 10 ignored, with external
  evaluator Docker proofs enabled. Clippy, formatting and locked build passed.
- Dashboard: clean install, type checking, 247 tests, production build,
  embedded-bundle sync/freshness and lint passed. Lint retains existing React
  effect warnings. The standalone Tauri check and repeated locked check passed.
- Release workspace check and repeated locked check, strict API documentation,
  dependency-policy audit and regenerated package/license notices passed.
  Both lockfiles change only Kranz package versions.
- All four package inventories were inspected for runtime state, credentials,
  generated caches and oversized fixtures. `cargo publish --workspace --dry-run
  --locked` verified every package and explicitly aborted all uploads.
- The optimized macOS ARM64 archive was unpacked outside the checkout; native
  architecture, `kranz 0.3.0`, help, embedded licenses and all six archive members
  were verified. This local artifact does not replace the five-platform GitHub
  rehearsal, archive provenance checks or a later tagged build.
- Anonymous public cloning and fresh-clone audits passed across all branches,
  tags and PR head/merge refs. Gitleaks found no leaks in 1,367 reachable commits;
  domain lint passed across 1,023 files before this documentation update. The
  existing optional no-additional-vocabulary policy is unchanged.
- Final documentation checks passed: domain lint covered 1,024 files, all 69
  changed local Markdown links resolved and knowledge refresh passed. Docker
  inventory had no containers, volumes or private profile homes and only default
  networks; Colima was restored to its original stopped state.
- The four crates.io names remain owned by `craigcode`, with 0.2.3 current;
  0.3.0 and its Git tag were unused at preparation. Public-release enablement
  and the owner-approved GitHub release environment remain configured.

The release workflow and native archive smoke matrix must run after integration
on main before a tag is published. This record is preparation evidence, not a
claim that v0.3.0 has been published.
