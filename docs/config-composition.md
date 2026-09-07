# Config composition audit — extends-vs-replaces, bypasses, precedence

Ticket: `.kranz/tickets/config-fail-open-audit.md`. Source: the Warp Agent CLI
launch scan (2026-08-04), which shipped two live counterexamples —
`--auto-approve` bypassing the command denylist by default, and a
user-supplied denylist *replacing* the built-in one. This document is the
committed inventory of every kranz config/permission surface, swept for the
three footgun classes:

- **(a) replace-shaped lists** — a custom list that REPLACES safer defaults
  instead of extending them;
- **(b) silent deny bypasses** — a flag that short-circuits a deny/blocklist;
- **(c) wrong precedence** — allow/deny composition that is not deny-wins.

Verdicts per surface: **extends** / **replaces** for list composition, the
named bypass paths (or "none"), and the precedence direction. Every finding
is either FIXED or recorded below as an accepted exception with rationale —
none is left silent. The regression tests pinning each verdict carry the
`composition_audit` substring (`cargo test --workspace composition_audit`);
this audit found **zero unmitigated fail-opens** — each surface below links
its proving test.

## The two global composition rules

1. **Layered config arrays replace wholesale; safety floors are compiled
   in.** `config::deep_merge` merges objects key-wise and replaces scalars,
   arrays, and nulls across the defaults ← `~/.kranz/config.json` ←
   `<repo>/.kranz/config.json` layers (`crates/engine/src/config.rs:341`).
   That replace cannot strip a guardrail because every list whose floor
   matters is either a compiled-in constant the config only *extends at use
   time* (`WORKER_DENY`, `READ_ONLY_DENY`, `DEFAULT_EGRESS`) or empty by
   default (`denyPatterns`, `sandbox.extraWrite`, `sandbox.egress`,
   `allowValidatorCommands`, `contractEnvPassthrough`). A layer can only
   replace what another layer added — never the built-in floor. Pinned by
   `composition_audit_layered_deny_patterns_replace_but_never_strip_the_builtin_floor`
   (config.rs).
2. **The naming rule.** No flag short-circuits a deny list unless its name
   carries the `dangerously-` prefix. The prefix is the audit's escape
   valve: a dangerous flag may exist, but its name must say so. The full CLI
   surface is walked by
   `composition_audit_guard_weakening_flags_are_dangerously_prefixed_or_enumerated`
   (crates/cli/src/cli.rs), which fails on any new guard-weakening flag
   lacking the prefix or a recorded exception (the exceptions are F2/F5
   below).

## Surface inventory

### Reviewer independence (`reviewerIndependence`)

Set before creating a mission, for example:

```json
{
  "worker": {"backend": "codex", "model": "gpt-5.6-sol"},
  "validatorScrutiny": {"backend": "claude", "model": "opus"},
  "reviewerIndependence": {"scrutiny": true, "functional": false}
}
```

Both switches default to false. Each enabled role must use a known model family
different from **every worker attempt recorded in the mission**, including
repairs, failed attempts, parallel workers and discarded pool candidates. This
conservative scope also covers earlier milestones' influence. A mixed-family
worker pool may therefore require a third family for review.

The engine includes the policy in the plan preview and pins it in `plan.json`
and `plan.approved`. Runtime patches cannot change it; plan revisions must retain
it. The separate mission pin remains authoritative after config changes and
restart. A skipped required reviewer, unknown worker provenance or a same-family
resolved reviewer blocks the milestone with a durable explanation, before the
reviewer launches. Fix the pairing and rerun validation, or approve a new mission
with a different policy. There is no automatic waiver.

Every new `worker.spawned` event records the **resolved** backend and model.
The check runs after backend discovery/fallback and again for retry and local
PASS confirmation sessions. A missing Droid reviewer may fall back to Claude
only if Claude still differs from all recorded workers. A missing worker backend
that falls back to Claude is recorded as Claude, regardless of the original
configuration. Different versions, effort settings, CLIs or billing accounts
within one family do not count as independence.

The conservative catalog recognizes Claude, GPT, GLM and Kimi through supported
dispatch identifiers. Unmapped identifiers and automatic selection fail closed;
local endpoints and ACP cannot currently establish a family for this strict gate.
This records requested dispatch identity, not provider-side attestation or proof
of statistically independent errors. Existing containment requirements still
apply: the example keeps reviewers on Claude so the process wrapper is available.
No independence setting opts into uncontained validation.

Old plans and logs omit the optional fields and retain their prior behavior.
Regression coverage: `cargo test --workspace reviewer_independence`.

### 1. Role permission profiles and worker deny-rule grants (`crates/engine/src/permissions.rs`)

