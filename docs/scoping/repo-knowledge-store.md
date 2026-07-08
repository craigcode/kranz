# Persistent repo knowledge store for agents and humans

Status: researched 2026-07-08. Decisions D-A through D-F are recommendations,
not yet accepted operator decisions. Buildable as three slices after the cleanup
tier; no code has shipped from this document.

## Why

Kranz has three repo-memory lanes today, none of which is the durable knowledge
layer this repo wants next.

1. **The planner rediscovers the repo every draft.** `drive_draft` seeds the
   orchestrator with the ticket body, then asks for a plan; the planning seed
   knows the mission goal and may include lessons, but it does not carry a
   maintained architectural map. A fresh drafter still spends 10-20 minutes
   reading the same files and rediscovering the same hazards.
2. **The lessons loop is deliberately tiny.** `.kranz/lessons/` captures at
   most one reusable lesson per mission, and `render_lessons_index` injects a
   byte-capped newest-first block into planning. That is right for hard-won
   constraints; it is too small for module maps, gate recipes, design history,
   and "read these together" relationships.
3. **Research dies in the transcript.** During drafting, the model often learns
   which files matter, which docs are stale, which tests prove the claim, and
   which alternatives it considered. The approved `plan.md` keeps the final
   choice; the evidence trail is mostly buried in mission transcripts and is
   hard for humans or future agents to browse.

