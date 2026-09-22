# Post-v0.4.0 maintenance review

The public baseline is v0.4.0, commit
`fccdef95334f82f4595005407e2ba7caeacc824c`. This maintenance batch reconciles
released work with ticket state and integrates the four outstanding dependency
updates. It does not change the v0.4.0 tag or claim a completed pilot.

## Ticket reconciliation

- `orchestrator-current-repair-budget`: done. `digest::render` exposes the
  current cap, used cycles and saturating remaining allowance. The regression
  `current_repair_budget_survives_config_change_replay_and_session_reseed`
  captures the actual next message after a two-to-three cap change, permits
  the third repair, refuses the fourth, and covers a lowered cap, replay,
  streaming reseed and single-shot execution.
- `reviewer-independence-after-fallback`: done. The approved policy and actual
  worker provenance govern reviewer dispatch, fallback, retries, confirmation
  and completion. Twenty `reviewer_independence_tests` cover these paths,
  including unknown provenance, repairs, mutation and replay.
- `v0.2.0-public-cut`: superseded. Its exact-version proposal and historical
  publication assumptions were overtaken by the public release sequence.
  [Current release instructions](../releasing.md) and the
  [v0.4.0 release](https://github.com/craigcode/kranz/releases/tag/v0.4.0)
  are the applicable records; no old tag should be recreated.

Both implemented tickets' named regressions passed in the full v0.4.0 local
suite (3,110 passed, 0 failed, 10 ignored); the exact merged source also passed
[platform CI](https://github.com/craigcode/kranz/actions/runs/35776319180).
Lifecycle changes use `Ticket::write_lifecycle`. They do not fabricate Complete
missions or waive dependency admission. The pilot, ACP follow-ups and remaining
independent contract read-back stay open.

## Dependency review

This batch integrates the changes proposed by PRs
[#65](https://github.com/craigcode/kranz/pull/65),
[#66](https://github.com/craigcode/kranz/pull/66),
[#67](https://github.com/craigcode/kranz/pull/67) and
[#68](https://github.com/craigcode/kranz/pull/68) against the released baseline.
The two dashboard updates share a lockfile and build toolchain, so their combined
result needs a fresh install and one matching embedded bundle. PR #66 failed
`check-embedded` because it did not include that generated bundle.

The CodeQL init/analyze actions remain pinned to the same upstream release
commit, `1c5b675653bb5c22dbe9b12b556ec555138e09fd`, verified against upstream
v4.38.1. Permissions, queries and event triggers are unchanged. The
[Vitest 5 release](https://github.com/vitest-dev/vitest/releases/tag/v5.0.0)
raises its Node requirement to 22; the dashboard already requires Node
22.22.2 or later. Its local configuration and mock/test behavior are checked
through the complete dashboard suite. This is a development dependency change;
no test runner is added to the production bundle.

## Review boundaries

Correctness depends on the combined dependency graph and embedded asset
identity, not the old PR check statuses. Readability improves by reconciling
stale lifecycle claims while retaining historical evidence. Architecture keeps
existing dependency and evidence mechanisms. Security retains pinned actions,
mandatory audits and publication protection. This batch makes no performance
or review-efficiency claim. No live provider is invoked; this is an assistant
self-review, not an independent human audit.

## Validation

All local gates passed on the combined candidate (macOS ARM64):

- Rust workspace: 3,110 passed, 0 failed, 10 ignored; Clippy across all targets with warnings denied; formatting; locked build; strict rustdoc; cargo deny.
- Dashboard: fresh npm install, high-severity audit, TypeScript, all 259 tests, production build, embedded sync and clean-build comparison, lint. Oxlint reports warnings in unchanged React effects; its gate exits successfully.
- Even G2: fresh npm install, high-severity audit, all 22 tests, pack and lint.
- Domain lint, public-tree audit, Rust license notices, package license inclusion and archive-packaging tests.

All three explicit ACP, external-evaluator and mount-helper Docker proof flags
were enabled. These are deterministic fixtures with no provider invocation.
The ten ignored tests remain ignored; this does not claim they were exercised.
[Log digests and raw exit codes](evidence/2026-09-22-maintenance.json) retain the
local gate record. GitHub must still check the pushed candidate before merge.

npm 10.9.8 hit an internal Arborist resolver failure while composing the two
dashboard lockfile updates. Pinned npm 12.1.0 generated the version-3 lockfile;
normal npm 10.9.8 then completed `npm ci` and all app gates. No global npm
upgrade, forced peer resolution or relaxed audit was used.
