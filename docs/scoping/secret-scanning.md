# M5 scoping — Real secret scanning (gate the write; the log never forgets)

Status: scoped 2026-07-06, DESIGN-FIRST — the four flagged decisions (D-A…D-D) are the operator's;
review this doc before any mission is drafted against it. Roadmap contract: "real secret scanning
replacing the regex scrub" (docs/roadmap.md:146, M5).

## Why

One live incident and two confirmed unscrubbed paths, all from real operation:

1. **The committed serve.token (2026-07-05).** Commit `9b21172` — a hygiene sweep untracking runtime
   files — accidentally ADDED `.kranz/serve.token`. Fixed 55 seconds later by `81159c7`, but the value
   is visible in both diffs and was pushed: in remote history forever. Remediation stopped at
   burn+rotate. No scanner anywhere — merge gate, CI, pre-commit — had a chance to object.
2. **Final-gate contract-command output is never scrubbed.** `run_shell_command` output is embedded
   verbatim in `Finding.evidence` (orchestrator.rs:2488) and emitted as `ValidationFinding`
   (orchestrator.rs:2514-2519); `emit` (orchestrator.rs:558) does not scrub. A contract command that
   prints `env` or `.env` content lands unredacted in the append-only log, the WS feed, and report.md
   (whose evidence path is truncate-only, orchestrator.rs:3847-3854).
3. **Merge-gate output goes out raw.** `GateSuiteResult::Failed { output }` is verbatim stdout+stderr
   (merge_gate.rs:22-23), returned in the HTTP 422 body (crates/server/src/host.rs:613-615) — straight
   to the dashboard and Slack merge flows.

The threat model is not exotic: workers get a bare `Bash` allow (permissions.rs:150-160), the deny
list blocks push/sudo/curl but **nothing stops `cat .env` or `env`** (permissions.rs:39-53), and the
worker process inherits the operator's full environment — no `env_clear` (backend_claude.rs:605-618).
scrub.rs's own docs say the permission layer, not the regex, is the primary control (scrub.rs:1-47);
GETs and WS are tokenless until M6 (lib.rs:322-329), so anything missed has a wide blast radius.

**Root cause: scrubbing is per-emit-site discipline feeding an append-only log — every new emit path
is a new chance to forget, and there is no undo.**

## What exists (shipped — the "regex scrub")

`crates/engine/src/scrub.rs` (398 lines, engine-only, deliberately zero external deps — scrub.rs:8-11):
ordered rules for PEM blocks, GCP service-account JSON, twelve vendor prefixes (`sk-ant-`, `AKIA`,
`xox[baprs]-`, `gh[pos]_`, JWTs, …), auth headers, connection-string passwords, a generic assignment
catch-all, and an entropy gate (≥4.0 bits/char, ≥24 chars) (scrub.rs:81-167). Placeholder/UUID/git-SHA
allowlist (scrub.rs:205-244); scrub-before-truncate so a cut can't split a secret (scrub.rs:348-352).
Wired at: transcript writes (runner.rs:117), message deltas (runner.rs:147), worker reports
(runner.rs:336-339), the orchestrator turn choke point (`pump_turn`, orchestrator.rs:3095), decisions
(orchestrator.rs:572-580), spec re-scrubs. Everything downstream of events.jsonl — report.md, WS,
Slack, OTEL, CLI tail — inherits it.

All of it is hand-wired, site by site. Nothing guarantees the next emit path gets wired.

## Design principles

1. **Redaction gates the WRITE.** events.jsonl is append-only, single-writer, lock-guarded
   (event_log.rs:1-10, append at :251). Post-hoc redaction is impossible by construction, so scanning
   happens before the byte hits disk or wire — at ingest, never on read.
2. **Over-redaction is a nuisance; under-redaction is forever.** A false positive costs one waiver. A
   false negative lives in the log, the transcripts, and (via committed artifacts and `kranz/*` pushes,
   git_ops.rs:486) potentially remote history — see `81159c7`. Bias every boundary toward redacting.
3. **Detection here, containment in M7.** This build owns the CONTENT of what kranz writes and
   commits. What workers may read/write/reach is worker-sandboxing territory
   (docs/scoping/worker-sandboxing.md); see "Adjacent scopes".

## Scan-point architecture (the choke points)

