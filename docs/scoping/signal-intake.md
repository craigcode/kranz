# Signal intake: production signals as governed mission proposals (scoping)

Status: scoped 2026-10-12, DESIGN-FIRST. Decisions marked OPERATOR DECISION
are the operator's; PROPOSED ones stand unless the operator changes them.
Review this note before drafting any `signal-intake-*` ticket.

## What this is

Today kranz work starts from tickets, from Slack, or from the GitHub webhook
trigger. This series adds a governed path for work that starts from a
production signal: an alert or error report from a monitoring tool. Kranz
does not detect anything. It owns the step between "a tool noticed something"
and "an agent may act on it", and it keeps the evidence for that step.

Two reasons to build it:

- **Injection resistance.** A monitoring payload can be attacker-shaped:
  anyone who can reach a public error-reporting endpoint can put text into it.
  Wired straight into a drafter or worker, that text steers the agent. The
  gate between signal and execution is the point of this work.
- **Defect linkage.** False greens (`escalation_metrics.rs`, `docs/metrics.md`
  §3) count missions that closed COMPLETED and later traced to a defect, but
  the `traced-from-mission` link is entered by hand. A signal that matches a
  merged mission's paths can suggest that link.

It fits the positioning ADR (`docs/knowledge/decisions/positioning-governance-evidence-layer.md`):
dispatch, gate, record, prove. It adds no execution primitive. Triage is
deterministic; no model reads signal content before a human has consented.

## What already exists (verified 2026-10-12)

- **The GitHub webhook trigger is the template.** `POST /api/repos/{id}/hooks/github`
  (`crates/server/src/hooks.rs`, `crates/engine/src/hooks.rs`,
  `docs/protocol.md` §Webhooks) verifies `X-Hub-Signature-256` in constant
  time, refuses everything when no secret is configured, normalises to a
  `Trigger`, dedupes by a deterministic slug, checks the payload's repository
  against the served repo's origin, and writes a ticket with a provenance
  block. It then drafts immediately (`draft_async`).
- **Its consent comes from a trusted actor.** A comment trigger fires only
  when a user on `hooks.allowUsers` posts the fix or queue label, so the
  labelled comment is the operator's consent to draft, and the queue label
  pre-consents the queue step. Plan approval is never skipped.
- **Untrusted text is scrubbed and bounded, not fenced.** Trigger excerpts
  (`bound_scrubbed`, 8 KiB raw, 1000 chars) and ticket notes go into the
  drafter's seed as plain text. The established fence is the validator's
  `<<<BEGIN/END KRANZ UNTRUSTED RUNTIME EVIDENCE>>>` block in `runner.rs`.
- **The event log is per mission.** `Event.mission_id` is required and logs
  live at `.kranz/missions/<id>/events.jsonl`, hash-chained and MACed with the
  repo authority key (`event_log.rs`). A signal exists before any mission, so
  it cannot be an `EventKind` without a schema change.
- **"Grant" means a mid-mission capability grant** (`grant.requested` /
  `approved` / `denied`; `GrantKind`). What a human approves to start work is
  the plan (`approve_plan`), after a draft. There is no "approve a proposal
  before drafting" step; drafting a ticket already spends.
- **Write-back precedent.** `EnqueueSource { producer, external_ref }`
  (`queue.rs`) joins a mission to an external producer, and the Gas City pack
  returns results with a pending receipt and a rename to
  `enqueue-source.returned.json` once confirmed (`packaging/gascity/`). That
  path auto-approves plans (`kranz exec --enqueue`); signal intake reuses the
  join and the receipt discipline, never the auto-approval.
- **The multi-repo catalogue** (`crates/server/src/multi.rs`, `HostConfig.repos`)
  and per-repo webhook URLs exist. Nothing maps a source's project to a repo.

## Invariants

1. **No signal drafts, queues or runs anything without a recorded human
   consent.** A signal produces a proposal: an undrafted ticket with
   `schedule: once` and an unconsented `signal` marker (`signal-proposal: unconsented`)
   that `kranz work`, `kranz ticket queue`, and `Scheduler::find_due_tickets`
   refuse to queue or run until an explicit human consent action (`kranz signal consent <id>`
   or `kranz draft --consent-signal` / authenticated operator UI action)
   records the `consented` ledger event. A test asserts there is no other path.
   Pre-consent labels never apply to signals.
