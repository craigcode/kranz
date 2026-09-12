# September 11 audit remediation

Scope: the six findings and refactor recommendations reviewed against
`c12814f8e91267f7b1dd3d20b2bf569fa7b32f04`.

| Finding | Change | Regression evidence |
|---|---|---|
| F1: mandatory review bypass | One deterministic completion check covers milestone skip, normal closure, final gates, and mission completion. Required reviewers must have successful compatible evidence from the latest relevant round. Finalization detects checkout drift; pending-only revisions preserve unaffected review context. | Blocked → steer → model skip with real feature commits; replay; both roles; failed/stale/tampered review; gate mutations; legacy no-policy completion. |
| F2: stale Git configuration | Local Git uses an explicit cleared environment and rechecks executable configuration before commands. New driver names fail closed; original overrides survive cloning and removal. Conditional includes fail closed because a branch-changing command can activate previously invisible configuration. | New and changed drivers, cloned handles, linked worktree config, environment sentinels, and a conditional-include worktree positive control. |
| F3: stale plan approval | Full canonical SHA-256 identity travels from preview to approval across dashboard, CLI host adapter, and Slack. Comparison and consumption share the serialized approval operation. | Preview A → replace with B → refuse A; matching approval; failed approval retains pending plan; dashboard refresh after 409. |
| F4: external import reads | OpenSpec imports pin repository-local source ancestors and each nested directory. Files are opened no-follow and checked as regular files. Traversal failures propagate. | Linked proposal/design/spec/root/ancestor, checkout aliases, normal nested specs, and aggregate/depth limits. |
| F5: redirected ticket writes | Ticket creation uses exclusive relative creation through pinned `.kranz/tickets` handles. CLI, REST, Slack, and import share it; imports construct the complete body before writing. | Dangling/existing leaf links, linked parents, concurrent creators, and malformed input before creation. |
| F6: blocking/unbounded artifacts | File opens reject nonregular handles and use nonblocking open on Unix. Artifact reads run in bounded blocking tasks with byte limits. | FIFO child process with deadline, exact/oversized/invalid-UTF-8 files, and artifact response status checks. |

The filesystem utilities have three concrete consumers rather than parallel
implementations of the same trust rules. Configuration now declares project and
runtime authority together in one sensitive-key registry, with table-driven
tests. Projects can add reviewer requirements but cannot remove an operator's
floor. Cross-review also reproduced a positional-JSON-array bypass of nested
configuration checks. Protected configuration containers now require object
shapes on both sides of the project merge; synthetic typed-load regressions
cover sandbox, tool, and reviewer-policy authority. Sandbox enforcement is
ranked from its typed enum so alternative serde encodings cannot hide a weaker
setting. Finalization is extracted
from the orchestrator by lifecycle responsibility.
Persisted event and state schemas are unchanged.

## Operational changes

- Preview approvals must send `planIdentity`. Missing or stale identity returns
  HTTP 409 and requires a refreshed review. Older Slack cards with short hashes
  must also be refreshed. Explicit untargeted operator approval remains available.
- Artifact responses are limited to 8 MiB and eight concurrent file readers.
  Oversized artifacts return 413; reader saturation returns 503. Content is never
  silently truncated. The optional mission index retains its empty fallback.
- OpenSpec source content is limited to 8 MiB in aggregate, 1,024 spec entries,
  and 32 nested spec directories. Explicit external change roots remain supported;
  repository-local roots and aliases cannot follow repository-owned ancestors.
- Local hardened Git refuses conditional include directives. Operators using
  those directives must resolve their local configuration before retrying.
- Vitest and its companion packages are updated to 4.1.11. The dashboard was
  installed with `npm ci`, rebuilt, and synchronized into the CLI bundle.

## Remaining boundaries

F2 is a fail-closed preflight mitigation, not an immutable configuration view.
A process with concurrent write access to Git configuration can still race the
preflight and Git's own read. Environment clearing reduces inherited authority;
enforced configuration write-denies or an immutable filesystem view are needed
to eliminate that race. Network Git retains its separately authorized credential
behavior. No new claim of containment is made for sandbox-off execution.

The standalone Tauri lockfile still has seven informational RustSec warnings.
They are tracked with dependency-path evidence in
[the Tauri disposition ticket](../../.kranz/tickets/tauri-informational-advisory-disposition.md).
The GTK dependency graph uses glib 0.18; the
[upstream fix starts at 0.20](https://rustsec.org/advisories/RUSTSEC-2024-0429.html).
No advisory suppressions or unverified major desktop dependency migration were
introduced. Root workspace and npm audits have no reported vulnerabilities.

## Validation

All final gates passed on the combined changes:

- `cargo test --workspace`: 2,865 passed, zero failed, seven existing ignored,
  across 63 test targets. The FIFO child's result is not double-counted.
- `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo fmt --all --check` after running `cargo fmt --all`.
- `cargo build --workspace`.
- Dashboard `npm ci`, TypeScript, all 201 tests across 30 files, lint, production
  build, embedded sync, and embedded consistency check.
- Root `cargo audit`, `cargo deny check`, and dashboard `npm audit`: passed;
  no reported root/npm vulnerabilities. Standalone Tauri warnings are described above.
- Operator-marker and release-packaging Python tests: 10 and two passed.
- Final diff whitespace check: passed.

The final workspace run includes 20 configuration trust tests, 55 Git integration
tests, 20 reviewer-independence tests, and the filesystem and stale-approval
regressions. The configuration representation counterexamples were observed to
fail their new regression assertions before their fixes.

Native execution is on macOS. Container tests use the existing local Colima
socket via `DOCKER_HOST`; this resolves the original unavailable-default-socket
failures without weakening or skipping those tests. Linux and Windows were not
executed in this remediation. These validation results were recorded before
the operator-authorized commit and push.
