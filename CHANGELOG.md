# Changelog

Notable user-visible changes are documented here. This project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

- Fixed: the dashboard's one-call permission card stays mounted in place
  after its deadline passes (or after the engine closes it as expired),
  instead of unmounting and shifting the remaining panels and buttons
  underneath the cursor. The card's Allow/Deny buttons disable and a
  reserved status line reads "Expired — no answer was sent" with a Dismiss
  button to remove it. A card the operator already answered, or one closed
  for a reason other than its deadline passing, no longer gets mislabeled
  as expired.
- Fixed: the dashboard's Trajectory Board now pauses background polling and
  shows a "Polling paused — token required" status after an authentication
  failure, instead of parking a fresh token-prompt waiter on every poll
  tick; a "Resume polling" button re-tries once with the normal token gate
  and restarts the timers.
- Fixed: switching missions in the dashboard (back/forward, deep link, or URL
  edit) now clears the revision-request textarea, the per-question free-text
  drafts, the orchestrator composer's text and interrupt flag, and the
  standards waiver reason/expiry form, so a draft typed for one mission can
  no longer be submitted to a different one.
- Fixed: the dashboard's grant request panel now sends the `kind` and
  `requestSeq` of the exact pending request it rendered when approving or
  denying, so a stale or superseded decision is refused by the server
  instead of silently acting on a different request.
- Fixed: a Slack grant card's Approve and Deny buttons carry the sequence
  number and kind of the request the card was posted for. A click on a card
  for a request that has since been replaced is refused as superseded and
  queues nothing, even when the newer request has the same command text. A
  card posted before this change is refused the same way; use the newest
  card or the dashboard.
- Fixed: `POST /api/missions/:id/grant/approve` and `/grant/deny` accept
  optional `kind`/`requestSeq` fields and refuse the decision as superseded
  (409) when they no longer match the pending grant request, binding a
  dashboard/Slack decision to the exact request the operator saw rather than
  to any pending request sharing the same command text. Bodies without these
  fields keep today's behaviour.
- Added: `PendingGrantRequest.requestSeq` records the sequence number of the
  `grant.requested` event that parked the pending capability grant, so a
  later decision can be bound to the exact request the operator saw instead
  of matching on target text alone.
- Changed: Sentry signal intake derives its replay key only from the signed
  request body, so a delivery re-sent with a new `Request-ID`,
  `X-Sentry-Delivery` or timestamp header is refused as a replay. An accepted
  delivery is remembered for at least 24 hours on the server clock, bounded by
  the 4,096-entry in-memory cache; the 300 second timestamp window is a
  freshness check only. A restart or a full cache can shorten the window. See
  `docs/scoping/signal-intake.md`.
- Changed: `signals.sentry.apiBaseUrl`, `signals.sentry.tokenEnv` and
  `signals.sentry.allowedHosts` are settable from the global config layer
  only. A project `.kranz/config.json` that sets one is refused with an error
  naming the key, and the Sentry webhook for that repository refuses
  deliveries until it is removed. `signals.sentry.secret` stays settable per
  repository and merges with the global fields.
- Fixed: loose ends in tickets that `kranz ticket import-atlas` writes from a
  schema 3 bundle. Reference links are read from `groups.json`, counted and
  listed with caller, callee, the callee's ticket and the occurrences; a
  group whose only loose ends are reference links no longer says "none". The
  by-reference line fits a selection program as well as an expression and
  points to the group's `references_program` facts. Each loose end that is a
  fact carries its fact id, the kind of an external invocation reads as
  words ("OS command", not `os\_command`), and stored text such as a path is
  shown in a code span instead of backslash-escaped, with a note after the
  span when the text was cut at 80 characters or held a tab or line break.
  Reference links are listed ahead of the other loose ends. Entries with no
  fact id that read the same become one bullet with a count. Tickets
  imported from schema 1 and 2 bundles are unchanged. See `docs/tickets.md`.
- Changed: release builds no longer restore a Rust or npm cache
  (`.github/workflows/release.yml`), so every release artifact is built only
  from the tagged source; `scripts/check-release-workflow.py` guards this in
  CI's `supply-chain` job.
