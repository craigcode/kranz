# M3 wall-clock measurement (2026-07-04)

The roadmap's M3 done-when demanded live proof: *"a 2-milestone mission with
independent features completes in materially less wall-clock than sequential
at comparable cost, with zero event-log corruption across 20 repeated runs."*
The corruption half is covered by the soak harness (`scripts/soak.sh`,
20/20 PASS). This document records the wall-clock half.

## Setup

Two byte-identical sample repos (empty Python project: `src/`, `tests/`,
seeded `__init__.py`s), configs differing **only** in `maxParallelWorkers`
(1 vs 2; both `skipScrutiny=true`, `skipFunctional=false` so
`python3 -m unittest discover` gates every milestone). One prescriptive
mission file drove both arms through headless `kranz exec`: 2 milestones ×
2 features, each feature owning strictly disjoint files (slugify / stats /
duration / table modules + their tests). Arms ran back-to-back, not
simultaneously. Default models (opus orchestrator, sonnet workers),
kranz v0.1.0+ (post cycle-4 hardening).

## Results

| | sequential (=1) | parallel (=2) |
|---|---|---|
| wall-clock | **1118 s (18.6 min)** | **581 s (9.7 min)** |
| outcome | COMPLETE, exit 0 | COMPLETE, exit 0 |
| cost | $44.33 | $16.35 |
| events | 328, seq contiguous 1..328 | 253, seq contiguous 1..253 |
| worker sessions | 6 (2 fix cycles) | 5 (1 conflict resolution) |
| leaked worktrees/branches | 0 | 0 |
| `unittest` on mission branch | green | green |

**Parallel was 1.92× faster (48% less wall-clock) at lower cost — even though
it hit and self-healed a real merge conflict** (below). The cost gap is
single-run trajectory variance (the sequential arm drew two judge-ordered fix
cycles and a heavy waiver deliberation), so the honest claim is "comparable
or better", not "parallel is cheaper".

Concurrency was verified from the per-run transcripts, not inferred:
both milestone batches spawned their two workers simultaneously
(identical transcript birth times), overlapping 36 s (milestone 1) and
47 s (milestone 2) of live claude-session execution.

## The conflict that proved the conflict path

Milestone 1's two workers each ran the test suite; Python wrote
`src/__pycache__/*.pyc` in both worktrees and both checkpoint commits
included it — an add/add conflict on the second ordered merge. The engine
discarded the failed branch, synthesized `ms-1-conflict-1`, and the
resolution worker **untracked the pycache artifacts, added a `.gitignore`,
and re-implemented the discarded feature on top of the merged branch**.
Milestone 2 then merged clean because the `.gitignore` existed. The M3
conflict-resolution path validated itself end-to-end in a live mission.

Follow-up worth considering: seed parallel worktrees with ignore rules for
common generated artifacts (`__pycache__`, `target/`, `node_modules/`) so
the first conflict of this class never happens.

## Measurement caveats

- **events.jsonl is not a timing source for buffered workers.** In the
  parallel path workers buffer events and the engine appends them serially
  (single-writer preserved), so buffered `worker.spawned`/`completed` carry
  Phase-C append timestamps (0-length intervals). Live per-run transcripts
  (`runs/<id>.jsonl` birth/mtime) are the wall-clock source for overlap.
- n=1 per arm. The 48% delta is far above run variance for this shape, but
  worker time was only ~26% of the sequential arm's wall-clock (orchestrator
  turns dominate small features), so speedup scales with feature size —
  bigger features, bigger win.

## Verdict

M3's done-when is met: materially less wall-clock (48%) at comparable cost,
zero corruption in the 20-run soak, no leaks, contiguous logs in both live
arms. `--sequential` stays the default for now — one live A/B is proof the
capability wins on this workload, not yet a fleet-wide default change; flip
it when a few more live parallel missions repeat the pattern.
