# v0.2.0 release-candidate review

Historical local-preparation record. The later
[2026-09-06 unattended acceptance receipt](2026-09-06-unattended-acceptance.md)
closes the remote platform-CI and live-smoke gaps for `d1eafe0`. References below
to the missing Actions credential and pending remote checks describe this
earlier review, not the current release status. Owner review and publication
remain gated by the [current checklist](../public-readiness.md).

The release work is based on active origin commit
`eea2e62d8723d6e2ea3d59890c07820f17a6b36c`, in a fresh clone of
`craigcode/kranz`. The earlier local audit was on the archived lineage.
Its fixes were reconciled as source changes; no archived Git objects were
imported into the clean public-origin history.

## Changes reviewed

- GitHub comment triggers require an explicit `hooks.allowUsers` list. An
  authentic webhook signature alone does not authorize its comment author.
  Workflow failure triggers also verify that the head belongs to the receiving
  repository, rather than trusting a fork's branch name.
- Engine Git operations disable external diffs and text converters and refuse
  custom merge execution. Handles refresh after workers return so new driver
  names do not escape the protected configuration snapshot.
- Mount-backed sandboxes use private authority directory views to hide files
  created or atomically replaced after launch, without creating placeholder
  credentials in the checkout. Sibling mission metadata stays protected.
  Seatbelt pins authority ancestors against rename-based escapes.
- Validator cache copying uses descriptor-relative, no-follow operations and
  rejects symlinks and special files while preserving executable permissions.
- Cursor uses a private Keychain backing store with a local login alias,
  migrates legacy stores without replacing their contents, rejects injected
  passphrase commands, and tests actual native lock-state transitions.
- Container gates retain a separate allowlisted host client environment for
  both launch and timeout cleanup. Worker environment values cannot redirect
  that client, and client proxy settings cannot inject payload credentials.
- Both Cargo lockfiles replace yanked `chacha20` 0.10.1 with 0.10.2. No
  dependency policy or advisory exception was broadened.
- The live rehearsal exposed repeated worker attempts to commit through a
  correctly denied shared Git index. Static worker instructions now direct
  that case to the existing reviewed engine checkpoint, without widening the
  sandbox. Verification instructions also preserve actual command exit codes.
- Interrupting the rehearsal exposed native processes surviving the CLI.
  `exec`, `run`, and `work` now unwind on Ctrl-C, and all six native backend
  session types kill their process group on drop. Regression tests cover a
  tool descendant, exit code 130, retained mission logs, and lock release.
- The acceptance script now checks the delivered branch in a detached
  worktree. An offline regression fixture verifies five delivered tests while
  the original primary branch and commit remain unchanged. Synthetic auth
  values use explicit example names so generated fixture plans do not look
  like leaked credentials.
- The rehearsal also exercised checkpoint refusals for runtime function
  calls caught by the scanner's generic assignment heuristic, a Python default
  referencing a synthetic value, and one synthetic negative-test credential. Their five exact fingerprints were reviewed and
  scoped to the synthetic fixture's allowlist;
  no production rule, file, or credential category was exempted. Failed
  rehearsals retain their repository for inspection/resume. A fixed base-owned
  HTTP contract now drives the first milestone, and final acceptance also
  verifies CLI success, CLI authentication failure, documentation, and that
  the delivered branch did not alter the contract. A known faulty auth helper
  now exists in the base, and the final audit requires its first change to
  belong to a completed fix-feature. The helper is tested independently of
  HTTP behavior. Eight offline harness regressions run in CI without a model
  or credentials, including premature and ineffective repairs. The receipt
  check accepts the persisted `SHA subject` format as well as bare hashes;
  the regression fixture uses the actual labeled format by default.