2. **Signal content is data.** It reaches a drafter or worker only inside a
   per-instance nonced untrusted-data fence
   (`<<<BEGIN KRANZ UNTRUSTED SIGNAL EVIDENCE [<nonce>]>>>` ...
   `<<<END KRANZ UNTRUSTED SIGNAL EVIDENCE [<nonce>]>>>`, with a fresh 128-bit
   CSPRNG hex nonce per envelope and any literal `<<<BEGIN KRANZ UNTRUSTED` or
   `<<<END KRANZ UNTRUSTED` substrings stripped from the body before wrapping).
   `Ticket::parse_sections` ignores `## ` headings inside untrusted fences so
   excerpts cannot inject `## Goal`, `## Review artifact`, or other sections.
   The goal and title of a proposal are composed by triage only from strictly
   validated normalised fields (`source` matching `[a-z0-9_-]{1,32}`,
   `fingerprint` matching `[a-zA-Z0-9._:-]{1,128}`, `severity` in
   `critical|high|medium|low|info`, `project` matching `[a-zA-Z0-9._/-]{1,128}`
   with no `..`, `environment` matching `[a-zA-Z0-9._-]{1,64}`, and
   `evidence_links` parsed as `https://` URLs whose host matches the configured
   vendor host allowlist, with no whitespace, control chars, or markdown syntax
   `[`, `]`, `(`, `)`, max 4 links × 512 chars); any field failing validation
   is rejected or demoted into the fenced untrusted block, never interpolated
   into title, goal, or frontmatter.
3. **Raw payloads are bounded, scrubbed, and isolated** with source, receipt
   time and SHA-256: HTTP bodies are capped (e.g. 256 KiB) before buffering,
   sensitive headers/cookies/DSNs and secret tokens are scrubbed via
   `scrub::scrub_text` before persistence, `.kranz/signals/` is created mode
   `0700` with raw files mode `0600` under a bounded retention/disk quota
   (e.g. max 1,000 raw files or 64 MiB), and `.kranz/signals/` is listed in
   `sandbox::authority_read_deny_paths` so sandboxed workers, validators, and
   gates cannot read raw signal payloads.
4. **Unverified or replayed payloads are dropped** after a one-line decision
   log that carries neither secret nor body. Authenticity and replay bounds
   (timestamp header within a 5-minute skew window plus a bounded replay cache
   of `(source, delivery_id_or_sig_hash, timestamp)`) are checked first.
5. **Every intake decision is durable and replayable** in the signal ledger
   (D-A): received, triaged, proposed, consented, linked, written back.
6. **Write-back is idempotent and egress-pinned.** A crash and resume never
   double-posts, and outbound requests never follow payload-supplied URLs or
   redirects.

## Flow

1. **Receive.** A source adapter enforces the 256 KiB body cap, verifies the
   request signature and replay window/cache, and writes the scrubbed raw
   bytes to the mode-`0600` raw store and ledger under `.kranz/signals/ledger.lock`.
2. **Normalise.** Validate `source`, `fingerprint`, `project`, `severity`,
   `environment`, and `evidence_links` against strict character-class
   allowlists and length caps; wrap excerpts in a nonced untrusted fence after
   stripping any literal `<<<BEGIN KRANZ UNTRUSTED` / `<<<END KRANZ UNTRUSTED`
   markers.
3. **Triage**, deterministic: dedupe by fingerprint within a window, map the
   project to a repo through config, classify by declared rules, and list
   recent merged missions whose paths overlap the signal's frames.
4. **Propose.** An undrafted ticket with `signal:` provenance, `schedule: once`,
   `signal-proposal: unconsented`, a composed goal, nonced fenced evidence, and
   suggested `traced-from-mission` candidates.
5. **Consent.** A human consents and drafts it through an explicit consent
   action (`kranz signal consent <id>` or `kranz draft --consent-signal` /
   authenticated operator UI action). From there the normal plan approval and
   queue apply.
6. **Close.** When the linked mission completes, merges or fails, the result
   is written back to the source once.

Triage outcomes: **proposed**; **parked** (unknown project, or a rule asks for
a human look); **not a code change** (a declared rule matched an
infrastructure-only signal; recorded, no ticket).

## D-A. Where signal records live (PROPOSED)

A repo-level, append-only signal ledger at `.kranz/signals/ledger.jsonl`
(gitignored runtime, directory mode `0700`, file mode `0600`, serialised via
`.kranz/signals/ledger.lock`), with scrubbed raw bodies stored beside it by
hash under `.kranz/signals/raw/<sha256>` (mode `0600`, capped at 256 KiB per
payload and a bounded total quota such as 1,000 files / 64 MiB) and
`.kranz/signals/` added to `sandbox::authority_read_deny_paths`. Records are
hash-chained and MACed with the same authority key and primitives as the
mission event log, but the ledger is its own record type, so `events.rs` stays
untouched. Missions created from a signal carry the link in ticket
frontmatter, and the ledger records the mission id when consent happens.

