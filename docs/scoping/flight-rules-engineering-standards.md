# Flight Rules — governed engineering standards for humans and agents

Status: **shipped 2026-08-11**. D-A through D-K are
decided below. Implementation is the KRZ-341–349 ticket series; the P1
vertical slice is roadmap M5.5.

Source pattern: Cloudflare, [How Cloudflare enforces engineering standards
using AI](https://blog.cloudflare.com/engineering-standards-enforcement/),
reviewed 2026-08-07. Cloudflare's useful mechanics are a governed RFC corpus,
stable structured SHOULD/MUST statements, an approved-before-enforced
lifecycle, deterministic retrieval, and rule-appropriate review mechanisms.
Kranz adds approval pinning, base-branch ownership, drift refusal, sandboxed
gate execution, and replayable evidence.

## Decision in one line

Add **Flight Rules** as a governed standards layer in a configured pack:
human-readable RFCs contain stable machine-readable rules; Kranz resolves the
applicable set, pins it in the approved mission contract, projects it into the
right workflow stages, enforces it through existing gates, and records enough
evidence to explain every pass, failure, and waiver later.

This advances the accepted product boundary rather than changing it:
Kranz still dispatches, gates, records and proves. It does not become a wiki,
an IDE, an incident tracker, or a code-writing runtime.

## Why this is a missing layer, not a new stack

Kranz already has nearly all of the execution substrate:

- `AGENTS.md` carries important repo conventions, but it is prose without a
  standards lifecycle or stable reusable rule identity.
- `docs/knowledge/` is committed, freshness-aware planning context. It is
  intentionally advisory and is not an enforcement source.
- Approved mission assertions are structured and self-contained, but describe
  one change rather than organization-wide policy.
- The gate pipeline structurally orders deterministic gates before model-
  judged gates; gate results, confidence, provenance replay, evidence bundles,
  and outcome folds are shipped.
- Packs are already the domain-free boundary through which house standards,
  prompts, gates, and evidence adapters extend Kranz.
- Merge gates already establish the ownership idiom: policy comes from the
  live base, and a mission cannot weaken the policy judging its own diff.

Flight Rules supplies the missing reusable policy object and lifecycle over
those primitives.

## Keep the artifacts distinct

| Artifact | Question it answers | Authority |
|---|---|---|
| Mission contract (`plan.json`) | What must this change deliver? | Human-approved and immutable for the active revision |
| Flight Rules pack | What engineering policy applies repeatedly? | Governed pack, resolved from trusted base/snapshot |
| `AGENTS.md` | How should an agent operate in this checkout? | Concise repo guidance; may link to or project rules, never canonical |
| Knowledge vault | What stable context helps planning? | Reviewed, freshness-qualified advice |
| Gate | What mechanism determines compliance? | Registered engine/pack checker with an evidenced outcome |

Copying all standards into `AGENTS.md`, duplicating them into every ticket, or
promoting ranked knowledge excerpts into hard policy would create multiple
sources of truth and is rejected.

## Canonical pack shape

The pack contract grows one additive schema version and one optional standards
root. A schema-2/3 pack or a mission with no configured pack behaves exactly as
today.

```text
acme-engineering-pack/
  pack.toml
  standards/
    RFC-014-rust-production-safety/
      rfc.md
      rules/
        ENG-RUST-014-no-unwrap.md
        ENG-RUST-015-error-context.md
```

`pack.toml` identifies the root; it does not duplicate the rule metadata:

```toml
[pack]
name = "acme-engineering"
schema = 4

[standards]
root = "standards"
```

`rfc.md` owns lifecycle and governance metadata:

```yaml
---
id: RFC-014
title: Rust production error handling
status: approved
owner: platform-security
effective-at: 2026-09-01T00:00:00Z
---
```

Each rule file owns exactly one normative statement and its applicability:

```yaml
---
id: ENG-RUST-014
revision: 3
rfc: RFC-014
level: must
status: active
statement: Production Rust must not call unwrap outside tests.
domains: [rust, reliability]
stages: [planning, implementation, validation, merge]
when-paths: [crates/]
task-classes: [implementation]
checker: gate:rust-no-unwrap
waivable: false
---
```

The Markdown body contains rationale, examples, counterexamples, and source
links. The frontmatter statement is the one normative machine/human text;
rendered documentation quotes it instead of maintaining another copy. No
generated JSON index is committed as an editable authority. Kranz produces a
normalized, stable-sorted manifest and content digest at load/approval time.

## D-A — packs are the source boundary

**Decided:** Flight Rules are an additive extension of the existing pack
contract, not a new core knowledge store or `.kranz/checks/` dialect.

The core defines generic schema, lifecycle, selection, enforcement, and
evidence. Actual house rules remain in a private or repo-vendored pack. One
configured pack is sufficient for the first release; organization-wide pack
distribution, signing, inheritance, and multi-pack precedence are later
control-plane concerns.

For repo-relative packs, all governing bytes—including `pack.toml`, rules,
RFC metadata, and referenced checker declarations—are read from tracked blobs
in the pinned base Git tree, not the mission worktree. For an external pack,
Kranz capability-reads the pack once and pins its normalized bytes and digest
at approval. In both cases, the approved manifest—not a later filesystem
read—is the active mission authority.

M5.5 permits an external/untracked pack to contribute approved advisory rules,
but not enforced rules: Kranz has no base history with which to prove its
lifecycle transition or ownership. Blocking policy must be in a tracked,
repo-relative pack for the first release. Organizations may vendor their
shared pack into each repo; signed/versioned external distribution is later
control-plane work, not a trust-shaped shortcut in this slice.

## D-B — explicit RFC lifecycle

**Decided:** RFC status is one of `draft`, `approved`, `enforced`, or
`retired`; every active contained rule inherits it. A rule may narrow that to
`retired` as an immutable tombstone, allowing one statement to leave an active
RFC without losing its stable identity. Retired rules cannot be reactivated or
deleted from the canonical catalog.

| Status | Prompt/review behavior | Blocking behavior |
|---|---|---|
| `draft` | Authoring/lint surfaces only | Never |
| `approved` | Applicable statements are projected and findings recorded | Never |
| `enforced` + `should` | Projected and reported | Advisory |
| `enforced` + `must` | Projected and evaluated by its checker | May block according to D-F |
| `retired` | Historical catalog/replay only | Never |

An RFC may not move directly from absent/draft to enforced. The standards
transition check compares the proposed pack to the trusted base and requires
an approved state first. `effective-at` permits a deliberate absorption
window after promotion; before that instant the rule remains approved in
effect. Demotion and retirement are explicit reviewed changes, never runtime
flags.

## D-C — stable identity, revision, and digest

**Decided:** RFC IDs and rule IDs are permanent, pack-wide unique identifiers.
File paths and titles may change without changing identity. An ID is never
reused after retirement.

`revision` increments for a semantic change to statement, level, scope,
checker, or waiver posture. Retirement is a one-way tombstone transition, not
deletion. The loader also hashes all normalized governing bytes. IDs make
findings and trends joinable; revisions make the meaning explicit; the content
digest detects a forgotten revision bump or any other byte drift. `kranz
standards lint --against <ref>` refuses a semantic change without a revision
increment, disappearance of a known ID, or reactivation of a tombstone.

## D-D — applicability is deterministic

**Decided:** the engine, not a model, selects applicable rules from declared
metadata.

For the first release, a rule applies when:

1. the current stage appears in `stages`;
2. `task-classes` is absent or contains the mission task class; and
3. `when-paths` is absent or at least one approved touch-set path sits at or
   below a declared prefix.

Domains are labels for browsing and reporting, not an LLM-selected policy
switch. Initial planning uses ticket repo refs and explicit touch hints to
select a conservative candidate set. When the planner returns a proposed touch
set, the engine resolves again. If that authoritative set contains a rule the
planner did not receive, the plan is not offered for approval: one bounded
revision turn receives the delta, and resolution repeats until the plan/rule
set reaches a fixed point or planning parks. Approval pins only that fixed
point. Final validation rechecks the actual diff; a newly applicable rule means
the mission escaped its approved policy envelope and must be revised, not
silently judged against a moving set.

All normative statements in the applicable manifest must fit the configured
hard byte/count caps. Kranz fails approval naming the excess; it never drops
an enforced rule to meet a prompt budget. Full RFC rationale is lazy context,
while the compact statement is always present.

## D-E — approval pinning and merge-time drift

**Decided:** the proposed plan carries an additive `standardsManifest`
containing pack identity/digest and the applicable rule IDs, revisions,
statuses, statements, scopes, sources, and checker bindings. Approval reloads
the trusted source and rejects a stale or substituted manifest.

Every later mission stage consumes the approved manifest, so an external pack
edit or branch mutation cannot alter a running mission. At final validation,
actual changed paths are checked against the pinned source snapshot. At merge,
Kranz resolves the current live base policy against the exact scratch
integration diff. If the current applicable enforced set differs from the
approved set, merge refuses with a `standards.drifted` decision and requires
explicit revalidation/reapproval. It neither grandfather-skips current policy
nor silently applies new policy to an old consent artifact.

A mission that edits its repo-relative standards pack is judged by the old
base version. Its changes can govern only future missions after landing.

## D-F — the mechanism belongs to the rule

**Decided:** `checker` is a typed reference, never arbitrary executable prose.
The initial checker forms are:

- `gate:<stable-gate-id>` — a registered deterministic engine/pack gate;
- `agent-judgement` — the engine-owned contextual standards reviewer over the
  applicable structured statements and the full diff;
- `manual-attestation` — an explicit authorized human decision.

The checker registry is resolved with the standards source: an engine checker
is immutable code, while a pack checker declaration/command is loaded and
pinned from the same trusted base pack bytes. Evaluation never looks up a
checker name in the mission worktree after approval. At merge, the live-base
checker binding participates in the same applicable-policy drift comparison.
Draft rules may omit a checker while being authored; promotion to approved
requires a valid binding so the absorption period produces real advisory
evidence rather than prompt-only guidance.

Deterministic gates remain structurally ahead of model judgement. A checker
owns its authoritative verdict and may report confidence/threshold through
the existing gate result; Kranz records but never reverse-engineers or
overrides that verdict from the score. An enforced MUST with a missing,
unknown, stage-incompatible, or unexecutable checker fails closed before
execution.

Approved rules and enforced SHOULDs are evaluated and can produce findings but
cannot block.
An enforced MUST blocks completion/merge on a failing authoritative checker,
unless an exact waiver permitted under D-I exists. Engine floor gates are not
weakened or made waivable by Flight Rules.

## D-G — one resolved set, stage-specific projections

**Decided:** the same pinned manifest feeds every consumer through a compact,
stage-filtered projection:

- planning: applicable statements and sources constrain the proposed plan;
- plan review: the operator sees the exact rules/revisions being accepted;
- implementation: workers receive only implementation-stage statements;
- scrutiny/functional validation: validators cite rule IDs in findings;
- final/merge: registered checkers evaluate the actual diff/integration;
- review task classes: spec and incident artifacts use the same
  resolver, without a new standards corpus.

Each projection's content hash joins the existing prompt/session provenance.
`AGENTS.md` may link to the standards pack or carry a generated summary for
human convenience, but drift in that summary cannot change enforcement.

## D-H — standards evidence is first-class

**Decided:** additive event and finding fields make policy provenance
queryable without parsing prose.

The event trail records at minimum:

- `standards.resolved`: source identity/digest, selected rule revisions,
  selection inputs, stage, `effective-at` evaluation instant, and approval
  sequence;
- `gate.result`: optional `ruleIds` linking the mechanism to the standards it
  evaluated;
- `validation.finding`: optional rule ID/revision/source/checker identity;
- `standards.drifted`: approved/current digests and changed applicable rules;
- `standards.waiver.approved`: the exact exception described by D-I.

Old events and plans continue to fold through `#[serde(default)]`. Provenance
replay, `report.md`, and evidence bundles render a rule coverage matrix:
`passed`, `failed`, `advisory`, `waived`, `not-evaluated`, or `not-applicable`,
with mechanism and artefact references. Absence is never rendered as pass.

## D-I — waivers are narrow human decisions

**Decided:** an enforced MUST is waivable only when its rule declares
`waivable: true` (`false` when omitted). A model may propose a fix or request a
decision; it may not approve a standards waiver.

An authorized human waiver is bound to the mission, rule ID and revision,
checker/finding fingerprint, affected paths, current diff digest, reason,
approver principal, approval event sequence, and expiry. The digest covers the
rule's affected-path diff (or the whole diff for an unscoped rule), so a
relevant change invalidates it without granting anything over unrelated paths.
Where the current local authority model cannot identify a person, evidence
says `local-operator` plus the authenticated invocation surface rather than
inventing a real-world identity. Waivers subtract one exact failure; no waiver
disables a checker, RFC, domain, or enforcement class. There is no global
`--ignore-standards` switch.

The first release stores mission waivers as events. Organization-wide standing
exceptions, signed policy bundles, and delegated waiver authorities are later
control-plane work.

## D-J — failure and security posture

**Decided:** configured standards fail closed; absence remains a no-op.

| Threat/failure | Required behavior |
|---|---|
| Mission edits its own rules | Read trusted base; ignore and surface the branch edit |
| External pack changes mid-run | Consume approval-pinned manifest bytes |
| Unversioned external pack declares enforced policy | Refuse blocking posture; advisory only until tracked or signed/versioned |
| Mission changes a referenced pack gate | Use approval-pinned base checker declaration; include binding in merge drift check |
| Symlink/FIFO/device or oversized corpus | Capability-relative no-follow reads; regular files only; per-file/count/total caps |
| Malformed or unknown metadata | Refuse load naming the field/file |
| Rule text attempts prompt injection | Only governed, bounded statements enter marked projections; prose cannot register tools or commands |
| Checker name shadows an engine gate | Stable ID namespace collision is a load error |
| Applicable enforced rule omitted for budget | Refuse approval; never truncate policy |
| Policy changes before merge | Refuse with drift evidence; require reapproval/revalidation |
| Waiver reused after change | Diff/rule/finding binding invalidates it |

Standards commands still execute worker-authored repository code, so they use
the same sandboxed, cleared-environment gate runner as existing contract and
merge gates. Source parsing itself executes nothing.

## D-K — product scope and scaling

**Decided:** M5.5 delivers repo/pack-owned standards for local and existing
multi-repo Kranz operation. It does not add a hosted company-wide standards
service, RBAC hierarchy, editor plugin, semantic search system, or incident
management product.

The normalized manifest and evidence schema deliberately leave room for a
future signed organization catalog and pack distribution. A later P3 consumer
may review spec or incident artifacts through explicit task classes, but it
must reuse this resolver and evidence model rather than build a parallel AI
review service.

## Delivery sequence

| Order | KRZ | Ticket | Pri | Outcome |
|---:|---:|---|---:|---|
| 1 | 341 | `flight-rules-pack-contract` | 1 | Canonical schema, strict loader, lifecycle lint, normalized digest |
| 2 | 342 | `flight-rules-resolution-pin` | 1 | Deterministic selection, approval manifest, base ownership, drift refusal |
| 3 | 343 | `flight-rules-finding-provenance` | 1 | Rule-linked findings/events/replay/evidence |
| 4 | 344 | `flight-rules-waiver-decisions` | 1 | Exact, authorized, expiring exceptions before blocking ships |
| 5 | 345 | `flight-rules-workflow-projection` | 1 | Planning/worker/validator stage projections |
| 6 | 346 | `flight-rules-enforcement-binding` | 1 | Approved advisory; enforced MUST blocking through existing gates |
| 7 | 347 | `flight-rules-dashboard-report` | 2 | Review and operations UI plus coverage matrix |
| 8 | 348 | `flight-rules-effectiveness-metrics` | 3 | Deviation, block, waiver, calibration, and false-green trends |
| 9 | 349 | `flight-rules-review-task-classes` | 3 | Spec/incident artifact consumers without a second policy system |

Tickets 341–346 are the near-term shippable unit. Enforcement does not ship
before both provenance and the human exception path exist.

## M5.5 done-when proof

A synthetic schema-4 pack contains one approved SHOULD, one enforced MUST
bound to a deterministic gate, and one enforced contextual MUST. Over a
mission whose touch set makes all three applicable:

1. draft and plan review show the exact stable IDs, revisions, statements,
   sources, statuses, and pack digest;
2. worker and validator prompts contain only their stage-applicable projection
   and record its hash;
3. the approved rule produces a visible advisory but cannot block;
4. each enforced MUST produces a rule-linked authoritative gate result, and a
   failure prevents completion/merge;
5. an authorized, permitted exact waiver is replayable and invalidates after
   the diff changes;
6. a mission-branch policy edit cannot affect its own judgement;
7. a live-base enforcement change before merge refuses as policy drift; and
8. provenance replay and the evidence bundle answer why every rule passed,
   failed, or was waived, while an old/no-pack mission remains unchanged.