- Changed: `.github/actions-allowlist.json` now lists exact reviewed commits
  for every first- and third-party action (`github_owned_allowed` and
  `verified_allowed` both `false`, no wildcards); `scripts/check-actions-allowlist.py`
  no longer exempts `actions/*`/`github/*` at any commit or matches
  `patterns_allowed` entries by glob. An Actions update must add its new
  commit to the allowlist in the same PR, and the operator must re-apply the
  file to the repository's selected-actions setting after it changes.
- Changed: `kranz exec -f mission.md --push <remote>` now preflights the
  remote (configured, no armed local config, no `::` remote-helper URL, and a
  read-only `git ls-remote` reachability probe) before `MissionEngine::create`,
  so an unusable `--push` target is refused (exit 4, no mission created)
  before any spend rather than only after the mission runs.
- Changed: `kranz ticket import-atlas` reads Magic Atlas bundles of schema 3
  as well as 1 and 2. A schema 3 ticket says which of its `blocked-by` tickets
  the group reaches by reference only (a program an expression or an unbound
  variable names: a mention, not a call), lists the group's external
  invocations and uninterpreted operations with its other loose ends, and
  warns when the bundle was cut from an export that does not record such
  links, so its dependencies may be incomplete. Tickets imported from schema 1
  and 2 bundles are unchanged. See `docs/tickets.md`.
- Changed: a session that the model declines to carry out is recorded as a
  failed run, with a message in the event log that names the refusal and its
  category when the agent reports one. It was recorded before as a partial run
  with no report, which gave no reason for the missing report. This applies to
  backends whose result reports a stop reason, which today is Claude Code.
- Changed the orchestrator role prompt: it names the decisions that warrant
  blocking (a change to the contract, scope or cost, or something only the
  operator has) and the decisions inside an approved plan that need no
  go-ahead, and it tells the orchestrator to plan from the repository as it is
  and say so when a recorded lesson no longer matches it. The orchestrator
  prompt hash recorded on new sessions changes.
- Changed the worker role prompt: the turn limit is stated as the point where
  the engine ends the session; ordinary obstacles are to be worked through
  while the stops the prompt names are reported; the worker keeps the files its
  feature touches in good shape without changing behaviour the spec does not
  mention; and subagents are for work that splits into independent parts, not
  for reviewing the worker's own diff. The worker prompt hash recorded on new
  sessions changes.
- Wired `SgianTerminalProvider` automatically into `MissionEngine` and `runner`
  (`AcpBackend::for_mission_worker`) for `checkout`-isolated uncontained ACP
  workers when both an external `KRANZ_SGIAN_BIN` and `<repo>/.kranz/serve.grant.key`
  (`0600`) are present, binding each active worker `run_id`, skipping cleanly on
  external `worktree` checkouts outside Sgian's served root, and enforcing
  `SessionSpec.disallowed_tools` (`WORKER_DENY` and `AUTHORITY_DENY`, including
  shell `-c` subcommands and protected `.kranz` authority paths) plus
  ambiguous-display checks before presenting one-call operator consent.
- Added: `kranz queue --timing` reports how long each mission waited in the
  queue and how long it ran, which mission was running when it was queued,
  and how many approved plans declared write paths. Waiting time is recorded
  from now on; earlier missions show `-`.
- Changed: plan review in the dashboard, Slack plan cards, `plan.md` and CLI
  plan output now state when a plan declares no write paths. The planner's
  instructions now ask for `touchSet` on every plan, so expect more plans to
  declare one and more grant requests when a worker writes outside it.
- Changed: a merge no longer waits for a running mission, and a mission can
  start during a merge, when the mission works in its own worktree (the
  default). Merges still run one at a time, and a mission that works in the
  primary checkout still excludes a merge.
- Added `kranz ticket import-atlas <BUNDLE_DIR>`: imports a Magic Atlas
  program bundle as one ticket per group, with `blocked-by` following the
  bundle's group dependencies so missions run callee-first. The bundle is
  verified against `bundle.json` before anything is written, and each ticket
  carries an `mgatlas coverage` command as an acceptance hint that fails until
  every fact of the group is mapped or waived, with a second command that
  fails if `bundle.json` is no longer the file that was imported. See
  `docs/tickets.md`.
- Changed: every `kranz serve` response now forbids being shown inside a
  frame (`Content-Security-Policy: frame-ancestors 'none'`,
  `X-Frame-Options: DENY`) and sets `X-Content-Type-Options: nosniff`.
