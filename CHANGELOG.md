# Changelog

Notable user-visible changes are documented here. This project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

- Worker runs in a repository served by a Sgian daemon now identify
  themselves there as `kranz:<run-id>`: the engine issues a write-scoped
  credential before the session, passes it as `SGIAN_CLIENT_TOKEN`, and
  revokes it when the run ends. Silent when `sgian` or its daemon is absent;
  `KRANZ_SGIAN_BIN` overrides or disables the lane. See
  [Sgian coordination](docs/sgian-coordination.md).

## 0.3.0 - 2026-09-20

- Added opt-in, qualified Claude and Codex ACP workers in pinned Linux ARM64
  Docker images on macOS/Linux ARM64 hosts. Profiles pin adapter versions,
  credential channels and egress; unsupported combinations fail before spawn.
  Existing worker defaults remain unchanged. See [ACP containment](docs/acp-containment.md)
  for the tested host/runtime combinations and setup requirements.
- Added durable one-call ACP permission decisions through the existing operator
  surfaces. Requests bind to the exact invocation, expire safely and cannot
  become session-wide or persistent grants; stale decisions cannot authorize a
  later command.
- Added content-pinned external gate evaluators with bounded JSON-RPC stdio,
  isolated evidence inputs and retained results. Initial/revised plan approval,
  milestone validation, final checks and local merge now consume these checks
  on macOS/Linux through the existing authority rules. External command-permission
  evaluators remain unsupported; the one-call consent broker owns that stage.
  Evaluator results never replace required human consent or waive engine prohibitions.
- Added stage evidence and audit exports that bind decisions to approved inputs,
  observed checks and the exact integration tree. Missing retained artifacts
  remain explicitly unresolved after cleanup; exports never replay effects.
- Hardened ACP protocol limits, cancellation, process supervision and container
  cleanup. Namespace leases bound execution after engine death, orphan processes
  are reaped without losing adapter exit status, and uncertain cleanup fails
  visibly. Container mount probes now have their own ownership and recovery
  records.
- Prevented ACP recovery ledgers from retaining session credentials, excluded
  nested private paths and PEM private keys from evaluator inputs, and corrected
  startup and multi-check gate deadlines.
- Added deterministic hostile-process and governed-mission proofs, plus bounded
  live Claude/Codex worker receipts. Live runs used scripted controllers and
  reviewers; they do not establish live model judgment or universal adapter
  compatibility. No ACP filesystem/terminal provider or Windows ACP containment
  is included.
- Fixed a Windows contract-lint timing fixture that depended on a network timeout.

## 0.2.3 - 2026-09-14

