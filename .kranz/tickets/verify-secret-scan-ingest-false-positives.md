---
state: done
state-note: resolved in 90f6146 (Craig): fail-closed refusal + false-positive tests
title: Verify the always-on secret-scan ingest gate doesn't over-redact real content
priority: 3
schedule: once
---

## Goal
M5 (0b731d0) redacts every event's string payloads at append time
(append_with_redaction_audits is the default append path), running curated
rules PLUS an entropy detector (is_high_entropy_secret). Entropy detection is
false-positive-prone. It is narrowed to KEY = "value" assignment patterns
(entropy_assignment_re), which correctly excludes bare commit SHAs / base64 in
prose — but because the gate is ALWAYS ON for every mission, an over-redaction
corrupts legitimate content across all runs. Verify on real event logs (a few
completed missions' events.jsonl + report.md) that the ingest gate does not
redact legitimate high-entropy content (SHAs, base64 blobs, structured config,
diff hunks). Tune the assignment regex / entropy threshold / add contextual
allowlisting if the false-positive rate is non-trivial.

## Context
Found in the 0b731d0 M5 review 2026-07-07. The design is correct (redact-at-
write is the right append-only strategy; over-redaction is the safe failure
mode per D-D; fingerprint allowlist is the escape). This is not a defect — it
is the one thing to VERIFY empirically before trusting the always-on gate,
since a noisy entropy detector on the hot path degrades every mission's logs.
Also note: ingest scanning adds per-event regex+entropy cost on the write path
(worker.message deltas) — confirm it doesn't add meaningful latency at volume.

## Acceptance hints
- Run the ingest scan over several real missions' logs/reports; confirm zero
  (or reviewed-acceptable) redactions of legitimate content; a fixture test
  asserts a SHA/base64/diff-hunk sample is NOT redacted.
- Note write-path latency of the ingest scan at realistic event volume.
- cargo test --workspace green.

## Verification

- Added fixture coverage for legitimate high-entropy operational text: commit
  SHAs, SHA256 artifacts, base64 fixture prose, diff hunks, and structured
  sandbox config.
- Added committed-report sweep: `.kranz/missions/**/report.md` scanned with
  zero findings. Local run audited 39 reports in ~60 ms.
- Added an ignored lived-in checkout audit for gitignored runtime artifacts
  (`report.md` + `events.jsonl`). Local run audited 77 artifacts / 13.7 MB in
  ~1.7 s with zero findings.
