# Governance and evidence layer — KRZ-300/310/320/330/340 backlog

Status: positioning accepted 2026-07-29, recorded as an ADR
(`docs/knowledge/decisions/positioning-governance-evidence-layer.md`).
The initial KRZ-300/310/320/330 tickets were created the same day; the accepted
Flight Rules extension added KRZ-341–349 on 2026-08-07. Sequencing and
reconciliations follow. This doc is the series map — the KRZ-numbered planning
ids live here (precedent: `local-inference-executor-tier.md` for KRZ-201–209);
the on-disk backlog is slug tickets in `.kranz/tickets/`.

## The decision, one line

Kranz dispatches, gates, records and proves. It does not write code.

Execution is commoditising (headless agent CLIs improve weekly with zero
effort from this project); model hosting and fine-tuning are backends, not
competitors. What is not commoditised — and what kranz already has most of —
is the layer above: event-sourced state, cost per event, grants/consent with
escalation, secret-scan-at-write, and a ledger that can answer *why did this
change pass, and who or what decided it* months later. Full reasoning and the
frozen/retained boundary: the ADR.

## Series map

| KRZ | Ticket slug | Pri | Blocked by | Substrate status |
|-----|-------------|----:|------------|------------------|
| 301 | `acp-worker-backend` | 1 | — | new; cursor ACP route decision is prior art |
| 302 | `claude-code-hook-gate-projection` | 1 | gate-plugin-interface | extension of `backend_claude` |
| 303 | `heterogeneous-dispatch-pool` | 2 | — | new orchestration over existing backends |
| 304 | `divergence-first-class-event` | 2 | heterogeneous-dispatch-pool | new; additive events |
| 305 | `execution-primitive-freeze-notes` | 2 | — | ADR shipped; this propagates it |
| 311 | `gate-plugin-interface` | 1 | — | new; merge_gate.rs is the nearest precedent |
| 312 | `gate-results-first-class-events` | 2 | gate-plugin-interface | additive events |
| 313 | `pack-contract-gates-prompts` | 1 | gate-plugin-interface | extends `packaging/gascity` pack |
| 314 | `clean-room-domain-lint` | 1 | — | new; secret-scan CI pattern to follow |
| 315 | `gate-confidence-score` | 1 | gate-plugin-interface, gate-results-first-class-events | new; scored-gates addendum |
| 316 | `gate-score-distribution-flags` | 2 | gate-confidence-score | new; scored-gates addendum |
| 321 | `outcomes-report-task-class` | 2 | — | **extension** — outcomes.rs fold exists |
| 322 | `escalation-ledger-export` | 2 | — | **extension** — ledger exists in outcomes.rs |
| 323 | `rubber-stamp-grant-flag` | 3 | — | **mostly shipped** — grant-latency fold exists |
| 324 | `false-green-defect-linkage` | 1 | — | extension — `traced-from-mission` exists; priority raised by the scored-gates addendum |
| 325 | `provenance-replay` | 1 | gate-results-first-class-events | new; the audit story |
| 326 | `evidence-bundle-export` | 2 | provenance-replay | new; packaging of 325 |
| 327 | `contract-validation-gates` | 1 | — | extension — contract_lint/contract_health |
| 328 | `ready-org-catalog` | 3 | — | **mostly shipped** — ready.rs + amm.rs per-repo |
| 329 | `cost-per-merged-change` | 2 | — | extension — cost fold + merged.rs; scored-gates addendum |
| 331 | `backend-routing-abstraction` | 2 | — | extension — AgentBackend seam + local-inference slices |
| 332 | `training-corpus-export` | 3 | divergence-first-class-event, escalation-ledger-export | extension — trace_export.rs |
| 333 | `outcomes-comparison-metrics` | 3 | — | extension — outcomes report presentation; scored-gates addendum (evidence series continues past the 330–332 inference block) |
| 341 | `flight-rules-pack-contract` | 1 | — | extension — pack contract gains governed structured standards |
| 342 | `flight-rules-resolution-pin` | 1 | flight-rules-pack-contract | new policy-resolution + approval-pin seam; merge/routing ownership idiom |
| 343 | `flight-rules-finding-provenance` | 1 | flight-rules-resolution-pin | extension — gate events, findings, replay and evidence bundle |
| 344 | `flight-rules-waiver-decisions` | 1 | flight-rules-resolution-pin, flight-rules-finding-provenance | new explicit-human exception event; no model waiver of enforced MUST |
| 345 | `flight-rules-workflow-projection` | 1 | flight-rules-resolution-pin, flight-rules-finding-provenance | extension — stage-filtered planning/worker/validator projection |
| 346 | `flight-rules-enforcement-binding` | 1 | flight-rules-resolution-pin, flight-rules-finding-provenance, flight-rules-waiver-decisions | extension — GatePipeline + pack/agent/manual checker bindings |
| 347 | `flight-rules-dashboard-report` | 2 | flight-rules-workflow-projection, flight-rules-enforcement-binding, flight-rules-waiver-decisions | extension — plan consent + coverage/drift/waiver surfaces |
| 348 | `flight-rules-effectiveness-metrics` | 3 | flight-rules-dashboard-report | extension — outcomes/gate-score/false-green folds by rule revision |
| 349 | `flight-rules-review-task-classes` | 3 | flight-rules-workflow-projection, flight-rules-enforcement-binding, flight-rules-dashboard-report | later spec/incident artifact consumer; no parallel policy system |

