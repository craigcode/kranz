# Review-effort pilot execution — human observations pending

The three predeclared synthetic cases have executed against engine commit
`fccdef95334f82f4595005407e2ba7caeacc824c` (v0.4.0). Craig Martin is the assigned
human reviewer; initial decisions, prior-knowledge declarations, active review
time and a final reviewed assessment are still pending. The pilot ticket remains
open. There is no review-efficiency, seed-detection or productivity result yet.

The [derived execution record](evidence/review-effort-pilot/2026-09-22-execution.json)
contains Git identities, original export manifest hashes, input-verification
hashes and actual control observations. Original engine exports, separate CLI
exports, small source review copies, answer keys and successive preparations are
retained outside Git in the operator's private pilot evidence directory. The
derived record is not a replacement or resealed version of an engine export.
Runtime event logs are not committed to this repository.

## What actually ran

The [fixture preparer](../../scripts/review-effort-pilot.py) commits source and
then checking inputs before candidate implementation. The
[fixture driver](../../crates/engine/examples/review_effort_pilot.rs) uses the
existing mission engine, control evaluator, human packet and export APIs.
Worker, controller and reviewer responses are explicitly scripted MockBackend
fixtures. Their edits occur in the trusted fixture process. They do not prove
worker subprocess containment, provider authentication or independent model
judgment. Mock usage is zeroed; no provider process or credential probe runs.

Command checks and baseline/control observations execute through the native
process `fs+net` boundary on macOS ARM64. Each mission also runs the pinned
Python-image external mechanical checker at approval, milestone validation and
final gate. That real contained process verifies every input artifact hash and
checks denied input/checker/root writes and denied network access. It makes no
behavioral judgment. The daemon for this exercise is Docker 29.2.1 in Colima.

Every retained external input is byte-identical to its raw input digest. A
unique marker is present in each worker transcript and absent from every actual
gate request, manifest and input artifact. Captured mock-review prompt, system
message and schema also exclude it. The latter check verifies input assembly;
scripted review remains scripted review.

All three engine runs reach Complete, with an actual candidate branch. None is
locally merged or released. The human procedure still distinguishes completion,
acceptance and merge. The report counts three selected and executed cases, zero
human-reviewed or timed cases, and three cases incomplete pending human review.
Unknown human measurements remain null.

## Preparation corrections retained

The first preparation stopped D1 after its FIFO fixture mistakenly supplied a
streaming controller script to the worker. It produced no candidate result.
The driver now dispatches its separate scripts by session role.

The second preparation executed all three cases, but its policy was only passed
to the engine in memory. Read-only packet generation did not load that same
policy. Its extra B1 variant also changed approved checking inputs without
changing the variant source, so the admission check correctly refused checker
drift before execution. Neither issue is hidden by the later run.

The third preparation persists its policy in the empty fixture HOME and verifies
reader/runner equality before execution. Its separate B1 variant commits the
broken-dependency checker to its own repository, then actually runs it contained:
the missing dependency appears in stderr and the observation is inconclusive.
The original behavioral candidate and its exports are unchanged by that variant.

These are fixture preparation attempts, not human-requested repair rounds.
No human decisions or time measurements are inferred from their duration.

## Current packet limitation to assess

Consumed gate attempts remain historical; they are not renewed consent. The
baseline/candidate observations also remain historical after completion changes
HEAD through generated mission metadata, even where selected source paths show
no change. The packets preserve that conservative label and both Git identities.
A reviewer may need a source comparison or current check rather than treating a
past pass as approval. Assess that extra work with the human observations before
proposing a product change; no policy has been relaxed to improve the pilot.

## Build and provenance verification

A shared target directory reused a sibling checkout's test binary. That test
attempt is retained and excluded from validation. All four workspace packages
were then cleaned from a separate copy-on-write target before recompilation.
The isolated workspace suite passed **3,110 tests, 0 failures, 10 ignored** with
all three container-proof flags enabled. Clippy with warnings denied, formatting,
workspace/example builds, strict docs, cargo deny, domain/public-tree audits,
notices, package licenses and packaging tests passed.

The clean runner and retained case runner have different whole-file hashes, but
only 48 bytes differ: the Mach-O UUID and code-signature payload. Every byte
outside those metadata ranges is identical, including executable code and data.
The comparison preserves both original binaries and records the ranges and
normalized digest; neither is rewritten or resealed.

The original read-only CLI came from the dependency-maintenance build with
unchanged review/outcome Rust sources. A clean CLI rebuilt from the released
engine base reproduces all three packet and outcome projections, differing only
in observation and reporting-window timestamps. All 887 inventoried original
artifacts remain unchanged. The supplied human packets therefore remain usable;
no new case, repair round or human observation is inferred from this rebuild.
[Commands, hashes and comparison results](evidence/review-effort-pilot/2026-09-22-verification.json)
retain this verification separately from original mission exports.

## Reproduction and review boundary

Use a target directory exclusive to the source checkout; do not share workspace
artifacts across worktrees. Build
`cargo build --workspace --example review_effort_pilot --locked`. Prepare
a **new absolute directory** with `python3 scripts/review-effort-pilot.py DIR`;
set `DOCKER_HOST` first if the daemon needs a nondefault endpoint. Run each case
with `DIR/run-env.json` as the complete subprocess environment and
`target/debug/examples/review_effort_pilot DIR/D1` (then B1 and Y1). Use only
locally prepared fixtures: the driver deliberately supports trusted scripted
file writes and is not an executor for third-party preparation files.

The preparer creates a private empty HOME and empty Docker auth configuration.
Native process containment and the preinstalled pinned checker image must be
available. The driver refuses reused attempts, wrong HOME/configuration and
provider credential variables. Each mission has a 600-second ceiling and no
worker respawn. A failure stays incomplete with its original artifacts retained.
Fresh OS environments need their own execution evidence; this record certifies
neither Windows nor an untested Linux setup.

Assistant self-review: correctness rests on actual receipts and explicit unknowns;
readability is tested by the pending human exercise; architecture reuses existing
engine APIs and adds only a fixture example; security keeps empty credentials,
contained checks and untouched original exports; performance is unmeasured and
the convenience sample supports no causal comparison. Human review is a separate
outstanding step, not replaced by these observations.