| Choke point | Where | Today | This build |
|---|---|---|---|
| Event ingest | `emit` (orchestrator.rs:558) / `EventLog::append` (event_log.rs:251) | per-site scrubs upstream; the boundary itself scrubs nothing | **SCAN — primary gate**; closes Why 2 structurally |
| Transcript writes | runner.rs:117,147 | scrubbed | same boundary, upgraded detectors |
| Human ingress | goal (orchestrator.rs:322-327), steering (orchestrator.rs:1232-1234), ticket appends (ticket.rs:429-445) | unscrubbed, persisted/committed | **SCAN** — a pasted secret is otherwise permanent |
| Artifact commits | `commit_paths` (git_ops.rs:181) behind plan/report/lessons | content pre-scrubbed upstream, commit unchecked | **SCAN staged content** before every engine commit |
| Merge | `merge_mission` (merge.rs:49-80) + 422 body (host.rs:613-615) | no scan; raw output egress | **SCAN branch diff** (D-C) + scrub the 422 body |
| Slack egress | outbound.rs / format.rs:67-112 | zero scrub calls; relies on upstream | inherits — safe by construction once ingest gates |
| Dashboard WS | ws.rs:1-56 tails events.jsonl | inherits events | NO scan — anything visible there is already persisted; the fix is upstream |

Read-side auth (tokenless transcript REST, rest.rs:211-224; tokenless WS) stays M6's ("token required
on reads too", roadmap.md:164-165). This build shrinks what a read reveals; M6 shrinks who can read.

## D-A — which choke points gate (OPERATOR DECISION)

**Proposal.** Move scanning INTO the write boundary: `emit`/`EventLog::append` scans every event
payload; `commit_paths` scans staged content; human ingress is scanned like model output. Existing
per-site scrubs stay as belt-and-suspenders (cheap, already tested). Slack and WS get NO second scan
layer — they render the canonical, already-gated log; scanning there would invite parity drift.

**Recommendation: all four write-side gates (event ingest, human ingress, commit, merge); nothing
read-side.** The alternative — hardening emit sites one by one — already failed twice (Why 2, Why 3).

## D-B — detection engine: external binary vs built-in (OPERATOR DECISION)

The dependency-discipline lens, applied:

| Option | For | Against |
|---|---|---|
| trufflehog v3 | best-known coverage; live verification | AGPL-3.0 (likely disqualifying); verification means provider-API network calls — at odds with the no-egress posture. Verify at build time |
| gitleaks | MIT, single static binary, diff/history modes, maintained GH Action | Go binary ~10-20 MB (from-memory figure — verify at build time); cannot sit in the per-event hot path |
| built-in (evolve scrub.rs) | zero new deps — the recorded posture (scrub.rs:8-11); already in the hot path; Rust-native | curated patterns rot without care; we own the rule treadmill |

**Proposal.** Hybrid, split by the shape of the work. The WRITE path (per-event, latency-bound) stays
built-in: evolve scrub.rs into a detector module with rule ids, fingerprints, and a `secret.redacted`
audit event (rule + fingerprint, NEVER the value). The BATCH path (merge gate, CI, the future history
pass) invokes gitleaks as an optional external binary, probed like `KRANZ_CLAUDE_BIN`
(backend_claude.rs:183-194), falling back to the built-in detectors on the diff when the binary is
absent — **the gate never silently skips**.

**Recommendation: the hybrid.** No new crate dependency, no bundled binary, one optional tool the
operator can install; verify the from-memory license/size claims before deciding.

## D-C — the merge-gate and CI slot (OPERATOR DECISION)

**Proposal.** A dedicated secret-scan pre-gate in `merge_mission`, ahead of the gate suite: scan the
mission branch's diff against the merge base — cheap, fails in seconds, before any cargo gate spends
minutes. Mirror it as a CI job in ci.yml; the existing test pinning the gate list to ci.yml
(merge_gate.rs:253-269) extends to pin the scan gate too — **merge_gate.rs and ci.yml move in
lockstep, by contract**. The 422 body (host.rs:613-615) gets scrubbed regardless of slotting. Expose
the same scanner as `kranz scan [--staged|--range]` so a documented, opt-in pre-commit hook covers the
human lane.

Honest label: the merge gate would NOT have caught `9b21172` — that was a human commit to main, pushed
by a human (**kranz never pushes**; humans do). The CI job catches that lane post-push; the opt-in
hook catches it pre-commit. Layered, honestly labeled.

**Recommendation: dedicated pre-gate + CI job + `kranz scan`, all three.**

## D-D — false-positive flow (OPERATOR DECISION)

The asymmetry is the design driver: a wrongly-redacted string is a mission inconvenience; a
wrongly-passed secret is unrecoverable (principle 2).