- Added `SgianTerminalProvider` (`kranz_acp::terminal::TerminalProvider`) behind
  `AcpBackend::with_sgian_terminals` for explicitly opted-in, uncontained host
  ACP sessions (`sandbox.enforce: off`, refused when combined with contained
  profiles): rotates `<repo>/.kranz/serve.grant.key` (`0600`, authority-denied,
  sandbox-masked, and scrubbed), mints RFC 2104 HMAC-SHA256-signed canonical JSON
  `ExecutionGrant`s, and routes `terminal/create|output|wait_for_exit|kill|release`
  through `sgian ctl --workspace <repo> --json grant` with one-call consent,
  scope/generation fencing, lifetime/retention budgets, verified
  `PaneObservation` projection, and fail-closed `CleanupReceipt` confirmation.
- Changed the project license from MIT to Apache 2.0. Releases up to and
  including 0.5.0 remain available under MIT. Contributions now require a
  Developer Certificate of Origin sign-off and carry the relicensing grant
  described in `CONTRIBUTING.md`.
- Changed: at most `hooks.ciDraftsPerHour` CI-failure tickets (default 3)
  are drafted per rolling hour; further failing runs get `429`. Raise it in
  `~/.kranz/config.json`; a repository's own config may only lower it.
- Changed: `gh pr create` from the dashboard runs with only the GitHub and
  network variables it needs, stops after 120 seconds, and is refused when
  the repository's git config names a program.
- Changed: the revision panel shows the diff above its buttons and enables
  Approve only once the diff has loaded.
- Changed: a Slack plan card for a mission already queued or started says so
  and offers no approve buttons. A card Slack rejects is replaced by a short
  notice pointing at the dashboard.
- Changed: workspace service names must be lowercase letters, digits, `-` or
  `_`, and cannot be `workspace`.
- Fixed: `POST /api/missions` now checks a patched `claudeBinary` the way
  the config files are checked.
- Fixed: the Gas City pack ignores receipts and state files that are tracked
  by git.
- Fixed: on Linux, sandboxed sessions can no longer see the Docker, Podman,
  containerd and system-bus sockets or the per-user runtime directory. Add a
  socket's directory to `sandbox.extraWrite` to give a tool access to it.
- Changed: a mission fails its final gate when a declared pty-script
  assertion failed in its most recent session. Before, only a missing
  transcript failed the gate.
- Fixed: stopping a session or command now also kills descendants that moved
  to their own session or process group, on macOS and Linux. A process that
  double-forks and lets its parent exit first is still not found.
- Fixed: the validator snapshot copies each untracked file through a single
  open that follows no symbolic link at the file or any directory above it.
- Changed: workspace bootstrap, readiness and data commands no longer get
  your ssh agent socket or global git config by default. Declare
  `SSH_AUTH_SOCK` or `GIT_CONFIG_GLOBAL` in the workspace contract's
  `secrets[]` and list the same name in `contractEnvPassthrough` to restore
  either. Without the git config these commands commit as `kranz gate`.
- Changed: contract-command and pty output shown to the functional validator
  is fenced as untrusted data, and the orchestrator's state digest keeps each
  item on one line.
- Changed: the plan review, ticket page, status strip and delivered panel
  show hidden and reordering characters as `\uXXXX` escapes, and the plan
  review says when a plan contains any.
- Changed: the box showing a command or invocation awaiting approval wraps
  and grows to its full height, with no scroll region, and long or padded
  text carries a size line.
- Changed: a Slack grant card offers Approve only for a short, unpadded
  target with no hidden characters; other grants are approved in the
  dashboard.
- Changed: a repository's own config file may set `orchestrator.backend`
  only to `claude`. Other orchestrator backends are chosen in
  `~/.kranz/config.json`.
- Changed: a Slack Queue button now queues only the drafted mission its card
  showed, and `kranz ticket queue --mission <id>` is refused when the ticket
  records a different mission. Both name the current mission in the refusal.
  A ticket re-drafted after the card was posted needs a fresh look first.