- **Composition: extends.** Config `denyPatterns` are appended to the
  compiled-in `WORKER_DENY` list (worker) and dedup'd; `READ_ONLY_DENY`
  (orchestrator, validators) has no config hook at all. `allowValidatorCommands`,
  plan `command_grants`, and `validatorFunctional.tools` add ALLOWS only.
  Tests: `composition_audit_config_deny_patterns_extend_never_replace_builtin_worker_deny`,
  `composition_audit_layered_deny_patterns_replace_but_never_strip_the_builtin_floor`.
- **Precedence: deny-wins.** Deny rules take precedence over allows in
  Claude Code, and a grant's allow pattern never removes the matching deny
  rule from the profile — a granted-but-still-denied command stays denied.
  Test: `composition_audit_grants_add_allows_without_lifting_deny`.
- **Bypass paths (named, all deliberate):**
  - `dangerouslyAllowAll` / `--dangerously-allow-all` → `bypassPermissions`
    for every role. Carries the prefix; loud stderr warning; never default.
    Test: `composition_audit_bypass_permissions_requires_the_dangerously_named_key`.
  - **WorkerDeny grants** (`deny_exceptions`): an operator-approved,
    exact-match lift of one named deny rule, recorded in the event log —
    a deliberate, auditable erosion, not a silent bypass. Approving one
    extends `deny_exceptions` ONLY (never `command_grants`/`touch_set`;
    reducer_test.rs pins the capability honesty).
  - The read-only profiles' `tools` field is deliberately disconnected from
    `SessionSpec` (module docs) — no composition hazard.

### 2. Sandbox enforce/extraWrite/egress + egress grants (`crates/engine/src/sandbox.rs`, `egress_proxy.rs`, `config.rs`)

- **enforce: fail-closed, no bypass.** Unsupported platform/backend pairs
  are rejected at `config::validate` (only the claude backend honors
  enforcement) and resolve to `UnsupportedWarn` → refuse at run time.
  Container `fs+net` with a non-empty egress list is advisory-only
  (proxy-env routing on the default bridge) and is REJECTED at validation;
  empty egress keeps the hard `--network none`. No flag weakens this;
  `sandbox.enforce=off` is the explicit, honest off state.
- **extraWrite: extends.** The writable floor (session cwd +
  session-private scratch) is not configurable away; `extraWrite` only adds.
  Test: `composition_audit_extra_write_extends_never_replaces_the_writable_floor`.
- **egress: extends.** `effective_egress` = `DEFAULT_EGRESS` (Anthropic
  endpoints) + configured `egress[]` + operator-approved `egress_grants`,
  trimmed and dedup'd. Grants fold in extend-only (`apply_egress_grants`;
  the reducer extends `egress_grants` and nothing else). Malformed
  allowlist entries fail closed at proxy start; a proxy that cannot bind
  fails the run closed rather than launch unenforced. Tests:
  `composition_audit_effective_egress_extends_never_replaces_the_default_floor`,
  `composition_audit_egress_grants_extend_the_allowlist_never_replace`.
- **Precedence: deny-wins.** SBPL is deny-default and denies take
  precedence over allows regardless of clause order (verified with
  sandbox-exec); the explicit deny sets — mission-metadata writes, authority
  reads (`serve.token`, `serve.read.token`, `config.json`,
  `domain-terms.local`, the `hook-status/` projection, the mission
  `control/` inbox, cargo credentials), shared cargo-cache writes,
  validator read-deny — survive
  EVERY allow, including an `extraWrite` broad enough to cover them.
  Test: `composition_audit_explicit_denies_survive_a_covering_extra_write_allow`.
  The bwrap argv composes the same denies as ro-bind/tmpfs masks.
- **Bypass paths:** none. Validator containment is mandatory regardless of
  `sandbox.enforce` (an uncontainable platform/backend FAILS CLOSED by
  default — 14th-pass reversal of the loud-degrade default; the
  `validatorAllowUncontainedDegrade` config flag is the explicit per-repo
  opt-in back to the loud per-round degrade).

### 3. Secret/domain allowlist waivers (`crates/engine/src/scrub.rs`, `domain_lint.rs`, `.kranz/secret-allowlist`, `.kranz/domain-allowlist`)

- **Composition: subtract-only over findings, per fingerprint.** A waiver
  is a sha256 fingerprint of one (rule, value) pair (domain lint:
  salt+term+path). `filter_allowed` drops exactly the waived fingerprints;
  no waiver shape disables a rule or a class. Test:
  `composition_audit_secret_allowlist_waives_one_fingerprint_never_a_rule`.
- **Precedence:** n/a (no allow/deny pair — the scan is the control, the
  waiver list the reviewed exception set).