**Proposal.**
- **Write path: no interactive waiver.** The emit hot path cannot block on a human. Redact, emit
  `secret.redacted` with rule id + fingerprint, move on. The operator tunes via the allowlist; the
  existing placeholder/UUID/SHA allowlisting (scrub.rs:205-244) carries over.
- **Merge gate: waiver by committed fingerprint.** A failed scan names fingerprints; waiving means
  adding the fingerprint to a tracked allowlist file on the mission branch, so the waiver itself
  appears in the merge diff and rides the existing allowlist-gated, spend-adjacent merge act. Where
  that file lives relative to the `.kranz/.gitignore` template (orchestrator.rs:4292-4304) and the
  `9b21172` tracked-content policy — verify at build time.

**Recommendation: as proposed.** No global "disable scanning" switch on any surface.

## Adjacent scopes (owned elsewhere — do not re-scope)

- **One-time open-source history scrub: OUT OF SCOPE, but UNBLOCKED BY this build.** Known hit: the
  burned serve.token value, visible in `9b21172`/`81159c7` diffs and pushed; going public ships it
  (public gating: roadmap.md:123). No doc anywhere describes a rewrite (grep clean). The batch scanner
  from D-B/D-C gives that pass its detector — scan ALL refs including `kranz/*` branches (they carry
  engine-committed plan/report/lesson artifacts), rewrite-or-accept each hit, once, human-driven.
- **sandbox-2-write-audit** owns worker WRITE surfaces: touch-set diff sweep, `out-of-contract-write`
  findings, scratch-HOME env hygiene. This doc owns secret CONTENT in output streams and committed
  artifacts. The missing `env_clear` (backend_claude.rs:605-618) is sandbox-2/M7 territory — named
  here as threat model, not fixed here.

## Build slicing (each a single mission brief)

1. **Close the confirmed leaks + centralize the gate.** Scan at `emit`/`EventLog::append`; scrub
   ValidationFinding evidence (orchestrator.rs:2488→2519), report evidence (orchestrator.rs:3847-3854,
   truncate→scrub), the 422 gate body (host.rs:613-615), and human ingress. Pure Rust engine+server
   work — gate `--workspace`, per the repo meta-lesson.
2. **Detector upgrade (D-B, write side).** scrub.rs → detector module: rule ids, fingerprints,
   `secret.redacted` audit events, allowlist plumbing. Consumes slice 1's gate placement.
3. **Merge/CI scan gate (D-C) + waiver flow (D-D).** Pre-gate in `merge_mission`, ci.yml job in
   lockstep, `kranz scan` CLI, external-binary probe with built-in fallback, committed fingerprint
   waivers. Consumes slice 2's detector interface.

Sequencing: slice 1 lands alone; slice 2 consumes 1; slice 3 can start once 2's detector interface is
fixed. All three are engine/server/CI — no dashboard work.

## Out of scope

The one-time history rewrite (own ticket, pre-public), read-side auth tokens (M6), worker env hygiene
and out-of-contract write detection (sandbox-2), fs/network containment (sandbox-3..5), live credential
verification via provider APIs (trufflehog-style network egress), OTEL pipeline changes (inherits event
gating), any change to merge's human trigger or the approval gate's semantics, any change to
**kranz never pushes**.

## Done when

The operator plants canaries — a fake vendor-prefixed key in an exported env var, a high-entropy token
in a repo `.env` — and runs a mission whose worker prints both and whose contract command fails while
echoing them: grep of events.jsonl, every transcript, report.md, the dashboard WS feed, and the Slack
thread finds ZERO canary bytes, and each redaction produced a `secret.redacted` audit event. A mission
branch carrying a committed canary is refused at merge from BOTH the dashboard and Slack with a
scrubbed error body, and the same commit fails the CI job; committing the fingerprint waiver on the
branch makes the same merge land. `kranz scan --range` over history finds the burned serve.token in
`9b21172`, proving the pre-public pass has its tool. Emit-path overhead on a live mission stays under
a bound agreed at slice-1 review (target single-digit percent — measure, don't assume).

## Open questions

1. gitleaks/trufflehog/ripsecrets license/size/maintenance claims are from-memory — verify before D-B.
2. Emit-path scan latency sits under the single-writer lock — measure on a live mission in slice 1.
3. Committed-allowlist location vs the `.kranz/.gitignore` template (orchestrator.rs:4292-4304) —
   probe before D-D's waiver file lands.
4. Whether ticket appends (ticket.rs:429-445) can reuse the event-ingest gate — verify, don't assume.
