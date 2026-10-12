---
state: open
title: Signal intake 6: injection corpus in CI
priority: 2
schedule: once
blocked-by: [signal-intake-5-proposal]
---

## Goal

Add a corpus of fabricated signals that try to steer the pipeline, and a CI suite proving each one yields at most a flagged, undrafted proposal with nothing executed.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 6. Must pass before any real source is connected (slice 7).

## Scoping answers

- Corpus covers embedded instructions, fake stack traces naming files to edit, package-install lures, fence-breaking marker text, oversized and malformed payloads, and look-alike repository names.
- Include the GitHub trigger path as well as the stub source.
- Follow the anti-vacuity rule in AGENTS.md: the suite's CI gate must fail on zero matched tests.

## Acceptance hints

- Every corpus item ends as a dropped, parked or undrafted signal; no draft, queue or process spawn occurs (asserted, not inferred from logs).
- A deliberately weakened consent check makes the suite fail (a test that flips the guard).
- The suite runs in CI on Linux and macOS.
