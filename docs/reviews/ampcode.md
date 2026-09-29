# Engineering notes from the Amp review (2026-07-27)

Historical design inputs from the [Amp manual](https://ampcode.com/manual)
and its [news archive](https://ampcode.com/news). The public notes retain
Kranz's implementation requirements and source attribution. They do not
assert current third-party capabilities or qualify an adapter. The dated
proposals and unfiled drafts below remain subject to the current roadmap,
architecture boundary and accepted tickets.

## 1. Cross-provider scrutiny

Worker and scrutiny roles can select different provider pools. A policy
warning should make a same-pool pairing visible without refusing a valid
single-backend configuration. Record the actual reviewer identity after
fallback; a fresh session alone does not establish a different provider.
Provider-pool declarations can be shared with backend quota routing.

## 2. Repo-owned review checks

The historical reference is Amp's declarative review-check pattern. Kranz
checks must be owned by the trusted base branch and describe their name,
severity and tool limits. Missing optional checks preserve normal scrutiny;
malformed checks fail closed. A mission must not weaken the checks judging
its own diff. The gate and pack contracts are the current integration seam;
the draft below records the earlier directory-shaped proposal.

## 3. Workload identity

Prefer short-lived, audience-bound OIDC workload credentials for remote
workspaces when the provider supports them. Bind JWT claims to the repo,
mission and profile. Keep credential values out of workspace contracts and
expose only the declared secret names or identity requirements. This is a
remote-provider design requirement, not a claim of a local implementation.

## 4. Step-up authentication

A hosted service should require fresh human authentication for sensitive
spend and control operations, including approve, start and queue. Evaluate
WebAuthn at the hosted-auth milestone; the existing local token and Slack
allowlist retain their documented roles.

## 5. Backend qualification

Amp remains a candidate requiring a version-pinned probe. Verify headless
execution, structured report and usage output, permission mapping, working
directory isolation and the no-push/no-main-write boundaries. Recheck the
selected version's interfaces; this historical note is not compatibility
or containment evidence.

## 6. Supersede-aware retrieval

Historical context can be revised or contradicted. Retrieval should check
for superseding decisions, preserve source revisions and identify context
as untrusted data. Apply this requirement within the existing knowledge
boundary, without adding a new execution-side context system.

## 7. Runaway-cost reporting

Cost averages can hide expensive failures. A proposed mechanical label
uses the ratio of actual cost to estimate or an authoritative budget-
exhaustion outcome. Preserve missing estimates as unknown. Define the
threshold and denominator explicitly before comparing backends or models.

## 8. Workspace lifecycle

The retained workspace requirements are bounded setup/resume hooks,
runtime-assigned service and preview addresses, authenticated previews,
observable pause/resume state, and explicit provider billing behavior.
Record workspace identity and handoff artifacts through the workspace
contract; evaluate these requirements when selecting a remote provider.

## 9. Event-triggered work

Webhook triggers need durable identifiers, deduplication for at-least-once
delivery, handler deadlines and rate limits. They create audited tickets or
missions behind the existing consent boundary. A trigger does not grant
permission for autonomous merge or push.

## 10. Context and tool scope

Keep tool availability explicit in the session contract. Measure context
cost at the provider seam where usage is reported. Feature-scoped sessions
retain their approved scope; this note does not authorize new prompt or
context-management machinery.

## 11. Transcript access and redaction

Transcripts can contain source and credentials. Apply the documented read
authority, redaction and audit policy to stored and exported records. A
redaction pass is not proof that an arbitrary transcript is safe to publish.

## Scope boundary

General-purpose agent meshes, multi-user co-editing and editor features
remain outside this work. Persisted mission contracts and event schemas keep
their compatibility rules. Hosted orchestration uses the declared workspace
and consent seams.

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
Source: Amp's historical cost-tail discussion (ampcode.com/news/opus-4.5).
Report costly failure tails separately from average mission cost.
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

## Recorded follow-ups

The 2026-07-27 review informed workspace-contract identity, bounded hooks,
authenticated previews and the hosted-auth step-up requirement. Roadmap and
backend notes carry those requirements. The three drafts above were not
filed by that review; consult current tickets before scheduling new work.
