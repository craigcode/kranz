# Mission report — m-a78225

**Goal:** Free the single-writer lock held by serve's adopted in-planning engines when they go idle (time-based auto-release plus an on-demand kranz release verb), so a terminal kranz plan is never stranded behind a serve that adopted the mission.

Branch `kranz/mission-m-a78225` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 29m 07s
**Tokens:** 47166 in / 103728 out / 12031421 cache read / 600728 cache write
**Cost:** $41.83 actual vs $9.18–$45.88 estimated (expected $18.35)

## What shipped

### Milestone 1 — Server-side idle auto-release and release endpoint ✅

- ✅ **Add planningIdleReleaseMinutes to MissionConfig** — 1 run
  - `16da7f3` [f-1-1] add planningIdleReleaseMinutes to MissionConfig
  - `fdec48d` [f-1-1] checkpoint (engine commit)
- ✅ **Last-use tracking, sweep_idle, and lazy background sweeper in MissionHost** — 1 run
  - `c0f72f1` [f-1-2] track last-use per planning cell, add sweep_idle and lazy background sweeper
- ✅ **POST /api/missions/:id/release endpoint** — 1 run
  - `53c43ad` [f-1-3] add POST /api/missions/:id/release endpoint

### Milestone 2 — CLI release verb and documentation ✅

- ✅ **kranz release CLI verb calling the serve API** — 1 run
  - `e02af82` [f-2-1] add kranz release CLI verb calling the serve API
- ✅ **Document the release endpoint and update the slice-5 trade-off note** — 1 run
  - `8d6faf1` [f-2-2] document release endpoint and update slice-5 trade-off note

## Validation history

### ms-1 round 1 — Server-side idle auto-release and release endpoint

- [critical] [a2] kranz CLI tests must include parsing of `kranz release` and its --mission/--url/--token options — The milestone diff (15833630..HEAD) touches no file under crates/cli. crates/cli/src/cli.rs's Command enum (line 69) has no Release variant — grep for 'Release' in cli.rs returns no matches. The only … [truncated]
- [critical] [a5] docs/protocol.md documents the release endpoint and docs/slack-management.md's slice-5 note records it is closed by idle-release — The a5 validation command fails on both halves: grep '/release' docs/protocol.md → no match (the endpoint table at docs/protocol.md:37-39 lists approve/start/abandon but not release), and grep 'closed… [truncated]
- [minor] a5 — Command: `grep -q '/release' docs/protocol.md && grep -q 'closed by idle-release' docs/slack-management.md` → FAIL (exit 1). `grep -n '/release' docs/protocol.md` returns no matches — the endpoint tab… [truncated]

Disposition: waived.
- [a2] kranz CLI tests must include parsing of `kranz release` and its --mission/--url/--token options: Not an ms-1 defect: the CLI release verb is the deliverable of pending feature f-2-1 (ms-2), which will add the Command::Release variant, its --url/--token flags, the API-calling handler, and the clap parse test satisfying a2. No fresh worker needed — already scheduled.
- [a5] docs/protocol.md documents the release endpoint and docs/slack-management.md's slice-5 note records it is closed by idle-release: Not an ms-1 defect: both doc edits are the deliverable of pending feature f-2-2 (ms-2), whose criteria require the protocol.md /release row and the 'closed by idle-release' slice-5 note that satisfy a5. Already scheduled; a fix-feature would duplicate it.
- a5: Duplicate of the a5 finding above; covered by pending feature f-2-2. Waived for the same reason — the docs work is scheduled, not missing.

### ms-2 round 1 — CLI release verb and documentation

No findings.

## Contract outcomes

- ✅ **[a1]** kranz-server tests pass, including: an idle planning cell past the window is released with its lock provably free via EventLog::acquire; a cell with a turn in flight survives the sweep; POST /api/missions/:id/release returns 200 (frees the lock), 409 mid-turn, and 404 for an unknown mission. *(command: `cargo test -p kranz-server`)*
- ✅ **[a2]** kranz CLI tests pass, including parsing of `kranz release` and its --mission/--url/--token options. *(command: `cargo test -p kranz`)*
- ✅ **[a3]** The engine crate's tests still pass after the new MissionConfig field is added (config load/validate/defaults unaffected). *(command: `cargo test -p kranz-engine`)*
- ✅ **[a4]** The workspace is clippy-clean at deny-warnings. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a5]** docs/protocol.md documents the release endpoint and docs/slack-management.md's slice-5 trade-off note records that it is closed by idle-release. *(command: `grep -q '/release' docs/protocol.md && grep -q 'closed by idle-release' docs/slack-management.md`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
