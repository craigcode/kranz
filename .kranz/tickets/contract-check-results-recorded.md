---
title: Record each contract command's exit code and output tail in the event log
priority: 2
schedule: once
---

## Goal

Every engine-run contract command leaves a machine-readable result in the mission's event log: which assertion, the exit code, whether it timed out or failed to start, and a bounded tail of its output. Two runs of the same failing check can then be compared without reading prose.

## Context

Reported from a mission run on 2026-11-15. The engine runs the contract's command assertions and hands the results to the functional validator, but the mission directory holds no exit code or stderr for any of them. The only durable trace is `validation.finding.evidence`, which is text a model wrote. Three findings about one failing check quoted the same error in three different wordings, and the severity changed on the third. Nothing in the log could show that the failure was identical each time.

The milestone gate also keeps only the stderr tail of a failed command. When a test suite fails, the failing test names are on stdout and are lost.

This is a prerequisite for `harness-fault-findings-block-milestone`.

## Scoping answers

- Add a new event (or additive fields) for a per-assertion command result. `events.rs` and `types.rs` change additively only, with `#[serde(default)]`.
- Record: milestone id, assertion id, stage, exit code or signal, a could-not-start or timed-out marker, duration, and bounded tails of stdout and stderr.
- Output tails pass through the same redaction and secret-scan rules as other captured output. Bound them by bytes.
- Include a stable digest of the normalised failure (exit code plus output tail) so "same failure as last round" is a comparison of two strings.
- This records what the engine already captures. It does not change what the validator is shown or how a verdict is reached.
- The review packet and the dashboard may render it later; that is out of scope here.

## Acceptance hints

- A mission test with one failing command assertion finds one result event carrying that assertion's id, a non-zero exit code and the stderr tail.
- A command that cannot start records the could-not-start marker, not an exit code.
- Running the same failing command in two validation rounds yields two results with equal digests.
- An old event log without these events still folds.
