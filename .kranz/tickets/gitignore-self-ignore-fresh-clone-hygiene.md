---
title: .kranz/.gitignore is self-ignoring — fresh clones lose runtime hygiene
priority: 3
schedule: once
---

## Goal
Decide and implement one of: (a) track `.kranz/.gitignore` (handle the
engine's append-churn when it materializes new rules, e.g. preview/ from
m-cde2e8's a4 — an append must not dirty the tree mid-mission), or (b)
teach ready.rs's gitignore_hygiene probe that engine-materialized rules
count (the engine writes the canonical template on init — the probe
currently credits only committed rule sources). Found by dogfooding
`kranz ready` on this repo: the probe reports "kranz runtime gitignore"
unmet even though every sentinel is locally ignored, because
`.kranz/.gitignore` ignores ITSELF and is untracked — a fresh clone has no
runtime ignore rules until the engine first writes them.

## Context
From the amm-readiness-projection dogfood run (2026-07-23). The tension:
AGENTS.md "Tracked vs runtime" is silent on `.kranz/.gitignore` itself;
the self-ignore line exists because the engine generates the file locally.
ready.rs's standard (rules must be committed to count) is defensible — a
fresh clone IS unprotected today — but so is the engine-materialize design.
Pick one; don't do both. Note the self-ignore line currently also makes the
file invisible to `git status`, which is how this went unnoticed.

## Acceptance hints
- Either `.kranz/.gitignore` is tracked AND the engine's append path
  tolerates a tracked file (test: init/mission on a repo where it is
  committed does not leave the tree dirty), or the probe accepts
  engine-materialized rules with a comment explaining the exception.
- `kranz ready` on this repo then reports the gitignore dimension honestly
  (Pass or a remedy that matches the chosen design).