## Sequencing

1. **311, 313, 314** — plugin contract and clean-room check. The boundary
   must exist before anything is load-bearing on it.
2. **301, 302** — dispatch to external agents with in-process gate projection.
3. **321, 322, 325** — the evidence spine. Mostly extensions; finishing it
   makes the layer demonstrable.
4. **303, 304** — heterogeneous dispatch and divergence recording.
5. Remainder.

Shippable unit: steps 1–3 — a governance layer that dispatches to an agent
CLI and produces a provenance trail.

Near-term next shippable unit (roadmap M5.5): **341 → 342 → 343/344 →
345/346**. Flight Rules is the first product feature built on the now-shipped
pack/gate/evidence spine. Blocking enforcement does not ship until both typed
provenance and the authorized-human waiver path exist. 347 is the P2 operator
surface; 348/349 are P3 follow-through.

## Reconciliations against the source plan (recorded, not silent)

Discovery (2026-07-29) found the repo further along and differently shaped
than the plan assumed. The tickets encode these corrections:

- **KRZ ids are planning aliases, not the backlog format.** Tickets are
  slugs citing their KRZ id in Context (the KRZ-201–209 precedent).
- **KRZ-109 has no repo artifact.** `acp-worker-backend` supersedes nothing
  on disk; nearest prior art is `docs/scoping/cursor-cli-backend.md`'s
  direct-parser-vs-ACP route decision.
- **`hooks.rs` naming collision.** The engine's `hooks.rs` is D-F webhook
  triggers, not Claude Code lifecycle hooks; KRZ-302's slug and text
  disambiguate.
- **Dependency relaxations.** The plan blocked 302 and 303 on the ACP
  worker; `backend_claude` and the existing backend set already provide the
  session seam and a heterogeneous pool, so those edges are dropped (ACP
  *widens* the pool; it does not gate it).
- **KRZ-323/328 are mostly shipped** (grant-latency distribution in
  outcomes.rs; per-repo ready/AMM in the CLI). Scoped down to the true
  deltas: threshold flagging; M8 catalog aggregation.
- **KRZ-321/322/327/331/332 are extensions**, not new builds — outcomes.rs,
  contract_lint/contract_health, the AgentBackend seam, and trace_export.rs
  carry most of the weight already.
- **KRZ-314 will pass on first run.** The leakage inventory was clean on
  2026-07-29 (zero domain-vocabulary hits in code, docs, tickets). The
  deliverable is the guard, not the failures. The lint's own config must not
  leak the vocabulary it bans — the ticket specifies a hashed denylist.