The failure mode is not just wasted tokens. A stale or hallucinated repo fact
can steer the validation contract, workers, and validators wrong before any
diff exists to review. A May 2026 retrieval study found stale repo snippets can
actively bias code generation toward obsolete APIs rather than acting as
harmless noise ([arXiv:2605.14478](https://arxiv.org/abs/2605.14478)). Kranz's
store must therefore make freshness and provenance first-class.

## What exists

- `AGENTS.md`, `docs/design.md`, `docs/roadmap.md`, and `docs/scoping/*.md`
  are the current human-readable standing context.
- `.kranz/tickets/*.md` is the operator backlog and planning input.
- `.kranz/lessons/index.md` plus `.kranz/lessons/<mission>.md` is the existing
  append-only cross-mission memory. It is intentionally small, repo-level, and
  injected only into planning seeds.
- `crates/engine/src/draft.rs::drive_draft` is the primary consumption path
  for ticket planning.
- `crates/engine/src/orchestrator.rs::ensure_orchestrator` and
  `orch_single_shot_turn` are the current planning-seed injection points.
- `request_revised_plan` and the M2 scoping doc will add a second planning-like
  consumer: mid-mission revision proposals. This store should feed that path
  too, once M2 exists.

## Research notes

### OpenWiki / DeepWiki-Open

[AsyncFuncAI/deepwiki-open](https://github.com/AsyncFuncAI/deepwiki-open) is
the likely project behind the operator's "openwiki" note. It presents itself as
an open-source DeepWiki implementation that analyzes GitHub/GitLab/Bitbucket
repositories, generates documentation, creates diagrams, and organizes the
result into an interactive wiki. As of this research pass it was MIT licensed,
had about 17k GitHub stars, used a Python/TypeScript stack, and published no
GitHub releases.

Fit for kranz: useful as a comparison target and possible offline generator,
but not as a trusted runtime dependency. Its output shape is an app/wiki
experience, while kranz needs reviewable, committed, source-linked Markdown
that can be diffed in the same approval flow as plans and reports.

### Obsidian-compatible vaults

Obsidian defines a vault as a filesystem folder containing notes, attachments,
and optional Obsidian settings ([Manage vaults](https://obsidian.md/help/Files%2Band%2Bfolders/Manage%2Bvaults)).
It supports both Wikilinks and ordinary Markdown links for internal notes, and
explicitly calls out interoperability tradeoffs
([Internal links](https://obsidian.md/help/Linking%2Bnotes%2Band%2Bfiles/Internal%2Blinks)).

Fit for kranz: strong. The durable artifact can be plain Markdown in git, with
an Obsidian-friendly link convention for operators who want a graph UI. Avoid
requiring Obsidian-specific block refs or plugins in the canonical files; those
do not travel as cleanly across tools.

### Git-backed wiki viewers

[Gollum](https://github.com/gollum/gollum) is a mature Git-backed wiki that
serves human-editable text/markup files, supports directories, preserves page
history through git, and can run locally. It is useful evidence that a
plain-file wiki is enough for browsing and review.

Fit for kranz: optional viewer only. Kranz should not need Ruby or a web wiki
to draft a mission, but a future `kranz knowledge serve` could point a viewer
at the same committed files.

### RepoAgent and CodeWiki

[RepoAgent](https://github.com/OpenBMB/RepoAgent) and its paper describe a
repository-documentation pipeline with global structure analysis, generated
Markdown docs, and incremental updates through git/pre-commit
([arXiv:2402.16667](https://arxiv.org/abs/2402.16667)). Its implementation is
Python-heavy and its documented hook path is currently Python-oriented, which
does not fit kranz as a direct dependency, but two ideas transfer well:
maintain docs as code changes, and compute affected documentation from
structure/reference changes instead of regenerating everything.

[CodeWiki](https://arxiv.org/abs/2510.24428) reinforces the same lesson at the
repo level: hierarchical decomposition and explicit architectural context beat
isolated symbol summaries. It is research-grade evidence for the shape, not a
ship-ready dependency decision.

### Repo maps and deterministic context

Aider's repo map is a concise, token-budgeted map of important files, symbols,
signatures, and relationships, with graph ranking used to fit the active token
budget ([aider repo map](https://aider.chat/docs/repomap.html)). A 2026
Repository Intelligence Graph paper reports gains from deterministic,
evidence-backed build/test structure maps ([arXiv:2601.10112](https://arxiv.org/abs/2601.10112)).
Those are not persistent human knowledge stores by themselves, but they show
the prompt-side selection rule kranz should borrow: inject a small, ranked
slice, not the whole vault.

### Context can hurt

A June 2026 revision of an AGENTS.md evaluation found repo context files did
not generally improve task success and increased inference cost by more than
20% on average; it concluded that context files are useful for non-standard
coding practices, but broad repo overviews need rigorous evaluation before
deployment ([arXiv:2602.11988](https://arxiv.org/abs/2602.11988)). This should
temper the design: the knowledge store is browseable and queryable by default,
but only narrow, evidence-linked excerpts enter prompts automatically.

## D-A - canonical shape

**Proposal: a committed Markdown vault under `docs/knowledge/`, plus generated
indexes under `.kranz/knowledge/` only as disposable runtime cache.**

The canonical store is:

```text
docs/knowledge/
  index.md
  architecture/
  operations/
  validation/
  surfaces/
  decisions/
  lessons.md
  glossary.md
```

Each note has front matter:

```yaml
---
title: Short title
owner: human|agent|mixed
freshness: live|check-on-touch|stale
last_verified: 2026-07-08
verified_against:
  - crates/engine/src/orchestrator.rs
  - cargo test -p kranz-engine lessons
---
```

Use ordinary Markdown links for canonical cross-links. Wikilinks are allowed
only as an optional duplicate alias if they do not replace the Markdown link.

Rejected: a generated single mega-brief. It is cheap to inject, but painful to
review, easy to stale as one unit, and not pleasant for humans to browse.

Rejected: embeddings/vector DB as the canonical store. It can be an index over
the vault later; it cannot be the source of truth because it is opaque to git
review and hard to audit in plan approval.

## D-B - generation model

**Proposal: build the first generator inside kranz, using existing backend
sessions and deterministic file discovery; treat OpenWiki/RepoAgent as design
references, not dependencies.**

The first version should be a kranz mission/command that:

1. Reads `docs/knowledge/index.md` and the changed files since a chosen base.
2. Produces a proposed note patch and a `research.md` evidence artifact.
3. Runs drift checks named in each touched note's front matter.
4. Leaves normal git review to the operator; no auto-commit outside a mission.

OpenWiki/DeepWiki-Open can be evaluated in an optional spike to seed an initial
vault, but generated output must pass through the same Markdown/provenance
contract before becoming canonical.

## D-C - prompt consumption

**Proposal: inject a ranked, capped "Knowledge from this repo" block into
planning and revised-planning only, never into every worker turn by default.**

Initial cap: 4 KiB, separate from the existing 2 KiB lessons cap. Selection:

1. Always include `docs/knowledge/index.md` headings and the top-level map.
2. Include notes explicitly referenced by the ticket body or mission goal.
3. Include notes whose `verified_against` paths overlap the ticket's likely
   touch set or changed files.
4. Include lessons independently through the existing lessons channel; do not
   fold them into the same budget.

Workers and validators should receive knowledge excerpts only when the
approved plan names them in a feature brief or validation assertion. This keeps
the store from becoming prompt sludge and avoids shifting all missions toward
global-context overread.

## D-D - `research.md`

**Proposal: every draft may produce a per-mission `research.md`, and approved
plans over a configurable complexity threshold must produce one.**

`research.md` lives beside `plan.md` and `plan.json` on the mission branch. It
is not runtime state; it is an audit artifact. It should contain:

- Files and docs actually read.
- External sources consulted, if any.
- Important facts extracted, each tied to a path, command, or URL.
- Ambiguities and stale docs found.
- Candidate updates to `docs/knowledge/`.

Relationship to the persistent store: `research.md` is both input from the
store and feed back into it. A later knowledge-refresh mission can mine
accepted research artifacts, but nothing moves automatically without review.

## D-E - freshness and trust

**Proposal: every note carries freshness metadata and an executable or
file-based drift check; stale notes are still browseable but are not injected
automatically.**

Freshness levels:

- `live`: checked in the last successful knowledge refresh or tied to a stable
  invariant such as "kranz never pushes".
- `check-on-touch`: trusted until one of its `verified_against` paths changes.
- `stale`: visible to humans, excluded from automatic prompt injection unless
  explicitly requested.

Drift checks can be commands or path probes. Commands must follow the existing
anti-vacuity lesson when they assert test coverage. A failed drift check does
not rewrite the note; it opens a knowledge-refresh finding/ticket.

## D-F - relationship to lessons

**Proposal: lessons stay separate. The knowledge vault summarizes and links to
lessons, but `.kranz/lessons/` remains the append-only mission-capture lane.**

Reason: lessons are high-signal operator scars. They are intentionally short,
captured at completion, and injected with a tight cap. The knowledge vault is
broader and more editable. Mixing them would either bloat lessons or make the
knowledge store append-only forever. `docs/knowledge/lessons.md` should be a
curated index that links to lesson files and restates durable rules already
accepted into standing docs.

## Build slicing

1. **Research artifact and vault skeleton.** Add `docs/knowledge/` conventions,
   `research.md` rendering beside plan artifacts, and planning prompt language
   that asks for evidence and candidate knowledge updates. No automatic prompt
   injection yet.
2. **Knowledge selection and planning injection.** Add a pure selector that
   reads the vault, applies freshness and relevance filters, enforces the byte
   cap, and injects the result into planning and M2 revised-planning seeds.
3. **Refresh/drift command.** Add `kranz knowledge refresh` / server endpoint
   as a reviewable mission path: detect changed verified paths, run note drift
   checks, propose Markdown edits, and surface stale notes in dashboard/Slack.

Each slice should carry focused tests plus the full workspace gate. Dashboard
work begins only in slice 3, when there is a user-facing stale-note surface.

## Out of scope

- Vector search as a source of truth.
- Automatic commits to `docs/knowledge/` outside a mission or explicit command.
- Serving a full wiki UI in the first two slices.
- Replacing `.kranz/lessons/`.
- Sending the full vault to every model turn.
- Trying to make OpenWiki/RepoAgent output trusted without kranz review and
  provenance metadata.
- Cross-repo/fleet memory; this document is repo-local only.

## Done when

A new draft for a non-trivial ticket starts with a capped knowledge block that
names the relevant architecture notes, still includes the independent lessons
block, and avoids stale notes whose verified paths changed. The approved
mission branch contains `plan.md`, `plan.json`, and `research.md`; the research
artifact lists the files/sources read and candidate knowledge updates. A human
can open `docs/knowledge/index.md` in a plain Markdown viewer or Obsidian and
navigate the same facts the planner consumed. When a code path named in a
note's `verified_against` changes, `kranz knowledge refresh` reports the note
as check-needed or proposes a patch; it never silently injects that stale fact
into a later plan.

## Open questions

1. What exact threshold makes `research.md` mandatory: feature count, touch-set
   breadth, estimated cost, or the same considered-alternatives policy?
2. Should the selector read `docs/knowledge/` directly in the engine, or should
   it go through a generated `.kranz/knowledge/index.json` cache for speed?
3. Should note drift checks be allowed to run arbitrary shell commands, or only
   a safe allowlist of existing validation commands?
4. Does M2 revision consume the same knowledge cap as initial planning, or a
   smaller cap focused only on changed remainder scope?
5. Should `kranz ready` score repositories with a maintained knowledge vault
   once this feature ships?
