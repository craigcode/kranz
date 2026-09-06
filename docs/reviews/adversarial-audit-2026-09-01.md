# Adversarial code review and security audit of `main`

Date: 2026-09-01
Target: `craigcode/kranz` `main` at `33732c27` (merge of PR #29), read-only
worktree. No file in the audited tree was modified.
Method: seven parallel adversarial reviewers, one per attack surface, each
tracing inputs to sinks and citing `file:line`. Every CRITICAL and HIGH
finding below was then re-verified by hand against the cited source before
inclusion. Confidence is stated per finding: CONFIRMED means the full path
was traced in code; PLAUSIBLE means the mechanism is verified but the last
step (an OS behaviour, a third-party CLI's behaviour) was not executed.

Severity is calibrated to SECURITY.md: a bug that lets an agent process or
repository content bypass the consent surface (plan approval, command grants,
merge gate) or weaken an advertised sandbox is HIGH or CRITICAL. Gaps already
documented as out of scope are INFO.

## Outcome

The auth plane, the child-environment discipline, the CI supply chain, the
Tauri shell, and the Slack inbound path are well built and held up under
adversarial tracing. The weaknesses cluster in one shape: **the engine
performs privileged operations on files and trees the agent can write**, and
the consent substrate itself (control inbox, event log, repo-level config)
has no integrity mechanism. The OS sandbox is the only thing standing between
an agent and that substrate, and it defaults to `off`.

| Severity | Count | Themes |
|---|---|---|
| CRITICAL | 1 | Unauthenticated control inbox forges operator consent |
| HIGH | 14 | Repo-owned config as trust root, unhardened engine git, unbounded ambient env, no log integrity, sandbox profile gaps, terminal injection, verdict parsing |
| MEDIUM | 33 | Cross-tier drift, proxy auth, Slack escaping, token handling, exports |
| LOW | 26 | Hygiene, bounds, Windows parity |

Per surface:

| Surface | Reviewer scope | C / H / M / L |
|---|---|---|
| Server | `crates/server`, host bridge | 0 / 0 / 1 / 3 |
| Sandbox and egress | `sandbox*.rs`, `egress_proxy.rs`, containers, `unsafe` | 0 / 4 / 5 / 6 |
| Command exec and git | `command_exec.rs`, `git_ops.rs`, gates, hooks | 0 / 4 / 6 / 4 |
| Backends and credentials | `backend_*.rs`, `agent_env.rs`, `config.rs`, `scrub.rs` | 0 / 3 / 4 / 4 |
| Slack | `crates/slack` | 0 / 0 / 5 / 4 |
| State integrity | `event_log.rs`, `reducer.rs`, `control.rs`, `judgement.rs`, evidence | 1 / 10 / 10 / 6 |
| CLI, dashboard, supply chain | `crates/cli`, Tauri, CI, scripts | 0 / 1 / 3 / 6 |

Counts above are before deduplication. The consolidated list below merges
findings that several reviewers reached independently.

## CRITICAL

### C1. The control inbox is an unauthenticated filesystem channel that forges operator consent

Confidence: CONFIRMED. Reached independently by the state and exec reviewers.

- `crates/engine/src/control.rs:88-110` drains `.kranz/missions/<id>/control/*.json`
  with no origin, ownership, or signature check.
- `crates/engine/src/orchestrator.rs:2847-2937` applies `approve-grant`,
  `approve-revision`, `answer-question`, and `config-change` from it.
- `crates/engine/src/config.rs:363-373` validates a `config-change` patch by
  range only. There is no allowlist of patchable keys; `dangerouslyAllowAll`,
  `skipScrutiny`, `skipFunctional`, `denyPatterns`, and
  `validatorAllowUncontainedDegrade` all merge through.
- `crates/engine/src/types.rs:1067-1075`: `SandboxEnforce` defaults to `Off`.

Attacker: a worker, validator, or any gate command with ordinary write access
to the repo. Under the default posture there is no sandbox, so writing one
JSON file into `control/` is within any agent's normal file-write ability.
The resulting `grant.approved` event is indistinguishable from a human
approval. The code already names this boundary (`sandbox.rs:769-772`) but it
exists only when the optional sandbox is on.

Fix: authenticate control files with a per-mission key kept in the set the
sandbox already read-denies, HMAC the canonical body, quarantine failures to
`.bad`. Separately, give `apply_validated_patch` an allowlist of runtime
patchable keys; consent-bearing keys belong at plan-approval time.

## HIGH

### H1. `<repo>/.kranz/config.json` is the winning config layer and can name the binary kranz executes

Confidence: CONFIRMED. Reached independently by the backends and exec reviewers.

- `crates/engine/src/config.rs:288-295`: global then project layer, later wins,
  no provenance distinction.
- `claudeBinary` flows to `ClaudeBackend::discover` (`crates/cli/src/commands.rs:622`,
  `crates/cli/src/ready.rs:708`) and becomes candidate one
  (`backend_claude.rs:176-180`). `validate` never inspects it; relative paths
  resolve against cwd.
- The same layer sets `contractEnvPassthrough`, `packDir`, `worker.baseUrl`,
  `acpCommand`, `dangerouslyAllowAll`, `validatorAllowUncontainedDegrade`.

Attacker: anyone whose repo the operator runs `kranz ready`, `kranz plan`, or
`kranz exec` in. Code execution as the operator before any sandbox or approval,
with the operator's full environment (see H5). The codebase pins
`routing-rules.json` to the base branch and classes packs by trust, so this
is the one repo-owned surface without a trust rule.

Fix: split `MissionConfig` into operator-only keys (settable from
`~/.kranz/config.json` only) and repo-settable preferences; refuse loudly
naming the key and layer. Minimum: reject a relative `claudeBinary` or one
inside the repo tree.

### H2. The same file is read-denied to sandboxed sessions but not write-denied

Confidence: CONFIRMED. Reached independently by the sandbox, exec, and backends reviewers.

- `crates/engine/src/sandbox.rs:760-767`: the write allowlist is session cwd,
  tmpdir, and extras; nothing under `.kranz/` is subtracted except the
  mission's own metadata files (`:793-815`).
- `crates/engine/src/sandbox.rs:874-890` names `config.json` as authority
  material for reads only.

Under `workerIsolation: "checkout"` the session cwd is the repo root, so a
contained worker can overwrite the trust root without reading it. The running
mission is immune (config is pinned into `MissionCreated`), the next mission
picks it up. Same asymmetry on Linux (bwrap masks close writes only for files
present at spawn) and on Windows (which is actually correct here).

Fix: reuse `authority_read_deny_paths` for `(deny file-write* ...)` on all
three tiers; consider denying writes to all of `<repo>/.kranz/` except the
session's own scratch.

### H3. Engine-side git runs unhardened in the tree the worker controls

Confidence: CONFIRMED for checkout mode; PLAUSIBLE for worktree mode via gitlink rewrite.

- `crates/engine/src/git_ops.rs:199-212`: `with_hooks_disabled` is opt-in per
  handle. It is applied at six sites (`merge.rs`, `validator_snapshot.rs`,
  `validator_integrity.rs`, two post-run inspections).
- `crates/engine/src/orchestrator.rs:4451` opens the integration worktree
  handle unhardened; `:4135` runs `commit_dirty_paths` on it after every
  feature; `:3359` makes that tree the worker's cwd.
- `.git` is not in `mission_write_denies`, so the sandbox permits the write.

Attacker: a worker plants `.git/hooks/pre-commit` or `core.fsmonitor`. The
next checkpoint commit runs it outside every sandbox with the engine's full
environment. `push_mission_branch` in `crates/cli/src/exec.rs:427` has the
same exposure via `pre-push` and `core.sshCommand`.

Fix: invert the opt-in so every engine `GitRepo` is hardened by default; deny
`<session_cwd>/.git` writes in the profiles.

### H4. Workspace bootstrap, readiness, and data-hook commands run with the engine's full ambient environment and no sandbox

Confidence: CONFIRMED.

- `crates/engine/src/workspace_gate.rs:196-218` → `command_exec.rs:237-253`
  → `run_shell_command_with_timeout_env(..., clear_env = false)`.
- The doc comment justifies this by the contract's `secrets[]` list, but
  nothing filters to that list; `contract.secrets` is only used by the remote
  provider.

Attacker: a hostile clone shipping `.kranz/workspace.json` (runs at mission
start, before any agent), or a merged worker commit editing it (affects every
subsequent mission). Strictly weaker than the contract-command path in the
same binary.

Fix: build the env the way `agent_env::contract_command_env` does and route
through `run_shell_command_sandboxed` with the mission's `GateSandbox`.

### H5. Binary probes spawn with the full ambient environment

Confidence: CONFIRMED.

`crates/engine/src/backend_probe.rs:18-26` and
`backend_readiness.rs:461-470` run `<binary> --version` without `env_clear`.
Every session spawn honours `agent_env`; the discovery probes are the one
exception. Combined with H1 or a PATH shadow, a repo-named binary receives
every operator credential on first invocation.

Fix: probe with `agent_env::sanitized_child_env` over a throwaway home.

### H6. No integrity mechanism on `events.jsonl`; forged appends and rollback truncation both go undetected, and the log is a live gate input

Confidence: CONFIRMED. State reviewer H1, H6, H9.

- `crates/engine/src/event_log.rs:501-565` checks JSON validity, sequence
  contiguity, and mission id. No MAC, no chained hash, no high-water mark.
- `orchestrator.rs:551-556`: resume folds the file as truth and overwrites
  `state.json` with the result (`:633`).
- `reducer.rs:50-90`: `PlanApproved` has no status guard.
- Three gate decisions read the log back mid-run (`orchestrator.rs:6753`,
  `:6403`, `:5231`), so this is a live bypass, not only an audit concern.
- Evidence bundles ship the raw log with a determinism claim that makes a
  forged log reproducibly self-consistent (`evidence_bundle.rs:648`).

Truncating at a line boundary silently rolls a mission back past a
`grant.denied` or `milestone.failed`. Appending a `grant.requested` and
`grant.approved` pair satisfies the reducer's forgery check.

Fix: hash-chain each line and MAC per mission with a key outside the
sandbox's readable set; verify in `parse_log_bytes` so every reader
inherits it; cross-check `state.json.last_seq` at resume.

### H7. macOS gate profile grants write and `file-ioctl` on the operator's own terminal device

Confidence: CONFIRMED for the grant; PLAUSIBLE that `TIOCSTI` yields host command execution.

`crates/engine/src/command_exec.rs:635-636` allows read, write, and ioctl on
`^/dev/tty[p-t][0-9a-f]+$` for every wrapped gate. On macOS that pool is the
terminal pool: a Terminal.app session is `/dev/ttys003`. The comment above the
grant names TIOCSTI as the thing the scoping prevents, but the regex includes
the operator's tty. The existing tests lock in the shape.

Fix: allocate the pty in the parent and grant only the specific slave path,
or emit an explicit deny for `ttyname(0)`.

### H8. bubblewrap unshares only the network namespace, and only under `fs+net`

Confidence: CONFIRMED for the flag set; PLAUSIBLE for `/proc/<pid>/environ` and TIOCSTI variants.

`crates/engine/src/sandbox.rs:1262-1288`: no `--unshare-pid`, `--unshare-ipc`,
`--new-session`, no seccomp. Host `/proc` is visible, so the contained agent
can read the engine's `environ` on `ptrace_scope=0` distros, keeps the
controlling terminal, and can signal the engine.

Fix: `--unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup --new-session`
unconditionally.

### H9. Agent-authored text reaches the operator's terminal with escape sequences intact, including on the approval screen

Confidence: CONFIRMED. Reached independently by the surface and exec reviewers.

- `crates/cli/src/output.rs:1049-1057`: `one_line` collapses whitespace only;
  ESC, BEL, and C1 controls pass through.
- `output.rs:138-206` (`render_plan`) prints spec lines and command strings
  raw; `commands.rs:1089` prints it, `:1101` asks `approve? [y/N]`.
- `scrub.rs` performs no control-character filtering (read every rule).

An orchestrator whose planning context was poisoned can print one plan and
have the terminal show another (`ESC[nA` + `ESC[K`), or write the clipboard
(OSC 52). In a governance harness this is a bypass of the primary control.

Fix: one `sanitize_untrusted` in `output.rs` applied inside `one_line` and to
every model-originated field of `render_plan` and `render_status`.

### H10. Verdict parsing: greedy brace span and first-verdict-wins on duplicate ids

Confidence: CONFIRMED for both code paths; PLAUSIBLE for exploitation of the first.

- `crates/engine/src/runner.rs:538-550`: `parse_report` tries whole text,
  then first `{` to last `}`, then fenced block. A JSON object the model
  quotes and disowns is taken as the answer.
- `crates/engine/src/judgement.rs:184`: `find` on verdict id; a reply with
  `[{"id":"a-1","pass":true},{"id":"a-1","pass":false}]` passes. The sibling
  `strict_standards_reports` 180 lines below implements the correct
  cardinality rule.

Fix: for decision turns, require the JSON to be the whole reply or the sole
fenced block; lift the cardinality match into `judge_contract_assertions`.

### H11. Sandbox metadata write-deny is scoped to the session's own mission

Confidence: CONFIRMED.

`sandbox.rs:800-815` derives every deny from `inputs.mission_dir`. Under
checkout mode sibling mission dirs, `.kranz/queue/`, `.kranz/tickets/`, and
`.kranz/lessons/` are writable. A mission-A worker forges approvals in
mission B (see C1, H6). Module docs say the mission dir is never worker
writable.

Fix: derive denies from `<repo>/.kranz` as a subpath, carving the session's
scratch back in.

### H12. Container tier mounts the operator's whole `$CARGO_HOME`, credentials included

Confidence: CONFIRMED.

`crates/engine/src/sandbox_container.rs:598-628` mounts `$CARGO_HOME` ro
with an `-e` for session mode; only gate mode carves the credential-bearing
root out. The process tier explicitly denies `credentials.toml`. Tier 3 is
therefore weaker than tier 2 for registry credentials, contradicting its
"stronger containment" description.

### H13. Codex scratch-HOME seed writes `auth.json` world-readable in shared `/tmp` (Linux)

Confidence: CONFIRMED.

`crates/engine/src/backend_codex.rs:113-126` streams the operator's
`~/.codex/auth.json` into `create_new` with no mode, so it lands `0644` under
`0755` parents in `temp_dir()`. Every other seeding site uses `fs::copy`,
which carries the mode. macOS is contained by the per-user temp dir.

Fix: `.mode(0o600)` plus `set_permissions`; `0700` on the scratch root.

### H14. Provenance checks that verify a reference while reading mutable bytes

Confidence: CONFIRMED. State reviewer H5, H7, H8, H10.

- Evidence bundle artefact bytes are read raw with no scrubbing
  (`evidence_bundle.rs:255-268`) while `summary.md` claims scrubbing.
- Lesson provenance uses `git log --diff-filter=A` (who added the path) but
  reads content from the working tree (`judgement.rs:435-451`,
  `lessons.rs:221-231`).
- `review_artifact` source immutability is a path-string match a rename
  defeats (`review_artifact.rs:253-266`).
- Ticket prose can supply a `## Task class` heading the engine only
  overrides when frontmatter set one, dropping standards rules from the
  approval pin (`ticket.rs:586-614`).

## MEDIUM

Grouped by theme. Full detail with evidence and fix for each is in the
per-surface reports listed under "Method" at the end.

**Consent and permission edges**
- ACP permission seam fails open when the peer omits the tool-call subject
  (`backend_acp.rs:298-360`): `wildcard_match("git push*", "")` is false, so no
  deny fires and the decision is `Allow`. No OS sandbox sits behind ACP.
- Worker-reported `commandsRun` become `Bash(<cmd>*)` allow rules for the
  functional validator with no human step (`orchestrator.rs:8280-8303`,
  `permissions.rs:205-284`).
- Grants never expire and the grant-request budget resets on every resume
  (`reducer.rs:156-174`, `orchestrator.rs:640-650`).
- At the final gate the same model judges agent-judgement assertions and can
  then waive its own findings; `protected_subjects` covers only two classes
  (`judgement.rs:186-205`, `orchestrator.rs:6841-6894`).
- Slack: Approve buttons carry only the mission id, so a stale card commits
  whatever plan is parked now (`format.rs:614`, `approve_flow.rs:50-65`).
  Grant, revision, and question buttons are correctly bound.
- Merge-gate suite and pack gates are second-order writable; pack gates are
  read unpinned from the working tree.
- `.kranz/queue/` entries are unauthenticated files (`queue.rs:118-151`).
- Ticket markdown is unauthenticated tree data interpolated raw into the
  planning prompt, and ticket frontmatter has no key allowlist;
  `max-budget-usd` is unclamped (`ticket.rs`).

**Cross-tier drift in containment**
- Seatbelt egress proxy is unauthenticated on loopback while every `fs+net`
  sandbox may reach all of loopback; the authenticated constructor exists and
  is used only for containers (`egress_proxy.rs:265-281`, `sandbox.rs:1148`).
- Worker container gets none of the relay's hardening (`--user`,
  `--cap-drop ALL`, `no-new-privileges`, `--pids-limit`)
  (`sandbox_container.rs:497-504` vs `container_egress.rs:246-322`).
- Container authority masks are a hand-copied three-name list, missing
  `domain-terms.local`, `hook-status/`, and `control/`
  (`sandbox_container.rs:545-556`).
- `workspace.json` `mounts[]` is an unbounded rw host-path grant
  (`workspace_container.rs:368-372`).
- AppContainer launch plan lives in a directory the contained principal
  gets `rwx` on.
- Sandboxed sessions and gates retain unrestricted read of the operator's
  credential files beyond the named deny set; the `fs` tier keeps full egress.
- `open_read_nofollow` degrades to check-then-open off unix
  (`paths.rs`), so artefact reads can escape the mission dir on Windows.

**Token and credential handling**
- Server leaf-file reads (`plan.json`, `plan.md`, `report.md`, transcripts)
  use `read_to_string` and follow symlinks, while the directory walk is
  no-follow (`rest.rs:1084-1097`, `:1255-1267`). Under checkout mode a session
  symlinks `plan.md` to `serve.token` and reads it tokenless over loopback.
- The mutation token is passed to `open`/`xdg-open` as argv
  (`commands.rs:2426`, `:2858-2888`), visible in `ps`.
- `http://tauri.localhost` is an unconditional CORS and WebSocket origin
  exemption unpinned to IP or port (`server/src/lib.rs:588-591`). Material on
  hosts where an unprivileged process can bind loopback:80.
- `kranz config show --global` prints Slack tokens verbatim
  (`config_cmd.rs:136-143`).
- Local backend `baseUrl` accepts any host; the engine POSTs prompts to it
  from outside the sandbox, and readiness probes any host:port
  (`config.rs:610-640`, `backend_readiness.rs:329-381`). Contrast
  `hookStatus.endpoint`, which is loopback-gated.

**Output and export**
- OTEL export bypasses the scrubber and has no length cap
  (`cli/src/otel/map.rs`).
- Six Slack builders post agent-authored text as unescaped mrkdwn
  (`<!channel>`, `<@U…>`, `<url|label>` all live) (`format.rs:823-924`,
  `:1162`, `:1182`; `commands.rs:268-282`). `slack-commands.md:112-118`
  claims every untrusted field is escaped.
- The Slack read-only surface is ungated and unscoped to the configured
  channel; catalog mode reaches any repo from any channel.
- Slack outbound failures retry at 2 Hz forever with no backoff.
- `export-traces --out` and `export-corpus --out` write through bare
  `std::fs::write`.
- `backend_readiness` and `pr_handoff` trust `state.json` directly instead of
  folding the log.
- A one-byte corruption in any mission log silently erases that mission from
  every cross-mission fold.
- TOCTOU on the evidence bundle's output anchor.
- `changed_paths` and `remote_has_branch` parse git output in spoofable ways;
  the quoting case silently skips path-scoped gates.

**Release**
- crates.io releases are published by hand from a workstation with a
  long-lived token, outside the attested `release.yml` pipeline. `cargo
  install kranz --locked` is the headline install.

## LOW

Backends: scrub rule gaps and an allowlist that suppresses findings; atomic
write temp files at default perms; unbounded ACP text; scratch HOMEs under
`0755` `/tmp` dirs. Sandbox: relay host proxy binds `0.0.0.0`; remote
workspace token over plain HTTP and across redirects; no macOS
die-with-parent; mount-proof cache never invalidated; `F_SETFL` discards
flags; mount specs unvalidated for `:`. Server: Tauri shell puts the mutation
token in the WS URL and never uses the read token; blocking fs and git work on
the async runtime; unbounded transcript read. Slack: dedup lost on restart;
`SlackConfig` derives `Debug` with tokens; arbitrary `model` string; no
`team_id` check. State: duplicate frontmatter `state:` keys resolve differently in
two readers; ticket body steers which knowledge notes are injected and is
re-read after approval; `chain.json` and `manifest.json` disagree; unknown
final event type dropped; out-of-range score silences a flag; non-unix lock
recovery. Exec: orphaned gate descendants on success; inconsistent `-` guards
on refs; hook-gate spec lives in session-writable scratch; predictable
worktree paths. Surface: `audit-public-tree.sh` and `audit-public-history.sh`
check far less than their names suggest; PR URL rendered as `href` without
scheme check; init script injects the token into every document; no security
headers on the browser-served dashboard; internal mission artifacts committed.

## Verified good

These were traced adversarially and held. Keep them.

- **Server auth plane.** Per-serve uuid tokens at `0600`, removed on
  shutdown; constant-time compare; read token structurally refused on
  mutations; Origin allowlist pinned to bound IP and port; Host gate closes
  DNS rebinding. All 40 routes traced; none mutates without the mutation
  token, a per-repo HMAC, or a per-run capability token.
- **Slack inbound.** Socket Mode only, so no signing-secret code to get wrong.
  All 34 `Action` variants traced; every mutating arm checks `is_authorized`
  on stable user id and fails closed on an empty allowlist. No token in a URL
  or log.
- **Child environment.** Every session spawn is `env_clear`'d from a locked
  allowlist with exactly one auth var injected. Contract commands likewise.
- **Fail-closed sandbox resolution.** Every resolver refuses rather than
  degrades; no path reports "sandboxed" while running unsandboxed. SBPL
  escaping, bwrap arg injection, egress allowlist and CONNECT parsing, and all
  21 non-Windows `unsafe` FFI sites checked clean.
- **Event log mechanics.** Torn-tail repair, seq contiguity, per-line mission
  id, generation-fenced single-writer lock, no-follow path pinning throughout.
  The prior review's F1, F2, F3 are verifiably fixed.
- **Merge path and validator containment.** Validated commit hash is the one
  merged; merge and snapshot handles are hooks-disabled.
- **No shell interpolation.** The one `sh -c` passes the command as a single
  argv element; `git_ops.rs` is argv-only.
- **Plan trust boundary.** Contract commands execute from state folded from
  the log, never re-read from a worker-writable `plan.json`; merge-gate and
  workspace contracts are read from the base branch.
- **CI supply chain.** Every action SHA-pinned; `pull_request_target` used
  only in a trusted-base pattern that never runs proposed code; digest-pinned
  Docker bases; checksum-verified tool downloads; build provenance
  attestation on binaries, SHA256SUMS, and SBOM.
- **Tauri shell.** Real CSP without `unsafe-inline` scripts, `core:default`
  capabilities only, no global Tauri, no remote IPC access, a markdown
  renderer that emits React nodes and no links.
- **TLS.** `danger_accept_invalid_certs` appears nowhere; response bodies
  bounded.

## Unverified and worth one experiment each

- Whether `TIOCSTI` on a `/dev/ttysNNN` opened under the gate profile pushes
  input into the operator's shell on current macOS (H7).
- Whether rewriting a worktree's `.git` gitlink redirects the engine's next
  `git status`/`commit` to attacker config and hooks (H3, worktree mode).
- Whether the Claude Code CLI honours a repo-level `.claude/settings.json`
  `PreToolUse` hook in a `-p` session with a fresh scratch `HOME`. If it does,
  a repo-supplied hook executes outside the permission system entirely and
  this is HIGH; the fix is `--strict-mcp-config` plus pinned settings sources
  (`backend_claude.rs:355-433`).
- Whether `pack/resolution.rs` pins gate command bytes or only a reference;
  if a reference, pack gates are a gate-command injection in the same class
  as C1.

## Documentation contradicted by code

- `docs/knowledge/surfaces/slack-commands.md:112-118`: escaping is not
  applied to every untrusted field.
- `crates/engine/src/sandbox.rs:18-21`: the mission dir is worker-writable
  for sibling missions under checkout mode.
- Every evidence bundle's `summary.md`: artefact bytes are not scrubbed.
- `crates/cli/src/ready.rs:3-5`: `kranz ready` is not read-only when a repo
  names `claudeBinary`.
- `crates/engine/src/sandbox.rs:476-478`: the container tier is not the
  stronger containment for registry credentials.

## Recommended order of remediation

1. **Authenticate the consent substrate.** HMAC control files (C1), allowlist
   patchable config keys (C1), hash-chain and MAC `events.jsonl` with the key
   outside the sandbox read set (H6), and cross-check `last_seq` at resume.
2. **Establish a trust rule for repo-owned config.** Operator-only keys
   settable from the global layer only; reject relative or in-repo
   `claudeBinary` (H1). Write-deny `.kranz/` authority files on all tiers
   (H2, H11).
3. **Make hardened the default for engine-side execution.** Invert
   `with_hooks_disabled` opt-in (H3); clear the env and wrap workspace gates
   (H4) and binary probes (H5).
4. **Close the profile gaps.** Specific pty slave instead of a device-class
   regex (H7); full namespace set for bwrap (H8); gate-style `CARGO_HOME`
   handling for sessions (H12); `0600` on the codex seed (H13).
5. **Sanitize what the human sees.** Control-character stripping at the CLI
   trust boundary (H9); `escape_mrkdwn` on the six Slack sinks; bind Approve
   buttons to a plan revision.
6. **Tighten verdict parsing.** Whole-reply or sole-fence JSON; cardinality
   match on ids (H10).
7. Then the MEDIUM cross-tier drift items, most of which are one-line
   reuses of a helper that already exists in the neighbouring tier.

## Remediation status (2026-09-03)

Landed on `main` in the follow-up PR from branch
`fix/adversarial-audit-2026-09-01`. Each item names the mechanism and the
test that locks it in. A second adversarial review of the fixes themselves
(source reports `review-consent.md` and `review-surface.md`, kept beside the
audit reports) found further gaps; those fixed in the same PR are listed
under the item they refine, and the rest are under "Residuals".

### Consent substrate

- **C1, closed.** Every control file carries `sig`, an HMAC-SHA256 under a
  per-repository authority key over the mission id, the file name, and the
  canonical command JSON. `drain` and `peek_interrupt` verify with a
  constant-time compare and quarantine unsigned, wrongly signed, or replayed
  files to `.bad`. The key lives at `<global kranz dir>/keys/<sha256(repo)>.key`
  (0600, dir 0700), outside the repository, inside the sandbox's read and
  write deny sets, and behind the agent CLI's own `Read`/`Edit`/`Write` deny
  rules for that directory in both `~` and absolute form. Replay is closed by
  a control mark: `acknowledge` records the last acknowledged file name
  outside the repository and any later file at or below it is refused.
  Runtime config patches from the inbox go through `PatchSource::Inbox`,
  which refuses consent-bearing keys. Tests: `control.rs` unit tests
  (unsigned, wrong key, cross-mission, replay, moved body) and
  `config_trust_test.rs`.
- **H6, closed as far as a same-uid attacker without access to the global
  kranz dir.** Every `events.jsonl` line carries `h` (sha256 chain over the
  previous hash and the canonical event) and `m` (HMAC of `h` under the
  key); readers verify both. A seal floor recorded at first sealed acquire
  makes stripping the fields detectable, and a high-water mark recorded after
  every durable append makes truncation detectable independently of the
  in-repo `state.json`. `acquire` refuses, rather than degrades, when the key
  cannot be read or minted. `PlanApproved` only applies from the planning
  status. Tests: `event_log_test.rs` (forged append, in-place rewrite, chain
  drop, wrong key, missing MAC above the floor, truncation with and without a
  snapshot, unreadable key) and `reducer_test.rs`.
- **H11, closed.** Sibling mission dirs, `.kranz/queue`, `tickets`,
  `lessons`, `hook-status`, and every top-level `.kranz` entry except the
  session's own mission are write-denied on all three tiers.

### Repository-owned inputs

- **H1, closed.** `PROJECT_LAYER_REFUSED` in `config.rs` refuses, naming the
  key and the file, any project-layer attempt to set `claudeBinary`,
  `packDir`, `contractEnvPassthrough`, `dangerouslyAllowAll`,
  `validatorAllowUncontainedDegrade`, `allowValidatorCommands`,
  `localBackendAllowedHosts`, `slack`, `hookStatus`, `workspace.remote`,
  `<role>.acpCommand`, `<role>.acpArgs`, `<role>.baseUrl`, `<role>.tools`,
  `skipScrutiny`, `skipFunctional`, `denyPatterns`,
  `allowBelowDefaultWorkerModel`, `workerIsolation`, or a lowered
  `<role>.sandbox.enforce`. A relative or in-repo `claudeBinary` is refused
  in any layer. Local-backend `baseUrl` must be loopback unless the operator
  allowlists the host. `kranz config show --global` redacts secrets.
- **H2, closed.** The authority read-deny set is also write-denied on all
  three tiers; the container tier keeps write-denied content readable through
  read-only self-binds rather than masks.
- **Exec I1, confirmed and closed.** A repository's `.claude/settings.json`
  SessionStart hook and a repository `.mcp.json` server both executed in a
  `claude -p` session with a fresh HOME (Claude Code 2.1.220). Every Claude
  session now launches with `--setting-sources user`, which suppressed both
  in the same experiment. Test: `backend_claude_test.rs`.

### Engine-side execution

- **H3, closed.** `GitRepo::open` is hardened by default (`open_unhardened`
  has no engine caller); the exec-disable set covers hooks, fsmonitor,
  attributes, filter drivers, gpg, `credential.helper`, `core.sshCommand`,
  `core.askPass`, editors, `uploadpack.packObjectsHook`,
  `protocol.ext.allow=never`, and enumerated `remote.*.uploadpack` and
  `receivepack`. Network operations keep the operator's global git config
  and refuse when the repository's own config carries a credential helper,
  ssh command, URL rewrite, proxy, or transport hook. User and system config
  are nulled through an empty regular file, not a device path. The sandbox
  write-denies `.git/config`, `.git/hooks`, `.git/info`, `.git/modules`, and
  the gitlink. Tests: `git_ops_test.rs`, `merge_test.rs`, `exec_test.rs`.
- **H4, credential half closed; sandbox half open.** Workspace bootstrap,
  readiness, data-hook, and `disk.prune` commands run from a cleared
  environment. A contract-declared secret crosses only when the operator's
  `contractEnvPassthrough` also names it (two-party consent); a fixed
  operational allowlist (`SSH_AUTH_SOCK`, proxy and CA variables) and the
  operator's global git config path cross unconditionally. The commands are
  still not wrapped in the gate sandbox; see Residuals.
- **H5, closed.** Version and login probes spawn from a cleared environment
  carrying only PATH and the backend's own auth variable.

### Sandbox profiles

- **H7, partially closed.** Gate and session profiles deny read, write, and
  ioctl on the operator's controlling terminal, resolved from the engine's
  own stdio. The device-class grant remains for the harness's pty pair, so
  other terminals the operator owns are not denied, and an engine started
  with piped stdio denies none; see Residuals.
- **H8, closed.** bubblewrap unshares pid, ipc, uts, and cgroup (try)
  unconditionally and starts a new session; network stays tier-gated.
- **H12, closed.** Container sessions mount only `bin`, `registry`, and
  `git` under the Cargo root, never `credentials.toml`.
- **H13, closed.** The codex seed is created 0600 under a 0700 scratch root.

### What the human sees, and what the model decides

- **H9, closed.** `output.rs::sanitize_untrusted` strips C0 and C1
  controls, whole ESC-introduced sequences, bidi and zero-width characters,
  and line separators, replacing each with a visible marker and bounding the
  scan so an unterminated OSC cannot hide the rest of a field. Applied to
  every model-originated field of the plan, status, tail, and backlog
  renderers.
- **H10, closed.** Decision turns parse through `parse_decision`: the whole
  reply, or a single fenced block that is the last content of the reply,
  with fence state tracked per line. Duplicate verdict ids fail the
  assertion. Worker-reported `commandsRun` are prompt text, never validator
  allow rules. ACP denies when the subject is missing or the kind is
  unclassifiable in a read-only session.
- **Slack M1 and M2, closed.** Every agent-authored field is escaped before
  clipping, including the plan-review alternatives and ticket views. Approve
  buttons carry `<mission-id>:<plan-identity>`; the host commits only when
  the parked plan's identity matches, under one lock.

### Provenance and reads

- **H14, closed.** Evidence bundles scrub artefact bytes and hash the
  scrubbed form. Lesson provenance requires the worktree bytes to match the
  committed blob at HEAD through git's own comparison. Review-artifact
  immutability compares the source blob at base and head. Ticket task class
  comes only from frontmatter. Server, `pr_handoff`, and ask-context file
  reads are no-follow. Exports refuse to write through a symlink. OTEL
  attributes are scrubbed and bounded. The mutation token is no longer
  passed to `open`.

### Residuals

Recorded here so they are not mistaken for closed.

- **Everything reduces to the integrity of the global kranz dir.** An
  attacker who can write there can zero the key, delete the seal floor and
  the marks, and rewrite a log keyless. Under `sandbox.enforce = off` the
  only barrier is the agent CLI's permission rules; the codex and droid
  backends do not consume them at all, and ACP matches subjects verbatim. Set
  `enforce` to `fs` or better for any repository you did not author.
- **H4 sandbox half.** Workspace gate commands run env-cleared but
  unsandboxed; wrapping them needs the mission's `GateSandbox` threaded
  through `WorkspaceHandle`.
- **H7.** The pty device-class grant should become a parent-allocated
  slave; until then only the operator's own controlling tty is denied.
- **Mid-run gate reads** of the event log are not rollback-checked; only
  resume is.
- **`GitRepo::open`** enumerates drivers and remote programs on every open
  (two git processes); cache if it shows up in profiles.
- **Upgrade hazard.** A pre-seal binary appending to a mission after a
  sealed binary recorded its floor makes the log unreadable with no
  recovery command. Stop every kranz process before upgrading. A
  `kranz mission reseal` escape hatch is the missing tool.
- **`parse_decision`** still accepts a fenced object the model disowns in
  the lead-in prose when it is the reply's last content; the prompts forbid
  trailing prose, so this is the shape they teach.
- **Verification is path-dependent.** A log read from outside the
  `<repo>/.kranz/missions/<id>/` layout is checked by chain only, and says
  so at `warn`.
- **Windows** parity for the empty-config file, the write-deny ACLs, and
  the container `--user` mapping is proven by CI, not by a local run.

## Method

Reviewers ran in parallel against a detached worktree at `33732c27`, each
with a scoped file list and an instruction to trace rather than pattern-match,
to state CONFIRMED only after a full path trace, and to record areas checked
with no finding. Their full reports, with per-finding evidence quotes, fix
sketches, and test-coverage notes, are the source for this document and are kept alongside it in
`docs/reviews/adversarial-audit-2026-09-01/`:

- `audit-server.md` (server crate and host bridge)
- `audit-sandbox.md` (sandbox profiles, egress, containers, `unsafe`)
- `audit-exec.md` (command execution, git, gates, hooks)
- `audit-backends.md` (backends, environment, scrubbing, config)
- `audit-slack.md` (Slack bridge)
- `audit-state.md` (event log, reducer, control, judgement, evidence)
- `audit-surface.md` (CLI, Tauri dashboard, CI, scripts, release)

Hand re-verification covered every CRITICAL and HIGH entry above: each cited
line was read in the audited tree and the quoted code matched.
