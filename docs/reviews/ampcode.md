# Review: Amp (ampcode.com) vs kranz (2026-07-27)

Amp is the agentic coding product spun out of Sourcegraph as an independent
company on 2025-12-02 (Amp Inc. / "Amp Frontier Corporation," Quinn Slack
leading; profitability is their own claim, unverified). Since the spin-out it
has moved fast toward exactly the territory adjacent to kranz: hosted remote
machines for unsupervised agents ("orbs," Jun 2026), event-driven agent
wake-ups, agent-to-agent spawning across machines, a Slack control surface, a
meta-agent ("Puck"), co-driven threads ("Multiplayer"), and subscriptions
(Jul 2026). One verdict per idea below, then ticket-ready drafts.

Method note: reviewed via the live site, the owner's manual
(ampcode.com/manual and subpages), the full news archive late-2025→Jul-2026,
security/pricing pages, and third-party coverage, all on 2026-07-27. Amp
deletes and renames features weekly by explicit policy and its model lineup
churns monthly — every claim below is dated; expect drift. Third-party
adoption figures (e.g. "40k teams") were contradicted by primary sources and
are not used.

---

## Positioning read (the headline)

Amp is the clearest embodiment yet of the *opposite* philosophy on every axis
kranz treats as identity:

| Axis | Amp (by policy) | kranz (by invariant) |
|------|-----------------|----------------------|
| Tool execution | default-allow, no approval prompts; policy delegated to plugins | consent gates, contract-scoped grants |
| Delivery | "Ship (direct to main)" is a first-class workflow | never pushes; human merges; scoped `kranz/*` handoff only |
| Plan | none — prompt and go | approved plan.json pinned at base_sha |
| History | threads; features deleted without backcompat | append-only event log; contract files additive-only |
| Autonomy | longer leashes, less handholding; safeguards for weaker models deliberately removed | enforcement rigor as f(blast radius, autonomy); fail-closed sandbox |

This *strengthens* the roadmap's Cursor-era positioning correction rather than
threatening it. The polished autonomous-agent-platform lane is now contested
by Amp, Cursor, and Factory simultaneously; kranz's lane — git-native mission
recorder, consent gate, and validation harness around headless agents —
remains uncontested, and Amp's own 2026 walk-backs are evidence the gap is
real: they killed publicly discoverable thread sharing because threads proved
too hard to review for leaked sensitive content (Jun 2026), added passkey
step-up auth for remote-controlling agents ("Proof of Human," May 2026), and
bolted on best-effort secret redaction. Safety backfilled after autonomy.
Kranz builds the gates first; that ordering is the product.

The kill-features ethos deserves one more line of contrast: Amp can delete
handoff, forking, Tab, TODOs, toolboxes, and its editor extension because a
thread is a disposable artifact. Kranz's event logs, plans, and reports are
durable audit records — additive-only is not conservatism, it is what an
audit record means.

---

## Verdicts

### 1. Cross-vendor writer↔reviewer pairing — **ADOPT (cheap, now)**

Amp's capability dial (Jul 2026) pairs every tier's writing model with an
"oracle" reviewer from a *different vendor* — ultra writes with Claude
Fable 5 and reviews with GPT-5.6; high is the inverse — explicitly for
cross-model perspective. Kranz roles already make this *possible* (worker and
scrutiny roles pin models independently across backend_claude/codex/droid);
what's missing is the policy: **scrutiny validators should default to a
different provider pool than the worker that produced the diff**, with a
config warning when they share one. Same-model review inherits same-model
blind spots. Dovetails with the provider-pool declarations already sketched
in `backend-quota-breaker-reroute`
(docs/reviews/local-llm-and-triumvirate.md §7). Draft ticket below.

### 2. Repo-owned declarative review checks — **ADOPT (best single borrow)**

