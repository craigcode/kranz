# M8 TypeScript and multi-repository live proof

Date: 2026-08-12

Host: macOS 26.5.2 (25F84), arm64

Kranz: 0.1.0

Node: 22.23.1

## Result

M8's fresh-repository and cross-language path passed end to end. A new,
dependency-free TypeScript repository was initialized by Kranz, took a real
ticket through draft, approval, queue, isolated worktree execution,
independent validation, final contract gates, and the repository-scoped merge
API, and landed a non-empty change on `main`.

The one-process multi-repository host also served two healthy repositories and
kept one authenticated Slack Socket Mode bridge connected with an exact
workspace/channel route. The existing integration routing suite passed for
explicit repository selection, exact channel routing, thread affinity, sole
healthy fallback, and ambiguity refusal. No human Slack command was sent, so
this receipt does not claim an observed inbound Slack event.

## Repository and mission

- Disposable repository: `/private/tmp/kranz-m8-rerun.7xAmyi`
- Base branch: `main`
- Ticket: `add-clamp-helper`
- Mission: `m-bb3632`
- Approved-plan commit: `d7476d3`
- Feature/fix/documentation commits: `f31fb4d`, `c19912e`, `5c563d0`
- Mission report commit: `41cc743`
- Gated merge commit: `e4e542618e2d107727011a4bab3ecf4a8b8703ad`
- Final ticket state: `LANDED`
- Mission cost: $16.77

The primary checkout remained on `main`; workers and validators used the
mission's dedicated worktree/snapshots. The final product diff added
`src/clamp.ts`, `test/clamp.test.ts`, and a README example. Fourteen Node tests
passed. The merge commit also retained the approved plan, report, mission
catalog, and cross-mission lesson as repository-owned audit artifacts.

## Onboarding and lifecycle checks

The repository started with only a dependency-free TypeScript source/test
pair and a package script using Node's built-in TypeScript stripping and test
runner. The exercised operator flow was:

```text
kranz init
kranz ready
kranz draft add-clamp-helper
kranz ticket queue add-clamp-helper
kranz work --once
kranz serve
POST /api/repos/kranz-m8-proof/missions/m-bb3632/merge
```

- `kranz init` detected `npm test`, created an unconditional merge gate, and
  was byte-idempotent on a second run.
- `kranz ready` reported the cold-start condition honestly: zero completed
  missions rather than invented calibration confidence.
- The approved changed-path contract excluded `.kranz/missions/**`, so
  engine-owned plan artifacts could not make the scope gate unsatisfiable.
- The merge endpoint returned
  `{"commit":"e4e542618e2d107727011a4bab3ecf4a8b8703ad","merged":true,"staleBase":null}`.

## Multi-repository and Slack evidence

The successful TypeScript repository and the Kranz repository were registered
temporarily in the operator catalog. One `kranz serve` process reported both
as healthy. The two pre-existing catalog roots (`sgian` and `sgian2`) remained
visible as unavailable and did not contaminate either healthy repository's
state.

The first Slack start failed closed because no catalog repository had a
channel route. After resolving the configured workspace identity through
Slack `auth.test`, an exact temporary workspace/channel mapping was attached
to the proof repository. A restarted host kept its single Socket Mode bridge
connected. These routing tests then passed:

```text
catalog::tests::explicit_repo_precedes_channel_and_is_removed_before_action_routing
catalog::tests::thread_affinity_precedes_the_current_channel_mapping
catalog::tests::exact_channel_then_sole_healthy_and_ambiguity_refusal
```

This proves live bridge authentication/connection plus the routing logic's
integration behavior. It is not evidence that a real inbound workspace
message traversed the bridge. That last operator event remains a small live-QA
follow-up if M8 is to claim literal Slack-command end-to-end coverage.

## Findings produced by the proof

An earlier disposable attempt, mission `m-760060`, was abandoned. Its plan
compared the feature diff to `$KRANZ_BASE_SHA` without excluding
`.kranz/missions/**`, making the contract unsatisfiable. It also exposed the
macOS `/var` versus `/private/var` alias in the touch-set guard and validator
scratch paths. The successful rerun exposed two further runtime hygiene gaps:

- Claude's own temp root needed `CLAUDE_CODE_TMPDIR` pinned to session scratch.
- generated Seatbelt profiles were written at the tracked mission root rather
  than the ignored `runs/` directory.

Those defects were fixed with regression tests on
`codex/stabilization-proof-sprint`. The six untracked profile files visible in
this pre-fix proof repository are retained only as the reproduction; the
shipping backend now writes profiles under ignored mission runtime storage.

## Cleanup

The temporary catalog rows and channel route were removed, and the operator's
original `sgian`/`sgian2` catalog was restored byte-for-byte at the host-row
level. The proof repositories are disposable and are removed after this
receipt is committed. No Slack credential or serve token is recorded here.
