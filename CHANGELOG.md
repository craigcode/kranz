# Changelog

Notable user-visible changes are documented here. This project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

## 0.2.0 - 2026-09-05

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
