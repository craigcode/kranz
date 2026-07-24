# Failure-mode harvest — 2026-07-24

The systematic pass over the mission corpus (53 closed missions,
`.kranz/lessons/`, and this week's validator-era post-mortems), per
`failure-mode-mining-to-fixtures.md`: name each recurring cluster, count
it, and pin where its regression fixture lives. Re-runnable: the cluster
counts below derive from `kranz ready --json` (contractHealth.blocked) and
`kranz outcomes`; rerun both after new missions land and extend the table.

## Clusters → fixtures

| # | Cluster | Evidence (missions) | Count | Fixture (where the third occurrence is prevented) |
|---|---------|--------------------|-------|---------------------------------------------------|
| 1 | **Contract-authoring bugs** — inverted grep, BSD-vs-GNU `grep -L` exit inversion, two-filter `cargo test`, substring-colliding test filters | m-8b3ec3, m-66aff8, m-0f1abd, a3 of m-66aff8 | 4+ | `contract_lint.rs` smoke-executes every command assertion on the untouched base at approval; suspect classification pinned at contract_lint.rs:500 ("already passes before the work"), which covers the substring-collision shape mechanically. AGENTS.md rule 5 covers wrong-test collisions for human-authored gates. |
| 2 | **Validator command friction** — awk/`;`/pipe compounds, output redirection, accidental backgrounding, Monitor stalls | m-9e4ef3 (5 blocks), m-3cda6a | 7 | Validator repair (2026-07-23): scrutiny/mechanical split (`982358c`), engine-run contract commands (`bfac7e6`), structured-refusal denial detection (`2a9035a`), guidance injection (`ab17654`), unblock-add-fix (`3cdfb7d`) — each with unit + e2e tests in mission_test.rs/backend_mock_test.rs. |
| 3 | **Secret-scan FP families** — identifiers, env reads, fixtures, placeholders, detector self-matches, pid-derived lock tokens | m-0f1abd, m-9dc8c1, m-9e4ef3, pre-public scan (58 findings) | 58 fingerprinted | `.kranz/secret-allowlist` reviewed-fingerprint blocks; scanner self-test in scrub_test.rs canaries; merge path reads the allowlist from the pinned base (history rewrite cannot re-waive). |
| 4 | **Grant-park patterns** — deny-default timeouts, grant cap exhaustion, validator grants that never fired | m-3cda6a (3600305ms deny-default), m-9e4ef3 | 3+ | Denial-detection fix (`2a9035a`) so structured refusals park grants at all; grant-flow e2e in mission_test.rs (`validator_denial_grant_approved/denied`). |
| 5 | **Worker auth deaths at turn 1** — credential-less worker homes producing $0 empty runs | m-66aff8 (5 deaths), m-165b6f | 2 clusters | The auth probe before any run-phase session (orchestrator.rs auth_verify), `auth_verify` unit tests; honest-block wording in the m-66aff8 waiver record. |
| 6 | **Same-millisecond control-enqueue ordering** — pause/resume inverted by random filename suffix | CI run 29888053578 | 1 (flake) | Nanos-prefix fix (`31c1127`) + NEW fixture `control.rs::rapid_back_to_back_enqueues_drain_in_issue_order` (this harvest). |
| 7 | **Mock-script session-order bugs** — auth probe consuming the orchestrator's streaming script, 6-hour parked tests | m-9e4ef3's own tests | 2 | The reconcile test fixes (`92e9602`): provision the probe's script in session-start order; the tests themselves are the fixture. |
| 8 | **Respawn/fix loops at the cap** — conversion turns demanding fixes past the fix-cycle cap | m-9e4ef3, m-3cda6a | 4 (cap blocks) | `unblock-add-fix` repair action (`3cdfb7d`) + `escalate_or_block` tier escalation; cap-behavior tests in mission_test.rs. |

## Open candidates (not yet fixture-worthy)

- **Stale embedded dashboard bundle after merges** (m-d1e3c3 pre-merge
  review) — mitigated procedurally (rebuild + sync-embedded + CI
  check-embedded); a merge-time engine check could pin it if it recurs.
- **Windows/POSIX probe asymmetries** (PATHEXT, path-in-JSON fixtures) —
  one each, fixed on sight (`11f2ff2`, `cdccff2`); cluster forms if a
  third appears.

## How to re-run this harvest

1. `kranz ready --json` → `contractHealth.blocked` for the blocked-cause
   histogram (grant/scan/contract-bug/cap/untrusted counts).
2. `kranz outcomes` → grant-park latency + escalation ledger for consent
   friction.
3. `ls .kranz/lessons/` → machine-written lessons; each names its mission.
4. Append new clusters above with counts and example ids; any cluster that
   reaches **three** occurrences without a fixture gets one, named after
   the cluster.