- **Bypass paths:** the checkpoint scan reads the allowlist from the tree
  being committed (`git_ops.rs:617`) — see finding F3. The AUTHORITATIVE
  merge-time scan reads it from the live base SHA (`merge.rs:117`), so a
  mission-branch waiver never applies at the merge gate. The scrub layer
  itself is documented defense-in-depth behind the permission layer.

### 4. Merge-gates and workspace-contract validation (`crates/engine/src/merge_gate.rs`, `workspace_contract.rs`)

- **Merge gates: fail-closed, base-branch-owned.** The suite is read from
  the live base branch (a mission cannot weaken the checks judging its own
  diff). Unparseable, empty, or conditional-only suites are rejected; at
  least one unconditional gate is required; `whenPaths` spelling `.` and
  path escapes are refused. Test:
  `composition_audit_merge_gate_suite_fails_closed_on_every_weakening_shape`.
- **Workspace contract: missing ⇒ `Ok(None)`; present-but-invalid ⇒ fail
  closed.** Missing is the documented worktree-only posture — with no
  contract there are no bootstrap promises to weaken, so this is not a
  fail-open. Unknown `schemaVersion` (including absent ⇒ 0), secret-value
  shapes in the names-only list, and escaping mounts are all refused
  naming the violation; the runtime read comes from the committed base ref.
  Test: `composition_audit_workspace_contract_missing_is_none_and_invalid_fails_closed`.

### 5. `--allow-unvalidated` and every `--dangerously-*` flag (`crates/cli/src/cli.rs`, `exec.rs`)

- **`--dangerously-allow-all`**: permission-gating bypass; carries the
  prefix. **`--dangerously-steal-live-lock`**: steals the engine lock from
  a provably LIVE holder; carries the prefix; implies `--force-lock`.
- **`--force-lock`**: steals only from a holder provably DEAD — the
  liveness probe is fail-closed, so this is not a deny bypass. Accepted
  exception to the prefix rule (F5).
- **`--allow-unvalidated`** (exec, `KRANZ_ALLOW_UNVALIDATED=1`): lifts
  EXACTLY the unattended scrutiny floor — the refuse-to-run gate that keeps
  a headless `skipScrutiny` mission from passing its own tautological
  acceptance. Not a deny-list bypass; the name states what it permits.
  Accepted exception (F2). Test:
  `composition_audit_allow_unvalidated_lifts_only_the_unattended_scrutiny_floor`.
- **`skipScrutiny` / `skipFunctional`** (config): disable validator roles;
  loud names, default false, and the exec floor refuses unattended
  `skipScrutiny` without the acknowledgment above.
- **`ticket queue --force`**: skips blocked-by READINESS only; dependency
  cycles are never overridable. Accepted exception (F5).
- **`serve --insecure-lan`**: acknowledgment that ADDS token requirements
  on non-loopback binds (removes nothing); `--read-auth` forces the read
  token on loopback. Accepted exception (F5).
- Tripwire for future flags:
  `composition_audit_guard_weakening_flags_are_dangerously_prefixed_or_enumerated`
  walks the whole clap tree.

### 6. Slack spend allowlist (`crates/slack/src/config.rs`)

- **Composition: replace-by-deliberate-choice, guarded.** An empty
  `allowUsers` FAILS CLOSED for money-spending actions unless the operator
  sets `allowAllUsers: true` (a loud, explicit open-posture
  acknowledgment). Blank entries are dropped at resolve so a stray `""`
  can't wash a configured list into the empty shape; a blank/absent
  `user_id` is denied whenever a list is set. Per-repo lists
  (`merge_repo_allow_users`): a repo with NO list inherits the global one —
  enabling `host.repos` cannot fail-open spend locked at the global level —
  while a repo's own non-empty list replaces it (deliberate: that list is
  itself operator-authored). Finding F4. Test:
  `composition_audit_spend_allowlist_composes_fail_closed_in_every_direction`.
- **Enforcement:** `is_authorized` gates every money-spending and
  spend-adjacent bridge action (new / plan / approve / start / config /
  draft / steering); read-only actions never consult it.

### 7. Auto-approve-shaped paths in serve/exec (`crates/server/src/rest.rs`, `crates/cli/src/exec.rs`)

- **exec auto-approves the plan by design** (headless CI: plan file in,
  exit code out). Compensating controls: the unattended scrutiny floor
  (surface 5), underspecified/wrong-plan escalations exit 3 instead of
  approving, and `--yes` is a documented no-op — no hidden flag.