- **KRZ-311/313 subsume** the `repo-owned-scrutiny-checks` draft from
  `docs/reviews/ampcode.md` §2 (repo-declared model checks register through
  the gate interface); cross-provider scrutiny (ampcode §1) is the 2-stream
  degenerate case of KRZ-303.
- **Local-inference tickets** (`local-inference-*`) remain live as
  implementation slices *under* KRZ-331 rather than a standalone workstream;
  their scoping doc's KRZ-20x framing folds into this series.
- **KRZ-305 splits**: the positioning decision shipped as the ADR; the
  ticket covers propagating deprecation/boundary notes so the freeze is
  findable at the point of temptation.

## Addendum 2026-07-29 — scored gates and comparison metrics

Source: review of Litera's "AI Productivity Inflection" post (ARGO/Mates
programme, Jul 2026) — confidence-scored review agents standing in for
sign-offs, low scores routed back to a human; the same shape as kranz's
grant/consent and escalation model, with a scoring layer on top. Four
tickets added: KRZ-315/316 (gate series) and KRZ-329/333 (evidence
series). The source's provisional numbering (301–304) collided with this
map and was reassigned. Reconciliations:

- The score-contract change is folded into `gate-plugin-interface`
  directly (result type reserves optional score + threshold from day one)
  so no gate is ever written boolean-only and reworked;
  `gate-confidence-score` is the persistence/query slice.
- `false-green-defect-linkage` raised 2→1 instead of duplicating a
  linkage ticket; `outcomes-comparison-metrics`' defect-density slot
  depends on it and renders empty until it lands.
- Out of scope, reaffirmed: knowledge grounding arrives as a pack, never
  core (positioning ADR) — an existing consumer-side knowledge base covers
  that need on the pack side of the boundary.

## Addendum 2026-08-04 — Warp Agent CLI scan

Source: the Warp Agent CLI launch review (roadmap pattern notes,
2026-08-04). Three tickets added outside the KRZ numbering:
`config-fail-open-audit` (governance hardening — Warp shipped
auto-approve-bypasses-denylist and replace-not-extend denylists as live
counterexamples), `routing-rules-config` (the KRZ-331 config-surface
slice; Warp's complexity-tier and ordered-rule router forms are the
reference), and `pty-functional-validation` (extends M5 functional QA to
terminal-interactive targets; validation-side only). Reference mechanics
recorded on `heterogeneous-dispatch-pool` (harness-agnostic addressing,
unified lifecycle vocabulary) and `backend-routing-abstraction`. Warp is
not a backend candidate until a headless/JSON mode exists.

## Addendum 2026-08-07 — Cloudflare engineering standards enforcement

Source: Cloudflare's 2026-08-04 engineering-standards write-up. The relevant
product pattern is not another knowledge base: one governed RFC corpus moves
from approved/advisory to enforced, each SHOULD/MUST has a stable structured
identity, the applicable subset reaches code/spec/incident review, and
mechanical rules use linters while contextual rules use AI review.

Accepted Kranz adaptation:
`docs/scoping/flight-rules-engineering-standards.md` (D-A…D-K) and the
KRZ-341–349 series above. It stays on the retained side of the positioning
boundary: standards content lives in packs; core owns generic lifecycle,
deterministic resolution, consent pinning, gate binding, exception authority,
and evidence. Kranz strengthens the source pattern with base-branch ownership,
approval-pinned manifests, merge-time policy-drift refusal, sandboxed gate
execution, and exact replayable waivers. User-facing name: **Flight Rules**;
`Codex` is rejected because it collides with OpenAI Codex.

## Standing rule for every ticket in this series

Kranz core stays domain-free: no customer names, no legacy-platform
vocabulary, no customer schema identifiers — in code, comments, ticket text,
fixtures, or example config. Domain knowledge ships in packs (KRZ-313)
consumed through the declared contract. Test: would this exist if that
specific customer did not? If it only makes sense with a particular legacy
platform in the room, it belongs in a pack. Tickets use synthetic examples
only.
