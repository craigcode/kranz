# Reliability enhancements review — 2026-09-12

The user approved these six improvements following the audit remediation.
They strengthen consent, recovery, evidence, and resource boundaries within
the positioning freeze; they do not add agent dispatch or prompting features.

1. **Bounded server reads.** Synchronous filesystem, authenticated log folding,
   Git probes, and aggregation run in bounded blocking tasks. Each repository
   shares eight slots across scoped routes, compatibility aliases, and WebSocket
   sessions. Catalog and artifact reads have separate limits. Overloaded reads
   return 503; busy WebSocket polls retain their validated cursor. Cancellation
   keeps capacity occupied until the underlying work finishes. See
   [server read work](2026-09-12-server-read-work.md).
2. **Dashboard request freshness.** Mission connection epochs distinguish an
   A→B→A navigation from the original visit to A. Request tokens keep older
   mission/ticket listings from overwriting newer results. Stale successes,
   errors, socket frames, and approval/start responses cannot update a later
   connection; state sequence checks prevent rollback.
3. **Machine-readable recovery reasons.** Additive HTTP codes identify
   `mission_not_hosted`, `turn_in_flight`, `repository_busy`, and `stale_plan`.
   Additive milestone `blockContext` identifies recovery ownership and cause.
   Consumers prefer structured identity over message wording, so an unrelated
   block cannot trigger workspace recovery and repository contention does not
   discard a reviewed plan. See the [block-context contract](2026-09-12-block-causes-contract.md).
4. **Resolved-backend accounting.** Outcomes, token grouping, and fallback cost
   calculations use recorded per-run backend identity. This accounts for actual
   fallback and tier changes. Provider-reported cost, including explicit zero,
   remains authoritative; estimates remain estimates.
5. **Git configuration and process boundaries.** Repository ordinary and
   conditional includes are refused. Enforced children cannot rewrite protected
   config inputs, discovered ancestor gitlinks, or replaceable metadata parents;
   index/object/ref writes remain available. Git capture has command-specific
   deadlines and separate output limits, with process-tree cleanup on failure.
   Windows assigns a suspended child to its Job before resuming it. See
   [the execution boundary and exact limits](../git-execution-boundary.md).
6. **Opt-in negative controls.** Operators can inspect paired valid/defective
   fixtures and pinned checking inputs. Approval and final validation execute
   the same command against both cases and record fresh advisory gate evidence.
   Zero checks, setup errors, timeouts, and missing receipts are inconclusive.
   Approved control definitions cannot change during additive replanning.
   See [control behavior and protocol](../contract-controls.md) and its
   [schema contract](2026-09-12-negative-controls-contract.md).

## Five-axis review

| Axis | Review conclusion |
| --- | --- |
| Correctness | Deterministic stale-response, reverse-order, typed-recovery, backend-fallback, overload, and control-revision fixtures exercise the changed decisions. Approval/final-gate integration checks distinct revision-bound receipts and legacy behavior. |
| Readability | Recovery codes replace prose-dependent decisions while retaining explanatory text. Focused read-work, Git supervision, and control modules keep lifecycle orchestration recognizable. |
| Architecture | Existing event, gate, evidence-export, and sandbox mechanisms remain the integration points. Router extensions preserve the public `ServerState` shape; no new mission-truth cache is introduced. |
| Security | Review covered integrity-preserving suffix reads, immutable control inputs, containment refusal, and Git config/rename boundaries. An ancestor-gitlink gap found in independent review was corrected, including parent-node protection and container mount/mask ordering. |
| Performance | Blocking work is admitted before spawning; permits survive requester cancellation. Git output and command duration, artifact bytes, and control concurrency/execution are bounded. Whole-log validation remains deliberately intact. |

## Compatibility and material limits

- HTTP status and error text remain available. Legacy message fallback applies
  only when the code is absent. Absent `blockContext` retains historical replay
  classification; a present context, including unknown values, does not fall
  back to prose. Existing assertions omit `negativeControl` when absent.
- Old runs without backend evidence retain each accounting consumer's previous
  config fallback. The change does not invent historical provider attribution.
- Server limits bound concurrent reads, **not event-log size or total fold
  cost**. Artifact reads retain an 8 MiB file limit. Read responses and mutation
  scheduling otherwise retain their existing semantics.
- Git configuration protection closes the contained-writer boundary. Enforcement
  off and separate unsandboxed host writers retain the documented preflight/read
  race. Unix process groups provide supervision, not containment against a
  program deliberately leaving its group.
- Controls require native macOS Seatbelt or Linux bubblewrap. Unsupported
  platforms/providers produce inconclusive evidence. Controls remain advisory,
  and checker receipts do not independently prove checker honesty or complete
  requirement coverage. This review record does not claim fresh native Linux
  or Windows execution results.
- The [parent read-back ticket](../../.kranz/tickets/contract-readback-and-negative-controls.md)
  remains open for independent model read-back, input isolation, and the operator
  comparison/disposition surface. A verified control pair does not implement
  that work.

## Validation

All required local gates returned exit 0:

- `cargo test --workspace`: 2,916 passed, 9 ignored across 64 test targets and
  documentation groups, including 1,212 engine library tests. Real container
  tests used the available local Docker daemon. The cancellation-admission
  regression also passed after the final identifier cleanup.
- `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check`, and `cargo build --workspace`.
- Dashboard clean dependency installation, TypeScript, 235 tests across 31
  files, production build, embedded bundle sync/check, lint, and dependency
  audit. No dashboard dependency vulnerabilities were reported.
- Strict `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps` and
  standalone Tauri `cargo check --locked`.
- Windows GNU engine-library cross-check with `--locked`, without warnings.
  This is compile evidence, not native Windows execution evidence.
- `cargo deny check`, root and Tauri `cargo audit --deny unsound`, dependency
  notices and package license checks. Tauri retains six allowed unmaintained
  dependency advisories; no audit exceptions were added.
- Staged secret scan, domain lint (850 files), public-tree and CI-mode
  public-history metadata audits, operator-marker audit controls, macOS CI
  checks, and knowledge freshness (all 10 notes). Local links in all 11 new or
  changed Markdown documents resolved.

Independent source reviews covered the dashboard/API, bounded reads, control
integration, Git follow-up, and documentation. The initial workspace run exposed
a stale validator-profile test assertion and exhausted the existing disk-space
preflight headroom. The assertion now checks read denial and write protection
separately; old incremental compiler caches were removed. The complete rerun
passed without weakening containment or the disk-space guard.
