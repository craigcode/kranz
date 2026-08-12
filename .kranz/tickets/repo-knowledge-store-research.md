---
state: done
state-note: research and slice-1 design shipped in 952cbc0; vault slice shipped separately
title: Deep research — persistent repo-knowledge store for agents and humans
priority: 2
schedule: once
---

## Goal

A research mission (deliverable: docs/scoping/repo-knowledge-store.md,
DESIGN-FIRST, D-X decisions flagged) answering what kranz needs and
wants from a persistent, human-and-agent-shared repo knowledge layer.
Today every draft re-derives the repo from scratch (10-20 minutes and
significant tokens per draft) and the lessons loop captures one line
per mission; there is nothing an operator can browse or an agent can
load as pre-digested context.

Questions the doc must answer:
- What kranz consumes: planner/draft context injection (cut draft time
  and tokens), worker briefs, validator grounding, operator browsing.
- Shape: an Obsidian-style markdown vault in-repo (plain .md with
  wikilinks, tool-agnostic, diffable, survives kranz) vs generated
  single brief vs embedding index — or layered combinations. Bias
  toward plain markdown in git unless research shows otherwise.
- Adopt vs build: evaluate existing tools — openwiki (operator
  suggestion: assess what it is, maturity, fit), Obsidian-compatible
  vault conventions, other agent-memory/knowledge-base systems worth
  borrowing from. Dependency-discipline lens applies.
- Freshness: who writes it and when — post-merge regeneration by a
  mission, drift-checks against the code, lessons-loop integration,
  human edits as first-class.
- Budgeting: how much of it enters a prompt, and how selection happens
  (whole brief vs queried sections).
- Trust: knowledge-store content is model-generated + human-edited;
  how errors get caught (it grounds planners — a wrong "fact" steers
  missions wrong silently).
- Per-mission research artifact: should each draft park a research.md
  beside plan.md capturing what the drafter learned and grounded on?
  Today that research is ephemeral (dies in the planning transcript),
  so the approve gate cannot audit the plan's grounding. Decide whether
  research.md is the per-mission feed INTO the persistent store, its
  input FROM it, or both.

## Context

Mission shape: research + doc deliverable, like m-d341a7 (Gas City
citizenship) — web research allowed for the tool evaluation, no code
changes beyond the doc. The lessons system (.kranz/lessons/, one
reusable lesson per mission) is the seed of this; the scoping doc
should say whether the store absorbs lessons or sits beside them.

## Acceptance hints

- docs/scoping/repo-knowledge-store.md exists in house scoping style
  (Why, D-X decisions with recommendations, build slicing, out of
  scope, done when), grounded in the actual draft/planning code paths
  it would feed.
- The adopt-vs-build evaluation names concrete candidates with
  maturity/fit assessments, including openwiki.
- No code changes.