- Live transcript review found cumulative Claude costs being counted again
  on each streaming turn. The backend now emits cost deltas while retaining
  raw totals and per-turn token usage, matching the provider's
  [streaming cost semantics](https://code.claude.com/docs/en/agent-sdk/cost-tracking#track-costs-in-streaming-input-mode).
  Regression replays cover observed totals, missing/invalid/zeroed results,
  conversation resets, process resume, and single-shot calls. Historical
  events are not rewritten; old mission cost estimates may be inflated.
- Recovery testing exposed a force-removal of retained integration worktrees
  that discarded uncommitted repairs. Resume now retains the worktree, and
  setup verifies repository ownership, registration, and branch before reuse.
  Regressions cover index/worktree/untracked preservation and refusal of a
  changed branch or symlink without deleting evidence.
- Redaction of escaped code inside a JSON recovery decision consumed an escape
  backslash, making valid model output unparsable. JSON strings are now decoded,
  scrubbed, and re-escaped individually. Object layout and duplicate keys are
  preserved so redaction cannot turn a rejected duplicate-key decision into
  an accepted one. Regressions cover fenced replies, nested transcripts,
  credential field context, escaped credentials, numeric credentials, and
  unchanged secret-free input.
- Finding diagnostics bracket the rule name so `authorization-bearer` cannot
  cause the following source path to be redacted as a credential. The raw
  finding schema and fingerprint are unchanged; the regression verifies that
  the formatted diagnostic survives the audit log's redaction pass.
- Full-file Python scans no longer treat a suite-header colon (for example,
  a comparison against `VALID_TOKEN`) as a secret assignment. Only the
  assignment heuristics receive a view with those terminal colons masked;
  fixed credential patterns still inspect the original bytes. Regressions
  verify original offsets, real assignments immediately after conditionals,
  and multiline JSON/YAML/Python dictionary credentials.
- Two later live runs exposed event lines failing their own integrity hashes:
  default JSON float parsing rounded costs between hashing, writing, and
  reading. New storage envelopes carry additive `v:2`, preserve numeric bits,
  sort object keys, and bind the version into the hash through the
  `kranz.event-log.v2\n` prefix. Event payload schemas are unchanged.
  Versionless seals retain their original numeric interpretation and hash
  format: an independent 99,972-case comparison against the original parser
  has zero mismatches, and a 96-case reference fixture runs in CI. Regression
  tests cover keyed legacy records, mixed old/new append, tail reads, signed
  fractional config commands, numeric tampering, version removal/change,
  unsupported versions, and alternate JSON map-order features. Valid legacy
  records are not rewritten. Already corrupt logs remain refused and retained;
  older binaries cannot verify new v2 seals. Replay of eight retained mission
  logs preserves the old reader's success/refusal results; successful reads
  produce identical provenance JSON, and every log remains byte-unchanged.

The review covered correctness, readability, architecture, security, and
performance. The [additive receipt contract](2026-09-05-feature-progress-contract.md)
preserves legacy event/state behavior. Changes retain environment clearing,
validator containment, default protected Git handles, Windows boundaries, and cache-only container mounts. New authority views enumerate
small control directories at sandbox construction; warmed cache copying remains
bounded by the existing size ceiling.

The separate Linux live-smoke job now installs and probes bubblewrap and
enables the same hosted-runner user-namespace settings as the Rust job.
Previously it lacked those prerequisites even though validators require
containment. Workflow parsing and shell syntax pass locally; execution on
GitHub remains a remote gate.

The acceptance harness's final audit previously ran worker-authored imports
and tests directly on the host. A test-only adapter now routes the fixed final
command through the existing production gate sandbox with filesystem
enforcement, cleared credential environment, and mission authority read-denies.
It fails closed when containment cannot be established. Nine offline harness
regressions pass, including generated code probing environment credentials,
authority files, and writes outside the deliverable. Substituting the old
unsandboxed runner makes the new regression fail on credential inheritance.
The filesystem tier permits networking, including the HTTP test's localhost
listener; this is not a network-isolation claim.

Sequential retry review also found a checkpoint retained in Git but omitted
from the final feature receipts. The engine now pins and records cumulative
progress before retry/resume, retaining failure attribution and the
fix-feature supersession guard.

## Local evidence

Commands ran with their actual exit codes retained, on macOS arm64 with
Rust 1.97.1 and Node 22.23.2. The latest production source is commit
`e1f482c`; `2afe3de` adds the scoped fixture fingerprint, and `117de53` fixes
the Linux live-smoke prerequisites. `9fed360` contains the final acceptance
audit; all four workspace gates were rerun after that test-adapter change.
The native gates, minimum-version check, and dashboard gates include the
receipt change.

The local model rehearsal uses Claude Code 2.1.261 with existing local OAuth.
Its orchestrator, worker, and scrutiny validator use `opus`; the functional
validator uses `sonnet`. Each role has `maxTurns=64`, `maxBudgetUsd=3`, and
`sandbox={enforce: fs, provider: process}`; orchestrator effort is `medium`,
`maxRespawns=1`, and the script passes `--max-cycles 3`. These are test-profile
settings, not changes to product defaults. The manual CI smoke job's Claude
Code pin is aligned to 2.1.261 (Node 22 is required); its remote run and API-key
configuration remain separate evidence. The 2026-09-05 read-only preflight
found no repository Actions secret named `ANTHROPIC_API_KEY`; configure it in
GitHub before dispatching that job. Local OAuth credentials were not uploaded.

The retained v13 rehearsal is operator-assisted. Its initial execution stopped
on a conservative secret-scan finding in a runtime credential expression. The
first intervention renamed that local identifier to `bearer_value`, preserving
the function's AST apart from that identifier, then sent authenticated unblock
guidance. The action was named `unblock-raise-cap`, but the persisted
configuration remained at three; the action only changed milestone status and
guidance. A second authenticated message kept later test repairs focused without waiving failing
contracts or actual defects. These interventions and the original stopped log
are retained; neither the immutable fixture inputs nor its scanner policy was
changed during recovery. A later repair introduced a synthetic URL with a
non-example password and was stopped by the connection-string rule. A third
intervention changed only that password to `example-kranz-token`; the
normalized AST was otherwise identical and the native scan passed. The
second stopped log was preserved before authenticated resume, with corrected
guidance to retain the actual existing cycle cap of three. This does not count
as an unattended CI smoke pass.

The assisted mission `m-9473b9` completed both milestones with 1,864 events.
The separate final audit used the committed `9fed360` harness and its production
filesystem gate wrapper, and passed 82 fixture tests plus the live HTTP/CLI
and documentation checks. It verified the original two-milestone/five-feature
plan, unchanged immutable inputs, and that the authentication helper's first
change belongs to a completed repair feature. Both stopped log prefixes remain
byte-for-byte prefixes of the final authenticated log; replay matches cached
state. The primary branch and tracked source stayed at the pinned base.
Two historical checkpoint-failure statuses remain in the final state, with
retained work delivered by later features. The remaining minor guard-comment
finding was waived as nonblocking with its rationale recorded. These facts are
retained rather than presented as a clean first attempt.

| Check | Result |
| --- | --- |
| `cargo test --workspace --no-fail-fast`, including interruption, cost, recovery, redaction, versioned integrity, and durable receipt regressions | 2,809 passed, 0 failed, 7 ignored across 61 targets; exit 0 |
| Full macOS workspace suite through the production gate sandbox, including durable receipts | 2,809 passed, 0 failed, 7 ignored across 61 targets; wrapped command exit 0 in 364.0 seconds; nine explicit `SKIP-UNDER-WRAP` markers |
| Earlier Linux arm64 full workspace suite, Rust 1.94, clean container with standard toolchain homes | 2,751 passed, 0 failed, 6 ignored; exit 0; Git, bubblewrap, and grep required; precedes the cost, redaction, recovery, and versioned integrity fixes |
| Declared Rust 1.88.0 minimum, `cargo check --workspace --locked` and the new acceptance example in a separate build directory | Exit 0 on macOS arm64 |
| Required runtime capabilities | Git, Seatbelt, Keychain, and grep were required; unavailable capabilities could not silently pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | Exit 0 |
| `cargo fmt --all`, then `cargo fmt --all --check` | Exit 0 |
| `cargo build --workspace --locked` | Exit 0 |
| Strict workspace rustdoc | Exit 0 |
| Dashboard clean `npm ci`, audit, TypeScript, tests, build, embedded freshness, lint | 196 tests passed; all commands exit 0; npm reported no vulnerabilities |
| Standalone Tauri `cargo check --locked` | Exit 0 |
| Root `cargo audit --deny unsound` | Exit 0, no advisories or yanked-package warnings |
| Tauri `cargo audit --deny unsound`, from its own workspace | Exit 0 under the existing scoped `glib` exception; 16 unmaintained dependency warnings remain |
| `cargo deny check` | Advisories, bans, licenses, sources pass |
| Four crate package inventories | Reviewed: engine 162 entries, server 18, Slack 35, CLI 42; numeric reference fixture and embedded dashboard assets included; no runtime state or credentials |
| Engine crates.io publish dry-run with locked dependencies | Package verification passes; upload was not performed |
| Optimized CLI installation into an isolated installation prefix | `cargo install --path crates/cli --locked` succeeds |
| Installed-binary smoke with source-checkout reads denied | v0.2.0, help, idempotent init, readiness JSON, embedded HTML/assets, read/mutation authorization, 0600 tokens, and graceful token cleanup all pass |
| Acceptance harness offline regressions | Nine pass: valid isolated delivery with labeled/bare commit receipts, failed-run retention, bad CLI rejection, contract replacement rejection, three seeded-repair integrity cases, and generated-code containment |
| Assisted live mission and contained final acceptance audit | Both milestones complete; 82 tests and HTTP/CLI/docs pass; immutable inputs, repair provenance, authenticated replay, and both stopped-log prefixes verified |
| Public-tree and uncommitted-diff Gitleaks scans | Pass |
| Baseline full advertised-ref history audit | 1,239 commits scanned, no leaks; all branch, tag, and pull-request refs fetched |
| Candidate `117de53` advertised-ref history audit | 1,256 commits scanned, no leaks; exit 0; final documentation-commit receipts are retained separately |
| Native secret scan of `eea2e62..9fed360`, using the scanner and policy built from unchanged `eea2e62` | Pass; matches the trusted-base CI procedure |
| Domain lint of the candidate using the unchanged base scanner and policy | Pass, 771 tracked files |
| Knowledge refresh after updating linked verification dates | Pass |
| Version/changelog alignment | v0.2.0 check passes with remote-main check explicitly skipped for the unmerged candidate |

The macOS skip ledger records container fixtures not exercised on this host
because its VM does not share the system temporary directory, plus Linux-only
integration fixtures. The Linux container proves bubblewrap but has no nested
container runtime; its container-provider fixtures are explicitly skipped.
Windows and the CI Linux container-egress proof remain required remote gates.
The interruption test uses the POSIX shell's signal builtin so minimal Linux
hosts do not need a standalone `kill` executable.
The supported release artifacts remain the CLI and embedded dashboard; signed
Tauri desktop installers are outside this release.

## Release closure

This report is local candidate evidence, not a publication receipt. The final
commit must pass the required platform CI, including an unattended live smoke.
The local assisted rehearsal is complete; it does not replace that remote run.
The exact documentation-commit scan receipts are retained with the private
release evidence. Owner confidentiality/licensing review, the protected
release environment, anonymous public clone, and published artifact checks
remain open. Dependent crates.io dry-runs must wait for their sibling
0.2.0 versions to become resolvable, as documented in `docs/releasing.md`.

The 2026-09-05 remote check still found a private repository, unchanged origin
main at `eea2e62d8723d6e2ea3d59890c07820f17a6b36c`, no version tags, an active
strict main ruleset with eleven required checks and no bypass actors, and
`KRANZ_PUBLIC_RELEASE_ENABLED=false`. The `release` environment exists but has
no reviewer protection yet. GitHub makes required reviewers available only for
public repositories on its Free, Pro, and Team plans; protection must be
verified after the visibility change and before unlocking publication
([GitHub environment documentation](https://docs.github.com/en/rest/deployments/environments)).
No branch, tag, crate, release asset, or visibility change was published during
this local review. The operator steps in `docs/public-readiness.md` and
`docs/releasing.md` remain open.