- **No REST endpoint auto-approves anything.** Approves are explicit
  operator POSTs gated by the mutation token (`x-kranz-token`), and each
  decision endpoint pre-validates the exact pending object (plan revision,
  grant command, question id + option) so a stale or absent target is a
  409, never a silently ignored 202; the engine re-validates at drain time.
- **`autoWork`** (config): drains the queue automatically — but queueing a
  mission is itself an operator act (draft `--yes` / `ticket queue`), so no
  unreviewed plan reaches a run through it. Default off.
- **Slack bridge**: approve/config buttons are spend-gated (surface 6); the
  approve flow re-checks the parked mission before enqueueing.

## Adjacent full-replace lists (recorded for completeness)

These custom lists REPLACE by design; both are validated fail-closed and
neither can strip a safety floor, so they are not findings:

- **`routing.taskClassRules` / `routing.patternRules`** (config.rs): a
  configured table replaces the literal task-class floor — but the omission
  direction is the SAFE side (any class the table doesn't name stays
  Frontier; a `local` route with no endpoint fails safe to Frontier). Shape
  validation (blank/duplicate classes/patterns) fails closed. The tracked,
  base-branch-owned `.kranz/routing-rules.json` file
  (`crates/engine/src/routing_rules.rs`, docs/routing-rules.md) populates the
  same table and supersedes this key wholesale when present — recorded on
  the mission's decision log, never a silent override.
- **`workerCandidates`** (config.rs): the COMPLETE worker backend list when
  non-empty — every candidate is validated by the same rules as the worker
  role (known backend, supported model pair, the worker model floor, the
  fail-closed sandbox pairs), and a single-entry pool is rejected as a
  roundabout `worker.backend`.
- **`contractEnvPassthrough`**: extends the cleared contract-command env
  with NAMED vars only; names colliding with managed keys are refused,
  values never logged. Extends-from-empty.

## Findings and dispositions

- **F1 — hook-guard fails open on guard error (ACCEPTED, documented).**
  `kranz hook-guard` exits 1 (non-blocking) when the guard itself fails —
  an unreadable spec or unparseable payload — so a broken guard never
  freezes a worker session (`crates/cli/src/hook_guard.rs`). The miss stays
  visible: a structured error record is appended, a notice lands in the
  transcript, and the authoritative engine-side out-of-contract sweep still
  judges the session. This is the sweep's only true fail-open, and it is a
  deliberate availability trade with a compensating control, documented at
  the site since KRZ-302. Disposition: accepted exception.
- **F2 — `--allow-unvalidated` lacks the `dangerously-` prefix (ACCEPTED).**
  It overrides a refuse-to-run meta-gate (the unattended scrutiny floor),
  not a deny list; it is scoped to headless `exec`; its name states exactly
  what it permits; and `skipScrutiny` itself remains an operator config
  decision. Pinned to exactly that blast radius by
  `composition_audit_allow_unvalidated_lifts_only_the_unattended_scrutiny_floor`.
  Disposition: accepted exception.
- **F3 — checkpoint secret scan honors mission-tree waivers (ACCEPTED,
  bounded).** The engine's commit-time scan reads `.kranz/secret-allowlist`
  from the tree being committed, so a mission could waive its own finding
  at checkpoint time. Bounded three ways: a waiver must name the exact
  (rule, value) fingerprint; the waiver edit is committed on the mission
  branch in plain sight; and the merge-time scan — the authoritative gate —
  reads the allowlist from the live base SHA, where mission-branch waivers
  do not exist. Disposition: accepted as defense-in-depth layering; the
  merge gate is the boundary. No code change warranted.
- **F4 — Slack per-repo allowlist replaces the global one (ACCEPTED,
  documented).** A non-empty per-repo `allowUsers` replaces the global list
  rather than unioning with it. The replace is deliberate (the repo list is
  operator-authored) and guarded (an empty repo list inherits the global
  one, so the widening cannot happen by omission). Disposition: accepted,
  semantics recorded above and pinned by test.
- **F5 — `--force-lock`, `ticket queue --force`, `serve --insecure-lan`
  lack the prefix (ACCEPTED).** None short-circuits a deny list: the lock
  steal is probe-gated fail-closed, the queue force skips readiness only
  (cycles never overridable), and the LAN flag adds requirements.
  Disposition: accepted exceptions, enumerated in the naming-rule tripwire
  test.
- **F6 — green audit on the three footgun classes.** No surface was found
  where a custom list replaces a safer compiled-in default, where a flag
  silently bypasses a deny/blocklist, or where allow beats deny. Every
  verdict above is pinned by a `composition_audit_*` regression test —
  including
  `composition_audit_config_deny_patterns_extend_never_replace_builtin_worker_deny`,
  which guards the most dangerous surface (the worker deny list) against a
  future replace-shaped regression.