Amp's code review runs user-defined checks from `.agents/checks/` — each a
markdown file with YAML frontmatter (name, description, default severity,
tool restrictions), one subagent per check, composable from CLI or thread
(Feb 2026). This is the merge-gates idea extended from commands to
LLM-judged review dimensions, and kranz already has every ingredient: a
base-branch-owned tracked gate file whose judged mission cannot weaken it,
scrutiny validators, a findings/waiver flow. A tracked `.kranz/checks/`
directory of repo-specific scrutiny checks (e.g. "no new dependencies
without justification," "public API changes need a docs commit") consumed by
the scrutiny pass gives repos control of *judgment*, not just *commands* —
the same per-repo story M8 built for gates. Draft ticket below.

### 3. Workload identity instead of injected secrets — **ADOPT (record now, build with the remote provider)**

Orbs mint short-lived OIDC JWTs carrying workspace/user/thread claims
(`amp orb id-token --audience …`, Jul 2026); services trust the issuer and
grant access without any secret ever entering the workspace. This is the
natural completion of workspace-contract D-A's "secret *names*, never
values": the best secret is one that doesn't exist even as a name.
Mission-scoped identity (claims: repo, mission id, profile) is the right
shape for `workspace-remote-coder-provider` and for M6's "auth grows up."
Not buildable locally today; record it in
docs/scoping/workspace-contract.md's secrets story so the remote provider
ticket inherits it.

### 4. Step-up auth on spend verbs — **ADOPT (M6-gated)**

"Proof of Human": passkey re-authentication required for sensitive
operations (remote-controlling a thread, admin actions), enforceable
org-wide — protection against a compromised account driving autonomous
agents. Kranz's per-serve session token is right for localhost; a *hosted*
`kranz serve` (M6 shape 2) should support WebAuthn step-up on
approve/start/queue — the spend-adjacent verbs the Slack allowlist already
singles out. One line in the M6 auth story now; implementation with the
hosted milestone.

### 5. Amp CLI as a candidate backend — **PARK (behind the Cursor-backend proof bar)**

Amp has a headless execute mode (`amp -x`, stdin piping), newline-delimited
JSON streaming with usage fields including cache tokens, env-var API-key
auth, and a TypeScript/Python SDK with a permission-callback hook. As a
`backend_amp` it would buy multi-model access (GPT-5.x, GLM, Claude) through
one seam — the same appeal as the roadmap's `backend_cursor` note, and the
same bar applies: prove report parsing, model/cost capture, worktree cwd
discipline, permission mapping, and the no-push/no-main-write invariants
first. Two Amp-specific cautions: its trust-by-default posture means the
policy surface kranz maps onto is thinner (tool-disable globs and plugin
hooks, not a first-class permission mode), and its weekly feature-deletion
policy makes any integration seam unstable. Note beside backend_cursor;
no ticket.

### 6. Supersede-aware retrieval — **ADVISORY (repo-knowledge lane)**

Amp's thread-reading subagent carries an explicit heuristic: don't stop at
the first relevant hit; check for newer messages that revise, supersede,
revert, or contradict it (Jul 2026). Directly applicable to
`repo-knowledge-ranked-brief-injection` and transcript search: recency-aware,
contradiction-aware selection beats first-match relevance for exactly the
artifacts kranz injects (decisions get revised; lessons get superseded).

### 7. Runaway rate as a first-class metric — **ADOPT (small, calibration lane)**

When Amp swapped Gemini 3 Pro out for Opus 4.5 after one week (Nov 2025),
the deciding evals were not just quality: "off-the-rails cost" — the share
of threads degenerating into runaway error loops — was 2.4% vs 17.8%, at
equal average thread cost. Tail risk, not averages, drove the decision.
Kranz's report.md records cost vs estimate; add a mechanical *runaway*
classification (final cost > k× estimate, or terminated on budget/failure
after N repeated identical errors) so per-backend/per-model tail behavior
accumulates in calibration data. Draft ticket below.

### 8. Orbs validate the workspace contract — **ADOPTED ALREADY (validated, three details worth stealing)**

The orb lifecycle matches accepted D-A…D-H nearly field-for-field:
`.agents/setup`/`.agents/resume` hooks ≈ `bootstrap[]`; `.amp/services.yaml`
with runtime-assigned `$PORT`/`$PUBLIC_URL` ≈ `services[]`/`previews[]`;
15-minute auto-pause with per-minute billing ≈ `workspace-idle-hibernate`;
a shared terminal into the agent's filesystem ≈ D-E takeover artifacts. An
independent, shipping product converging on the same contract shape is the
strongest validation the M6 backlog has had. Steal three details when the
remote provider lands: (a) preview portals are **authenticated by default**
— which is M6's "leaked URL reveals nothing" done properly; (b) the resume
hook carries an explicit timeout (10s) — bounded resume belongs in
`workspace-contract-schema` alongside readiness; (c) pause/per-minute
economics are a provider-selection criterion, not an afterthought.

### 9. Event-driven agents and self-scheduling — **VALIDATION for D-F, gate kept**

Orbs now wake on webhooks (CI failure, issue trackers, monitors) with
plugin handlers filtering payloads, and agents set their own recurring
wake-ups. The demand signal for `trigger-ci-pr-fix-mission` is confirmed,
and their webhook mechanics (durable URLs, at-least-once delivery, explicit
handler timeout and rate caps) are good reference plumbing. The boundary
stands: kranz triggers create audited tickets/missions behind consent —
agent-self-scheduling is precisely the unaudited prompt-loop shape D-F
rejects.

### 10. Context economics — **VALIDATION (two numbers to reuse)**

Amp's measured case for lazy-loading MCP servers inside skills: one
DevTools server = 26 tools = 17k tokens = ~10% of a frontier context
window; skill-wrapped it loads 4 tools at 1.5k tokens. That is the
quantified argument for the backlog's SessionSpec `tools` restriction item.
Their broader arc — compaction-first, handoff removed, one-thread-per-task
doctrine — is the shape kranz already has structurally: feature-scoped
worker sessions with contract-pinned scope instead of one long thread.

### 11. Transcript exposure and redaction — **VALIDATION (kranz ahead)**

Amp killed public thread discovery because threads couldn't be reliably
reviewed for sensitive content, and redacts secrets best-effort at display.
Kranz's equivalents are stronger where it counts: M6 already requires the
token on *reads* ("transcripts are source code"), and redact-at-write with
fingerprint audit events beats display-time best-effort. No action; the
incident-shaped confirmation is worth having on record.

### Do not chase

- **Puck / agent-to-agent mesh / cross-machine spawning.** Free-form agent
  meshes are the anti-pattern the event log exists to prevent. The
  heterogeneous-fleet future goes through the engine and the log or not at
  all (docs/gascity.md).
- **Multiplayer co-driven threads.** Multi-operator hosted kranz is far
  behind M6/M8; D-E takeover links cover the near-term need.
- **The agentic-IDE surface** (editor hooks, palette, voice, Tab). Already
  ceded to the sgian side; Amp itself killed Tab and its editor extension.
- **Kill-features / no-backcompat ethos.** See positioning: additive-only is
  kranz's product, not its debt.
- **Ad-funded / subscription token economics.** Their arc (ads at claimed
  $10M+ run rate, killed because ads can't fund frontier tokens →
  subscriptions bundling compute hours) is interesting context for cost
  visibility, not a kranz concern.

---

## Ticket-ready drafts

Not filed — lift into `.kranz/tickets/<slug>.md` as prioritised.

### `cross-provider-scrutiny-default`

```markdown
---
title: Scrutiny validators default to a different provider pool than the worker
priority: 3
schedule: once
---

## Goal
When resolving roles for a mission, if the scrutiny/validator role resolves
to the same provider pool as the worker role, emit a config warning (and a
plan-time preflight note) recommending a cross-pool reviewer; document
cross-provider scrutiny as the recommended default in the roles docs.

## Context
Amp pairs every capability tier's writing model with a reviewer from a
different vendor, explicitly for cross-model perspective (ampcode.com/modes,
Jul 2026). Same-model review inherits same-model blind spots. Kranz roles
already pin models independently; this is policy + warning, not new
plumbing. Provider-pool declarations are shared with
backend-quota-breaker-reroute (claude models one pool; codex another).
Advisory warning only — never refuse a same-pool config; solo-backend
setups are legitimate. See docs/reviews/ampcode.md §1.

## Acceptance hints
- Tests: same-pool worker+scrutiny yields the warning event/preflight note;
  cross-pool yields none; single-configured-backend yields the warning with
  a "single backend configured" variant, not a failure.
- Docs: roles reference names the recommended default and why.
- Passed-count guard on the named filter.
```

### `repo-owned-scrutiny-checks`

```markdown
---
title: Tracked .kranz/checks/ review checks consumed by scrutiny validators
priority: 2
schedule: once
---

## Goal
Let the base branch declare repo-specific review checks — markdown files
with frontmatter (name, description, default severity) under
`.kranz/checks/` — that the scrutiny pass evaluates against the mission
diff, producing findings with the declared severity through the existing
findings/waiver flow.

## Context
Extends the merge-gates model (base-branch-owned, live-base read, a mission
cannot weaken the file that judges its own diff — same read discipline as
.kranz/merge-gates.json) from commands to LLM-judged dimensions. Source
pattern: Amp's .agents/checks/ (one reviewer subagent per check, YAML
frontmatter with severity and tool limits; ampcode.com/news/
liberating-code-review). Missing directory ⇒ today's scrutiny unchanged.
Invalid check file ⇒ fail closed at draft/approve naming the file (merge-
gates precedent). Checks add to the engine's scrutiny floor; they can never
replace or lower it. See docs/reviews/ampcode.md §2.

## Acceptance hints
- Tests: no checks dir → scrutiny unchanged; valid check → finding carries
  declared severity and check name; malformed frontmatter → draft/approve
  fails closed naming the file; check read from live base branch, not the
  mission branch (mission-branch edit to a check is ignored + surfaced).
- Passed-count guard on the named filter.
```

### `runaway-rate-in-report`

```markdown
---
title: Classify and record runaway missions in report.md and calibration data
priority: 3
schedule: once
---

## Goal
Mechanically classify a completed-or-failed mission as "runaway" when final
cost exceeds k× its estimate (default k=5, config) or it terminated on
budget exhaustion, record the classification in report.md and the mission
index, and surface the per-backend runaway rate alongside calibration's
"based on N missions."

## Context
Tail risk, not average cost, is what distinguishes backends/models in
practice: Amp's Gemini→Opus swap was decided by an off-the-rails thread
rate of 17.8% vs 2.4% at equal average cost (ampcode.com/news/opus-4.5).
Kranz already records per-run costs and estimates; this is a derived label,
mechanical (no LLM judgment), computed at report time. Never blocks
anything in v1 — visibility first, policy later. See
docs/reviews/ampcode.md §7.

## Acceptance hints
- Tests: cost > k× estimate → runaway=true in report + index; budget-
  exhaustion termination → runaway=true regardless of ratio; normal mission
  → absent/false; estimate missing → unlabeled, never guessed (house rule:
  no fabricated numbers).
- Passed-count guard on the named filter.
```

---

## Follow-ups outside tickets

Applied 2026-07-27, same session as this review:

- **Scoping-doc cross-links:** workspace-contract.md D-A gained the OIDC
  workload-identity preference (§3) and bounded hook timeouts, D-E gained
  authenticated-by-default previews (§8); the `workspace-contract-schema`
  and `workspace-remote-coder-provider` tickets inherit those details;
  roadmap M6 "auth grows up" gained the passkey step-up line (§4).
- **Roadmap pattern-notes:** Amp paragraph (2026-07-27) added beside the
  Warp/Cursor/Monaco/Mission Control scans.
- **Backend candidates note:** Amp CLI added beside backend_cursor with the
  shared proof bar and the two §5 cautions.

Advisory only, no action now: §6 supersede-aware retrieval (apply when
repo-knowledge injection lands), §9 webhook mechanics reference, §10
context-economics numbers for the SessionSpec `tools` item. The three
ticket drafts above remain unfiled pending prioritisation.