- Security: updated rustls from 0.23.44 to 0.23.45 for
  [RUSTSEC-2026-0285 / GHSA-2mjx-qc3c-rqvc](https://github.com/rustls/rustls/security/advisories/GHSA-2mjx-qc3c-rqvc),
  which corrects acceptance of TLS 1.3 handshake messages across encryption
  level boundaries. Both dependency lockfiles and bundled notices include the
  fixed version. Existing v0.2.2 installations need this rebuilt release.
- Added a reusable manual check that verifies release-archive provenance and
  executes the matching binary on all five supported native platforms.
- Added reviewed website guides for first missions, review, validation
  evidence, and optional Slack setup. Scoped the remaining ACP and external
  gate work; those roadmap items are not new runtime capabilities in 0.2.3.

## 0.2.2 - 2026-09-13

- Includes the server security fixes prepared for the unpublished v0.2.1
  candidate below.
- Fixed release verification on Linux: install and probe bubblewrap, enable
  the required user namespaces on the ephemeral runner, and use the same
  linker and disk preparation as regular CI. Enforced gates remain mandatory.
- Added a manual release rehearsal that verifies source and builds every
  platform archive before tagging; manual runs cannot publish a release.

## 0.2.1 - 2026-09-13

Tagged candidate only; no GitHub binaries or crates.io packages were published.
Release verification stopped because its runner lacked bubblewrap. The public
tag is retained, and v0.2.2 carries the corrected release workflow.

- Security: non-JSON POST bodies with no Content-Length, including chunked
  requests, now receive HTTP 415. Known-empty requests remain supported.
- Security: GitHub webhook and hook-status authentication exceptions belong
  only to their registered POST routes; similar path suffixes stay protected.
- Security: gated reads and WebSocket upgrades no longer accept mutation
  tokens in query strings. Native clients can continue using x-kranz-token.
  The dashboard exchanges its header credential at GET /api/read-token and
  sends only read authority in WebSocket URLs, including after reconnect.
  Custom browser clients must use a read token or adopt that exchange.
- Fixed: `serve` rejects empty, malformed, or duplicated read credentials
  before startup, so printed and stored read tokens match the active server.
- Added a security-review index linking historical findings, remediation,
  regression evidence, and recorded limitations.

## 0.2.0 - 2026-09-13

- Fixed Windows validator snapshots failing to replay uncommitted edits when
  mission roots use canonical paths.
- Added an experimental Even Realities G2 thin client (`apps/even-g2`) that
  renders mission status from the existing REST API and lets an operator
  approve or deny a pending grant or pick a structured question's answer,
  each behind a review screen and a separate confirmation tap. Development
  sideload only; the physical-device receipt is tracked in the
  `even-realities-g2-demo` ticket.
- Optional reviewer-independence requirements are pinned at plan approval and
  checked against recorded worker model families after backend resolution,
  fallback and retry. Unknown identities or skipped required reviewers block.
- Run records and provenance exports now identify the resolved backend;
  fallback no longer leaves the requested backend in new run evidence.

- Security: Linux sandbox private-workspace rebinds preserve Git metadata
  and shared-cache write protections while keeping authority files hidden.
- Fixed PTY validation cancellation to finish target process-group cleanup
  before releasing the mission lock. Waiting for output or blocked terminal
  input no longer delays cancellation until the script deadline.
- Fixed Linux sandbox startup racing with removal of ordinary temporary files
  from private directory views. Disappearing entries stay hidden; authority
  masks and read-only restrictions remain mandatory.
- Fixed PTY validation waiting for a timeout after its target exited. Parent
  terminal handles now close after spawning, and child programs inherit only
  the intended standard terminal streams. Continuous output yields to session
  deadline checks instead of keeping the drain loop running indefinitely.
- Fixed Windows AppContainer launches refusing every worktree after authority
  hardening. The isolated worktree's `.kranz` namespace is protected separately,
  including future files and directory renames; ordinary source files remain
  writable and removable. Protected DACL inheritance and retained handles keep
  broad LPAC grants out of that namespace and prevent Git-pointer replacement;
  overlapping launches restore the original ACLs and inheritance settings.
- Fixed retries losing earlier checkpoint commit receipts. Sequential features
  now persist their baseline and cumulative receipts before retry or resume;
  failed features with retained work cannot be mistaken for empty proposals.
- Fixed fractional costs producing event lines that failed their own integrity
  checks. New versioned seals preserve numeric bits; valid legacy seals keep
  their original interpretation and can be continued without rewriting them.
  Older binaries cannot verify the new seals.
- Fixed resume discarding uncommitted integration repairs. Retained worktrees
  are validated and reused; unexpected repositories, branches, and symlinks
  are refused without deleting their contents.
- Fixed secret redaction corrupting escaped JSON decisions and transcripts.
  Redaction preserves JSON structure and duplicate-key rejection.
- Fixed secret-rule labels being mistaken for bearer credentials and hiding
  source paths in redacted finding diagnostics.
- Fixed full-file Python secret scans interpreting conditional-block colons as
  credential assignments; following real credentials remain detectable.
- Fixed double-counted Claude streaming costs: new result events carry each
  turn's incremental cost while retaining the provider's raw cumulative value.
  Existing event logs are not rewritten; their historical estimates may be inflated.
- Fixed Ctrl-C handling for `exec`, `run`, and `work`: native agent sessions
  terminate their process groups when cancelled, while mission logs remain
  available for resume.
- Sandboxed workers now stop retrying denied Git metadata writes and leave
  tested changes for the engine's reviewed, secret-scanned checkpoint.
- Fixed the acceptance rehearsal to verify the delivered branch in a detached
  worktree and to support an existing authenticated Claude Code installation.
  It now preserves failed fixtures for resume and independently verifies the
  final CLI against a fixed HTTP contract.
- Security: webhook comment triggers require an explicit GitHub user allowlist;
  workflow failure triggers refuse fork-owned branches.
- Security: engine diffs disable external programs and text converters, custom
  merge drivers fail closed, and protected Git handles refresh after workers.
- Security: sandbox authority directories hide future and rotated credentials,
  sibling mission metadata stays protected, and validator cache copies refuse
  links and special files.
- Fixed macOS Cursor private Keychain seeding, legacy migration, lock/unlock
  verification, and rejection of injected passphrase commands.
- Fixed container gate launch and timeout cleanup to preserve the host runtime
  context without forwarding ambient secrets or client proxy credentials.
- Updated the yanked `chacha20` dependency to 0.10.2 in both lockfiles.

- Security: authenticated the mission control inbox and the event log so an
  agent process with repository write access can no longer forge operator
  consent, roll a mission back past a denial, or patch consent-bearing config
  at runtime (adversarial audit 2026-09-01, C1 and H6).
- Security: repository-provided `.kranz/config.json` can no longer name the
  agent binary, an ACP program, a pack directory, an ambient-secret
  passthrough, a remote model endpoint, or a containment escape; those keys are
  operator-only. The sandbox now write-denies the authority files it already
  read-denied, on all three tiers (H1, H2, H11).
- Security: engine-side git handles are hooks-disabled by default, workspace
  bootstrap commands and backend version probes run with a cleared
  environment, and the macOS gate profile no longer grants the operator's own
  terminal device (H3, H4, H5, H7). bubblewrap sessions now unshare pid, ipc,
  uts, and cgroup namespaces and start a new session (H8).
- Security: those git handles also neutralize the credential helper, ssh
  command, editors, and transport hooks a repository's own config can name.
  Network operations (`kranz exec --push`, the mission-branch probe) keep the
  operator's global git config in force, so a credential helper, an
  `insteadOf` rewrite, or an `http.proxy` in `~/.gitconfig` still applies —
  and they refuse to run at all when the repository's own config (including a
  linked worktree's `config.worktree`) carries a credential helper, an ssh
  command, a URL rewrite, or a transport hook, naming every offending key.
  Remove such a key from `.git/config` to push.
- Security: Claude sessions launch with `--setting-sources user`, so a
  repository's `.claude/settings.json` hooks and `.mcp.json` servers no longer
  execute inside worker sessions (verified against Claude Code 2.1.220).
- Security: agent-authored text is stripped of terminal control sequences
  before it reaches the operator's terminal, including the plan approval
  screen; Slack posts escape every agent-authored field (H9).
- Security: validator verdict parsing requires the JSON to be the whole reply
  or the sole fenced block, and duplicate verdict ids fail the assertion
  (H10). Container sessions no longer mount the credential-bearing Cargo
  root; the codex scratch credential seed is created 0600 (H12, H13).
- Security: evidence bundles scrub artefact bytes, lesson and review-artifact
  provenance compare committed bytes rather than the working tree, ticket
  prose cannot set the task class, and server file reads refuse symlinks
  (H14).
- Security: the global kranz directory now honours `KRANZ_HOME`; the
  authority key, seal floors, high-water marks, and control marks live
  under it, outside every repository, and keys minted for temporary
  repositories are pruned automatically.
- Security: workspace gate commands see a contract-declared secret only when
  the operator's `contractEnvPassthrough` also names it; container sessions
  run as the operator's uid with all capabilities dropped and a pids limit.
- Upgrade note: stop every running kranz process before installing this
  version. The first sealed acquire records a seal floor for each mission, and
  a pre-seal binary appending above it makes that mission's log unreadable.
- Added `docs/reviews/adversarial-audit-2026-09-01.md`, the consolidated
  audit with the seven per-surface source reports and the two follow-up
  reviews of the fixes beside it.


- Prepared the repository's security, contribution, release, and supply-chain
  controls for public distribution.
- Expanded Kranz from its original three CLI integrations to additional
  worker and validation backends, including Kimi, Cursor, ACP, and local
  OpenAI-compatible inference.
- Hardened validator isolation, gate-command sandboxing, credential handling,
  mission-path filesystem operations, authorization, and audit evidence.
- Added first-class macOS, Linux, Docker, and Windows AppContainer containment
  evidence, including fail-closed validation and gate execution.
- Added multi-repository hosting, dashboard routing, Slack operation, knowledge
  refresh, release evidence, and clean public-distribution automation.

## 0.1.0 - 2026-07-04

Private preview. The historical binaries predate substantial security and
correctness hardening and are not a supported public distribution.