- Changed: a GitHub comment carrying the queue label (`kranz:fix-and-queue`)
  now queues the drafted plan only when `hooks.autoQueue` is `true` in the
  global config (`~/.kranz/config.json`). It is off by default and cannot be
  set from a repository's own config file. Without it the draft parks for
  review.
- Changed: merging a mission is refused with `base_policy_changed` when
  `.kranz/merge-gates.json` or `.kranz/secret-allowlist` on the base branch
  changed after the plan was approved. The dashboard offers an explicit
  accept step; API callers resend with the digest the refusal names.
- Changed: `kranz exec --push` scans everything the mission added for secrets
  before pushing, and refuses repository git config that would redirect or
  widen the push (`pushurl`, `push.followTags`, `push.pushOption` and
  similar).
- Changed: grants whose target would be enforced as a pattern (a written path
  containing glob syntax, a wildcard host) are no longer offered.
- Fixed: the secret scan skipped added lines that begin with `++`, and
  flagged ordinary prose after the words "basic" or "bearer".
- Fixed: `kranz config set` leaves the config file owner-only.

## 0.5.0 - 2026-11-08

- Added the Trajectory Board lens (`#/board` and `#/r/<repo>/board`) to the
  Mission Control dashboard: a figure-8 Apollo flight-plan 2D canvas plot,
  shared pending-ask predicates across mission panels and the fleet model, a
  deterministic scripted demo route (`#/board/demo`), and act-in-place beside
  the board reusing existing ask-owner panels without new mutation call sites.
- Retained deterministic review-effort pilot cases (`review_effort_pilot`
  example, verification harness, and execution receipts) pending human
  observations.
- Marked completed ACP qualification milestones on the roadmap and updated
  workspace, Tauri, dashboard, Even G2, and GitHub Actions dependencies with
  refreshed third-party license notices.

## 0.4.4 - 2026-11-07

- Serve the embedded dashboard by default. Filesystem dashboards require an
  explicit trusted override; pinned directory handles refuse symlinks in asset
  paths and SPA fallbacks, preventing reads outside the selected directory.
- Require read authentication for ordinary CLI serving, including loopback.
  Development browser origins require `--dev-origins`; desktop debug builds
  enable them explicitly. The health endpoint remains public.
- Apply the selected mission sandbox to local workspace bootstrap, readiness,
  clone, migrate, skew-check and reset commands. Refuse unsupported containment
  before execution. Container contracts with host-shell `disk.prune` commands
  are refused; owned Compose resource cleanup remains available.
- Include the TLS certificate and Tokio fixes already on main, and refresh
  both root-store dependencies to 1.0.11. Refresh dependency license notices.
- Backport the upstream GLib iterator pointer fix for the GTK 3 desktop stack,
  remove the unmaintained macro-error helpers, and verify the narrow vendored
  patches with source hashes, optimized iterator tests, compiler diagnostics
  tests and a full Linux desktop build check.

## 0.4.3 - 2026-10-18

- Hardened Git execution and network boundaries: disabled tag signing and GPG
  helper programs during engine-created tags, rejected leading-dash ref/tag
  names across Git operations, blocked `include.*`/`includeIf.*` and URL-scoped
  `http.<url>.*`/`remote.<name>.*` overrides before network calls, and refused
  remote-helper `::` URLs.
- Hardened sandbox, container, and validator containment: refused symlinked or
  hardlinked `target/` trees during validator snapshot warm-copy, created and
  masked absent `.git/hooks`, `.git/info`, and `.git/modules` directories under
  mount-based sandboxes, mounted common `.git` directories read-only for Docker
  worktrees, and resolved DNS targets in the egress proxy to reject loopback,
  unspecified, and link-local addresses unless explicitly allowlisted.
- Tightened URL, credential, and scrubber validation: enforced strict loopback
  URL parsing without backslashes or userinfo on `baseUrl` and hook-status
  endpoints, required `0700`/`0600` modes on isolated credential homes and
  hook-status specs, redacted full JSON secret values containing spaces, and
  prevented short placeholder words inside long alphanumeric tokens from
  suppressing secret detection.
- Hardened validator verdict parsing, live permissions, event-log high-water
  checks, `MissionHost::ask` tool/context isolation, and terminal/Markdown
  output sanitization across the CLI, server, and Slack bridge.