Alternative: a repo-level `EventKind` stream. Rejected for now: it needs
`Event.mission_id` to become optional, a contract change across every reader.

## D-B. Where the adapter contract lives (PROPOSED)

An in-process Rust trait in engine core (`SignalSource`: verify, normalise,
write back), beside the webhook code, not under the gate-evaluation
subprocess contract. Verification needs the configured secret, enforces the
timestamp skew window and bounded `(source, delivery_id_or_sig_hash, timestamp)`
replay cache, and must run before any untrusted byte goes anywhere else, which
rules out handing raw requests to a pack subprocess. Vendor adapters are
domain-free (a vendor's public webhook format), so they pass the clean-room
test. The GitHub trigger is ported onto the trait as its first implementation,
with no behavior change.

## D-C. First source (OPERATOR DECISION)

Sentry issue alerts or New Relic alert webhooks: build whichever the
operator's telemetry actually uses. The adapter is written against the
vendor's current documentation for payload shape and signing, checked at the
time of the work, not from memory.

## D-D. Should GitHub triggers become proposal-first too? (OPERATOR DECISION)

Signal proposals never draft on their own. The GitHub trigger drafts on
receipt, which is defensible because an allow-listed user's labelled comment
is the consent. Options: keep it as is (recommended; its trust comes from the
actor, not the payload), or route it through proposals for one consistent
path. The CI-failure trigger has no human actor and is the case to look at
first.

## D-E. Dedupe window and batching (PROPOSED)

One proposal per fingerprint; repeats within the window increment a count on
the open proposal and add a ledger record. Window configurable per source,
default 24 hours. No digest batching until noise proves the need.

## D-F. Defect linkage (PROPOSED)

Triage suggests `traced-from-mission` candidates from path overlap with
recently merged missions. A human confirms one when drafting; nothing writes
the link automatically, because a wrong link corrupts the false-green rate it
exists to feed. Signal receipt to consent latency joins the outcomes report.

## D-G. What is written back (PROPOSED)

Status and links only: proposal created, mission completed, merged, failed or
declined, with the dashboard or branch reference. Write-back URLs are
constructed strictly from the operator-configured vendor base URL plus a
validated `[a-zA-Z0-9_-]` external issue ID (never following a raw URL from the
incoming payload), using `reqwest::redirect::Policy::none()` and `.no_proxy()`,
with the destination host validated against the configured vendor host. No
diffs, reports or transcript text leave the host. A failure or decline never
resolves the source issue.

## Build slicing (one mission brief each)

1. `signal-intake-1-untrusted-fence`: fence external text in ticket seeds,
   starting with the existing GitHub trigger excerpts. Independent; worth
   doing first.
2. `signal-intake-2-ledger`: the D-A ledger and raw store.
3. `signal-intake-3-source-contract`: the D-B trait, the GitHub trigger
   ported onto it, a stub adapter conformance suite. Blocked by 2.
4. `signal-intake-4-triage`: deterministic dedupe, project-to-repo mapping,
   classification rules, path-overlap candidates. Blocked by 2.
5. `signal-intake-5-proposal`: undrafted proposal tickets and the
   no-path-without-consent test. Blocked by 1, 3, 4.
6. `signal-intake-6-injection-suite`: a fabricated-signal corpus in CI.
   Blocked by 5. Must pass before any real source is connected.
7. `signal-intake-7-first-source`: the D-C vendor adapter. Blocked by 3, 6.
8. `signal-intake-8-linkage`: D-F confirmation at draft and the latency
   metric. Blocked by 5.
9. `signal-intake-9-write-back`: D-G, idempotent across kill -9. Blocked by 7.
10. `signal-intake-10-surfaces` (stretch): a dashboard signals view and Slack
    notification. Blocked by 5, 8.

2 to 5 are the critical path; 3 and 4 can run in parallel.

## Out of scope

Detection or monitoring of its own. Any automatic consent, at any severity.
Model-based triage. A second vendor adapter. Source-trust scoring (a
candidate follow-up). Changes to the plan-approval or grant models.

## Done when

A verified signal from the chosen source lands in the ledger, becomes one
undrafted proposal with fenced evidence and linkage candidates, can be drafted
only by a human, and its mission's outcome is written back once. The
injection corpus runs in CI and fails when the consent check is weakened.
