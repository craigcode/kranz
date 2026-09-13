# Execution follow-up review — 2026-09-13

A fresh review of the reliability enhancements found three execution defects.
The two negative-control defects are corrected through a control-specific
runner; ordinary command gates retain their existing behavior.

- Linux bubblewrap selected writable scratch as the execution directory,
  overriding the host-side checkout cwd. The control wrapper now changes into
  the read-only checkout inside containment. The path and approved command
  travel as separate shell arguments, and the writable mount inputs stay
  unchanged.
- A successful checker could leave background processes with redirected output
  running after the case returned. The control runner observes leader exit
  without reaping it, kills its still-owned process group on every exit path,
  then reaps the leader and finishes bounded output collection. Cancellation
  reaches the active case as well as preventing later launches. Error paths
  also kill the still-owned direct child before waiting, so a leader that
  leaves its original group cannot evade timeout cleanup.
- A failed version probe for an explicit Claude executable silently fell
  through to another installed candidate. `claudeBinary`, or otherwise a
  nonempty `KRANZ_CLAUDE_BIN`, now selects exactly one executable and returns
  its probe failure. Automatic discovery remains available without an override.

The controls remain advisory, native macOS/Linux only, and subject to the
documented process-group supervision limit. Environment clearing, filesystem
containment, exit-code classification, and legacy gate behavior remain intact.
Evidence environment names include the final offline adjustment for `fs+net`.
The existing SIGINT fixture uses an immediate POSIX-shell version response,
retains its startup and interruption deadlines, verifies the mock's process
group, and captures bounded stderr diagnostics. Selection tests use explicit
candidate lists and sentinels so failures cannot invoke an installed agent.

The dashboard/API, bounded server reads, typed block recovery, resolved-backend
accounting, and Git configuration/process changes were independently reviewed
again without another actionable finding. The focused dashboard review reran
55 tests successfully.

The five-axis review checked exit-status and cancellation correctness, scoped
runner naming and ownership, compatibility with ordinary gates, contained
read-only checkout access and safe PID ownership, and bounded capture/cleanup.
No actionable findings remain in the reviewed changes.

Validation:

- The final Linux `cargo test --locked --workspace --no-fail-fast` passed:
  2,891 passed, zero failed, and nine ignored across 64 summary groups. Git and
  bubblewrap were required capabilities. Two nested-container checks reported
  expected skips because this disposable environment had no container runtime.
  The skip ledger also contains one synthetic capability-test marker.
- Native macOS and Linux each passed all five runner regressions. They cover
  argument/mount construction, retained leader identity, escaped-leader
  timeout, future cancellation, and real contained execution with descendant
  cleanup on success, nonzero exit, inherited output, timeout, and cancellation.
  One ignored test is a subprocess fixture explicitly invoked by its parent
  regression. The delayed-write positive control proves the cleanup probe can
  detect a surviving process.
- Both platforms also passed all nine control integration tests and the CLI
  SIGINT regression. Native macOS required Git and Seatbelt and reported no
  capability skips. Linux used Git 2.39.5 and bubblewrap 0.8.0 on aarch64 with
  Rust 1.94.1. Its source mount was read-only; the disposable environment and
  build caches were removed afterward, with validation logs retained.
- All three explicit-selection regressions passed on both platforms. Together
  with the runner, integration, and interruption checks, 18 focused native
  macOS regressions passed. One healthy shell probe initially exceeded its
  three-second deadline; that exact test passed unchanged on rerun, including
  its working sentinel and failed/hung override cases. No timeout was relaxed.
- Final workspace Clippy, formatting, build, and strict Rustdoc checks passed.
  The Windows GNU engine-library cross-check and standalone Tauri check passed;
  Windows evidence is compilation only.
- Staged secret scanning, public-tree auditing, domain lint, and knowledge
  freshness checks passed.

Investigation notes:

- The failed SIGINT attempt loaded the correct configuration, but a disposable
  diagnostic mission ran installed Claude instead of the fake. The diagnostic
  and fixture-owned processes were stopped and checked for orphans. The exact
  initial probe failure was not retained; its later successful probe does not
  establish why it initially failed. Native loader delays were separately
  observed and sampled before application startup.
- The first complete Linux attempt found two container setup problems: its
  PID 1 did not reap dead orphan fixtures, and its Rust toolchain lacked the
  standard home-directory path expected by a discovery test. A container-local
  subreaper and standard toolchain links addressed those without changing tests.
  A subsequent run exposed incompatible shell syntax for negative process-group
  IDs in the SIGINT fixture. The corrected syntax passed on both platforms
  before the final complete Linux rerun. The final full-suite result above is
  Linux execution evidence; the native macOS evidence is the focused regressions
  and full workspace compile/lint gates.

## CI compatibility follow-up

The first PR run exposed checkout authentication metadata, rather than stale
knowledge claims: `actions/checkout` v7 persisted credentials through repository
`includeIf` directives. Hardened Git refused these during knowledge probes and
before the wrapped macOS suite could start. All 14 checkouts in the CI workflow
now use `persist-credentials: false`; none of those jobs needs authenticated Git
after checkout. The production include refusal remains unchanged.

Secret-scan and release checkouts also disable persisted credentials. Their
later authenticated fetches use step-scoped `GH_TOKEN` through GitHub CLI's
credential helper for that command only; no token enters Git arguments or
repository configuration. Trusted-base scanner selection and the release
script's fresh-main comparison remain intact. Shell and structural checks,
isolated credential-protocol validation, and local release fixtures passed,
including stale-main rejection and unchanged repository configuration.

The Windows ACL fixture also expected a recursive write grant to succeed when
its `.git` pointer redirected metadata inside that writable tree. The fixture
now retains that layout as a refusal assertion and places the positive case's
Git metadata outside the writable root. This changes the test's setup, not the
production boundary. Rust 1.98 additionally introduced a denied Clippy warning
for fixed-size chunk iteration; Git's existing key/value-pair scan now uses
`as_chunks` with the same entries and remainder behavior.

Source review confirmed these changes preserve the Git and sandbox invariants.
The invariants note and its dependent lessons index were reverified on September
13; no knowledge dates were changed to conceal failed Git probes.

Local validation parsed the workflow, checked every checkout's setting and
knowledge's full-history fetch, passed the macOS CI structure check, and verified
all ten knowledge notes. A disposable repository also proved that the current
binary passes without persisted authentication, refuses a checkout-shaped
conditional include, and passes again when that include is absent. Hosted CI
must still rerun against the corrected workflow.
