---
title: Lessons manifest/body split — index in prompts, bodies fetched on demand
priority: 2
schedule: once
---

## Progress
- **Slice 1 SHIPPED (af5b187):** manifest-only planning injection (verbatim
  bodies dropped) + git-history provenance filter (lesson must be added by a
  `[kranz] mission report` commit with a matching `Kranz-Mission` trailer).
  New `git_ops::commit_that_added`; `lessons::render_lessons_manifest` takes a
  provenance predicate. Acceptance pin met: a dropped uncommitted lesson never
  reaches the planning seed.
- **Slice 2 REMAINING:** mechanical top-N body fetch — reuse
  `contract_sweep::touch_set_includes` to overlap each provenance-clean
  lesson's authoring-mission `plan.json` touchSet against the current mission's
  touch_set/repo_refs, pulling only the top-N full bodies back into context. No
  agent-driven fetch surface.

## Goal
Planning prompts today ingest lesson FILE BODIES verbatim via
render_lessons_index. Split that into two layers: prompts get only a
lessons MANIFEST (title, one-line summary, mission id, date — cheap,
low-risk), and the orchestrator pulls a full lesson body into context only
when it decides a lesson is relevant — and only for lessons whose
provenance checks out (committed by an engine meta commit that passed the
contract sweep, not an arbitrary file sitting in .kranz/lessons/). This
shrinks the prompt-injection blast radius from "any lesson file on disk"
to "explicitly fetched, provenance-checked lesson," and cuts planning
prompt tokens as the lessons store grows.

## Context
Security follow-through: the 2026-07 hardening review found (and ba5de16
narrowed) a lessons-smuggle vector — lessons are the one surface that
persists agent-authored text into FUTURE missions' prompts, so they remain
the highest-value injection target even with the sweep exemption fixed.
Influence: Flu (Astro) registers a skill's description in context but
withholds the payload until explicitly loaded; same shape here. Study:
render_lessons_index + capture_lesson in orchestrator.rs, lessons.rs,
contract_sweep.rs is_mission_record_path (the enumerated lesson paths),
and how planning turns assemble context. Decide: does the orchestrator
request bodies via a tool turn, or does kranz pre-select top-N relevant
lessons mechanically (e.g. touch-set overlap)? Prefer the mechanical
pre-selection first — no new agent-driven fetch surface.

## Acceptance hints
- Planning prompts contain the manifest only; a full body appears in
  context only through the selection path, and only for lessons whose file
  matches the enumerated engine-authored paths with sweep-clean provenance.
- A lesson file dropped into .kranz/lessons/ outside the engine's commit
  flow never reaches a prompt (test pins this).
- Token count of a planning prompt with 50 seeded lessons drops
  measurably vs verbatim ingestion (assert manifest size bound).
- cargo test --workspace passes with new pins for manifest rendering and
  the provenance filter.