- Added ACP profile resource ceilings (R1), observed resource failure
  classification and session budgets (R2), and test-only qualification
  measurement scaffolding (R3), plus container socket/file denial guest receipts
  and hosted mission stack-headroom boxing.
- Updated OpenTelemetry to 0.33, `rustls` to 0.23.46, `quinn-proto` to 0.11.19,
  and dashboard/Even G2 dependencies; refreshed third-party license notices.

## 0.4.2 - 2026-09-26

- Made Sgian host-control credentials an explicit opt-in through an absolute
  helper path outside the checkout. Enforced sessions never receive them;
  bounded issuance and revocation now run off the async executor.
- Refuse invisible/control characters in live approvals and render them visibly
  in the CLI, Slack, dashboard and review packets. Deny rules now cover repeated
  wildcard suffixes, argument arrays, every supplied path and lexical traversal.
- Require external gate acceptance to cover changed source with clean tracked
  candidate bytes and normal index flags. Changed private or detected-secret
  files block acceptance; ordinary web framework and Unicode paths are supported.
- Persist bounded ACP progress batches even without permission requests, retain
  transport failures, and refuse completion with unanswered consent. Mount proofs
  run freshly for the current image and container starts use the owned ID.
- Check configured credentials across adjacent ACP text chunks; verify retained
  evidence digests before export redaction. Added offline adapter-lock receipt
  checks and refusal to run proof checkers under optimized Python.
- Keep feature-execution futures on the heap to avoid exhausting hosted mission
  runtime stacks; the full REST lifecycle has a bounded-stack regression check.


- Extract bounded ACP v1 framing and session protocol into `kranz-acp`, with a
  provider-free threaded consumer and shared conformance fixtures. Kranz retains
  permission authority, event normalization, credentials and process containment;
  filesystem, terminal and resume capabilities remain disabled.


## 0.4.1 - 2026-09-23

- Isolated expired ACP permission requests from sibling candidates, clarified
  which operator controls stop the active batch, and made fixed-profile egress
  refusals identify the grant mismatch without printing destinations.
- Filtered Docker egress now requires a recognized stable daemon version of at
  least 25.0.5 for the internal-network DNS prerequisite. Documented cache
  readability and default-bridge relay trust assumptions. Optional container
  tests distinguish an unavailable daemon from a missing CLI; explicitly
  requested proofs still fail closed.

- Updated dashboard and experimental Even G2 dependencies, rebuilt the embedded
  dashboard, and refreshed CodeQL action pins.

## 0.4.0 - 2026-09-22

- Added read-only recorded outcome reasons in the CLI, API, dashboard and
  evidence export. Explicit authentication blocks, checker findings and human
  boundaries remain distinct; ambiguous failures stay unknown. Activity-window
  counts preserve repair history and label overlapping mission populations.
  See [recorded outcome reasons](docs/outcome-reasons.md).

- Added opt-in baseline/candidate evidence using the existing contained control
  runner. Approved expectations, source and checker identities, test overlays,
  environment configuration and both receipts travel through advisory gate
  inputs, human review packets and digest-verified exports. Missing evidence
  stays unresolved; stale observations do not authorize reuse. See
  [baseline/candidate observations](docs/contract-controls.md#baseline-and-candidate-observations).

- Added a read-only human review packet in the CLI, dashboard and authenticated
  report view. It brings approved scope, candidate changes, check freshness,
  findings, explicit exceptions and pending decisions together, with references
  to their recorded evidence. Dirty edits make prior checks historical; missing
  evidence remains visible. Human packets stay outside validator inputs and
  use the existing decision controls. See [review packets](docs/review-packets.md).

- Worker runs in a repository served by a Sgian daemon now identify
  themselves there as `kranz:<run-id>`: the engine issues a write-scoped
  credential before the session, passes it as `SGIAN_CLIENT_TOKEN`, and
  attempts revocation when the run ends, including dropped worker futures. Silent when `sgian` or its daemon is absent;
  `KRANZ_SGIAN_BIN` overrides or disables the lane. See
  [Sgian coordination](docs/sgian-coordination.md). Control calls now use bounded
  process-tree supervision and an explicit environment allowlist; helper reply
  bytes are excluded from error logs. The shared scrubber recognizes Sgian client
  credentials in worker output and evidence. Failed revocation or engine death still
  requires operator reconciliation.

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
