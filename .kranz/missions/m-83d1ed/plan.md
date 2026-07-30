# Mission plan — m-83d1ed

**Goal:** Harden the Gas City beads bridge translator (packaging/gascity/bin/) so its status verbs match the live bd dialect, claims are lease-aware with liveness-first recovery, brief field translation is type-correct, and all of it is proven by a round-trip fixture test against a live bd.

Branch `kranz/mission-m-83d1ed` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$9.10 – $45.49** (expected ~$20.95). Rough estimate — live usage is authoritative; based on 49 completed mission(s).

Context-fit check (corpus p90 over 54 plan(s): 4220 chars / 22 files):
- **f-2-1** (Round-trip fixture harness plus bridge hygiene check script) — spec is 4581 chars (corpus p90: 4220)

## Considered alternatives

**Chosen approach:** Probe the live bd dialect and commit the evidence FIRST as its own milestone, then land the test harness and the translator fix as file-disjoint siblings, then add lease-aware claiming last. This ordering is mandated by docs/scoping/beads-workstore.md:176, which flags the gc-vendored dialect as unverified and requires Brief 1 to verify against a live install before relying on any command shape; it means no later feature designs against an assumed verb. Splitting the harness (packaging/gascity/test/**) from the translator (packaging/gascity/bin/**) lets both run in parallel without collision while keeping the gate authored against target behaviour rather than retrofitted to whatever the implementation happened to do. Claims come last because they are the one part that can be blocked outright by a missing upstream capability, and the plan is built so that block is an honest stop rather than an invented mechanism.

Rejected shapes:
- **Implement all four ticket design items in a single feature over the three ~50-line shell scripts, with one round-trip test at the end.** — Rolls an unverified-capability risk (does `bd update --claim` even exist?) into the same unit as three safe mechanical fixes, so a claim/lease block would strand the status-verb and field-translation work that has no dependency on it.
- **Skip the live-bd probe and write the translator against upstream beads documentation, testing with a mock `bd` shim on PATH.** — Exactly the failure mode the scoping doc flags and lesson m-b66d34 records: a mock never exercises the real CLI boundary, so every assertion passes green while the deployed `gc bd` path is broken by a flag shape the vendored binary rejects.
- **Exercise the real `gc bd` wrapper end-to-end against a fixture Gas City created by the test.** — `gc init` registers a machine-wide launchd supervisor and spawns a live `claude --dangerously-skip-permissions --effort max` session (docs/gascity.md:48), with `gc stop` known to orphan tmux servers — an unacceptable side effect for autonomous test setup on the operator's machine.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** No occurrence of the stale `bd set-state` verb remains anywhere under packaging/gascity/bin/, and no script under packaging/gascity/ invokes `gc init` or `gc stop`.
  `bash packaging/gascity/test/check-bridge-hygiene.sh`
- **[a2]** The round-trip fixture test executes against a LIVE bd binary and passes: a fixture bead is created, claimed, driven through status transitions in both directions, and closed. A clean skip when bd is absent does NOT satisfy this assertion, because the PASS marker is emitted only on a real live run.
  `bash packaging/gascity/test/kranz-dispatch-roundtrip.sh 2>&1 | grep -qE 'ROUNDTRIP: PASS \(live bd '`
- **[a3]** The round-trip test proves lease-aware dead-claim recovery: a claim whose holder is dead becomes claimable again, while a claim whose holder is verifiably live is never stolen (liveness first, expiry only as backstop).
  `bash packaging/gascity/test/kranz-dispatch-roundtrip.sh 2>&1 | grep -qE 'LEASE: PASS \(dead-claim recovered, live-claim preserved\)'`
- **[a4]** The bidirectional bead-status to kranz-state mapping table is documented explicitly in the translator script headers, covering all four bead statuses (open, in_progress, blocked, closed).
  `bash packaging/gascity/test/check-bridge-hygiene.sh --status-map`
- **[a5]** A committed dialect-evidence document records the actual probed output of a live bd install (version plus the exact verified flag shapes for ready/show/update/claim/comment/close), rather than command shapes asserted from upstream documentation. *(agent judgement)*
- **[a6]** Brief translation is type-correct for acceptance_criteria: a string value is carried verbatim into `## Acceptance hints` and a JSON-array value is unwrapped into newline-joined entries, so raw JSON text never leaks into the mission brief.
  `bash packaging/gascity/test/kranz-dispatch-roundtrip.sh 2>&1 | grep -qE 'FIELDS: PASS \(acceptance_criteria string\+array\)'`
- **[a7]** This is a packaging/docs-only mission: no Rust crate source and no dashboard app source is modified relative to the pinned base commit.
  `test -z "$(git diff --name-only $KRANZ_BASE_SHA -- crates apps)"`
- **[a8]** The full Rust workspace test suite passes (bare exit code, never piped).
  `cargo test --workspace`
- **[a9]** Clippy is clean across the workspace with warnings denied (bare exit code, never piped).
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a10]** Formatting is clean across the workspace (bare exit code, never piped).
  `cargo fmt --all --check`
- **[a11]** The workspace builds (bare exit code, never piped).
  `cargo build --workspace`
- **[a12]** The round-trip fixture test creates no global state: it operates entirely within a throwaway directory under $TMPDIR, never touches an existing Gas City or the operator's real bead store, and cleans up after itself even on failure. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

note: contract lint ran against a working tree with uncommitted changes; results may not reflect the pristine base
author-bug suspects (already pass / no verdict on the untouched base): [a7] test -z "$(git diff --name-only $KRANZ_BASE_SHA -- crates apps)"
base-expected-to-fail (benign): [a1] bash packaging/gascity/test/check-bridge-hygiene.sh; [a2] bash packaging/gascity/test/kranz-dispatch-roundtrip.sh 2>&1 | grep -qE 'ROUNDTRIP: PASS \(live bd '; [a3] bash packaging/gascity/test/kranz-dispatch-roundtrip.sh 2>&1 | grep -qE 'LEASE: PASS \(dead-claim recovered, live-claim preserved\)'; [a4] bash packaging/gascity/test/check-bridge-hygiene.sh --status-map; [a6] bash packaging/gascity/test/kranz-dispatch-roundtrip.sh 2>&1 | grep -qE 'FIELDS: PASS \(acceptance_criteria string\+array\)'; [a8] cargo test --workspace; [a9] cargo clippy --workspace --all-targets -- -D warnings; [a10] cargo fmt --all --check; [a11] cargo build --workspace


## Milestone 1 — The live bd dialect is verified and recorded

### 1.1 Probe the live bd/gc bd dialect and commit the evidence

You are hardening the Gas City <-> kranz beads bridge. Before any command shape in the bridge scripts is changed, the actual dialect of the INSTALLED bd must be verified against a live binary and recorded. This is mandated by docs/scoping/beads-workstore.md:176, which flags the gc-vendored bd dialect version as unverified and requires Brief 1 to verify against a live install before relying on any command shape.

Both binaries exist on this host: `bd` and `gc` are on PATH (observed at /opt/homebrew/bin). PATH crosses kranz's env-clear boundary (crates/engine/src/command_exec.rs:352, asserted at :553), so you can reach them.

Create `docs/scoping/beads-bridge-dialect.md` recording the VERBATIM output (or a faithful excerpt) of read-only probes. Probe at minimum:
  - `bd --version` and `gc --version`
  - `bd --help` (top-level subcommand list)
  - `bd ready --help` (does it accept `--label` and `--json`?)
  - `bd show --help` (does `--json` return a bare object or a single-element array?)
  - `bd update --help` — CRITICAL: does it accept `--status`? Does it accept `--claim`? Are there lease/heartbeat flags (e.g. `--lease`, `--lease-ttl`, `--heartbeat`) or fields (`lease_expires_at`, `heartbeat_at`)?
  - `bd comment --help` and `bd close --help` (confirm docs/gascity.md:45: close takes `--reason`, comment takes the text positionally, `-m` belongs to neither)
  - `bd set-state --help` (confirm whether the verb exists at all on this version)
  - whether `gc bd <subcommand> --help` is a faithful pass-through to the same shapes

Use ONLY read-only probes: `--help` and `--version`. Do NOT create, update, claim, comment on, or close any real bead. Do NOT run `gc init` under any circumstance — docs/gascity.md:48 records that it registers a machine-wide launchd supervisor and immediately spawns a live `claude --dangerously-skip-permissions --effort max` session, and that `gc stop` can orphan tmux servers. Do NOT run `gc stop`.

Structure the document as: (1) probed versions; (2) a table of verified command shapes, one row per subcommand, marked VERIFIED with its flag list; (3) an explicit section on claim/lease support answering yes or no with evidence; (4) a section noting any shape the spike currently uses that the live binary does NOT accept.

ESCALATION REQUIREMENT — do not guess and do not substitute: if `bd update --claim` does not exist, or no lease/TTL/heartbeat mechanism exists, record that finding plainly in the document under the claim/lease section, state in your WorkerReport's knownGaps that the lease-aware claim milestone cannot be implemented as specified, and do NOT invent a substitute mechanism (no emulating leases via comments, metadata, or sentinel files). Recording an honest negative result is a successful outcome for this feature.

Also note in the document, for downstream features, whether `bd init` works standalone in an arbitrary empty directory to create a self-contained store (probe `bd init --help` only — do NOT actually init anywhere yet).

Touch only `docs/scoping/beads-bridge-dialect.md`. Do not modify the bridge scripts in this feature.

Done when:
- docs/scoping/beads-bridge-dialect.md exists and records the probed `bd --version` output from the live binary
- The document contains a per-subcommand table of VERIFIED flag shapes for ready, show, update, comment, and close
- The document contains an explicit claim/lease section stating yes or no, with the probe output as evidence
- The document states whether `bd set-state` exists on the installed version
- The document states whether `bd init` can create a self-contained store in an arbitrary empty directory
- No real bead was created, updated, claimed, commented on, or closed, and neither `gc init` nor `gc stop` was invoked


## Milestone 2 — The translator speaks the verified dialect and is proven by a live round-trip

### 2.1 Round-trip fixture harness plus bridge hygiene check script

Build the test harness that gates the Gas City beads bridge. Read `docs/scoping/beads-bridge-dialect.md` FIRST (committed by the previous milestone) and use only the command shapes it marks VERIFIED — do not invent flag shapes from upstream docs.

Context: the bridge scripts are `packaging/gascity/bin/kranz-dispatch` (drains ready kranz-labeled beads, spools a mission brief, exits fast because `gc order exec` enforces a context deadline), `packaging/gascity/bin/kranz-city-worker` (long-lived serial drain loop), and `packaging/gascity/bin/kranz-run-bead` (runs one mission, maps kranz exit codes 0/2/3/other back to bead state). All are POSIX `/bin/sh`.

Create TWO scripts. Note the sibling feature in this milestone changes the bin/ scripts; write your harness against the TARGET behaviour described here, so it will fail until that feature lands. That is expected.

(1) `packaging/gascity/test/kranz-dispatch-roundtrip.sh` — a live-`bd` round-trip fixture test.
  - Isolation is mandatory: create a throwaway working directory under `$TMPDIR` (`mktemp -d`), initialise a self-contained bead store there (per the dialect doc's `bd init` finding), and create a fixture git repo checkout inside it to serve as the rig. Never touch an existing Gas City, the operator's real bead store, or anything outside the temp dir. Clean up with a `trap ... EXIT INT TERM`, even on failure. NEVER invoke `gc init` or `gc stop` (docs/gascity.md:48: launchd supervisor plus a live max-effort Claude session).
  - When `bd` is not on PATH, print `ROUNDTRIP: SKIP (bd not on PATH)` and exit 0. On a real run print `ROUNDTRIP: PASS (live bd <version>)` only after every case below passes. This marker discipline is load-bearing: the validation contract greps the PASS marker, so a skip must never emit it.
  - Cases to cover: create a fixture bead; drive it through status transitions in BOTH directions across open/in_progress/blocked/closed using the verified `bd update --status` shape; assert `bd set-state` is never invoked by the bridge; close it and assert the close carried its reason.
  - Field-translation case: verify `kranz-dispatch`'s brief generation is type-correct for `acceptance_criteria`. Exercise BOTH a string-valued and a JSON-array-valued `acceptance_criteria`, asserting the generated brief's `## Acceptance hints` section carries the string verbatim in the first case and newline-joined entries in the second, with NO raw JSON bracket or quote text leaking through. Also assert the `bd show --json` outer-envelope unwrap still works for whichever of bare-object or single-element-array the dialect doc recorded. On success print `FIELDS: PASS (acceptance_criteria string+array)`.
  - Emit a distinct `LEASE:` marker line for the lease cases, but in THIS feature the lease cases may be stubbed to print `LEASE: SKIP (not yet implemented)`; the next milestone fills them in.
  - Structure the script so cases are individually named and a failure prints which case failed and the observed versus expected value. Use `set -u`; do not use bashisms if you declare `#!/bin/sh`.

(2) `packaging/gascity/test/check-bridge-hygiene.sh` — a static hygiene checker returning 0 when clean.
  - Default mode: exit 0 only if NO file under `packaging/gascity/bin/` contains `set-state`, AND no file under `packaging/gascity/` invokes `gc init` or `gc stop`. Print each violation with its file and line. This script exists specifically so the validation contract never needs a leading shell `!` for negation — kranz's preflight probes an assertion command's leading token for a PATH lookup and reports `'!' not found on PATH`, so `! grep ...` is not an acceptable contract form in this repo. Encode the negation in this script's own exit code.
  - `--status-map` mode: exit 0 only if the translator script headers document the bidirectional mapping table covering all four bead statuses (open, in_progress, blocked, closed) and their kranz-side meanings. Print what is missing.
  - Make both modes robust to being run from the repo root (the contract runs them as `bash packaging/gascity/test/<script>`).

Also mark both scripts executable, register the round-trip script in `.kranz/merge-gates.json` as a gate so it cannot rot, and note in the pack README that a merge-gate skip is silent on `bd`-less hosts whereas the mission contract fails on a skip.

Touch only: `packaging/gascity/test/kranz-dispatch-roundtrip.sh`, `packaging/gascity/test/check-bridge-hygiene.sh`, `.kranz/merge-gates.json`, `packaging/gascity/README.md`. Do not modify `packaging/gascity/bin/**` in this feature.

Done when:
- `bash packaging/gascity/test/check-bridge-hygiene.sh` exits nonzero while `set-state` is still present in packaging/gascity/bin/, and exits 0 once it is gone
- `bash packaging/gascity/test/check-bridge-hygiene.sh --status-map` exits nonzero until the four-status bidirectional table is present in the script headers
- Neither check script uses a leading shell `!` to express its negation; the negation is in its own exit code
- `bash packaging/gascity/test/kranz-dispatch-roundtrip.sh` prints `ROUNDTRIP: SKIP (bd not on PATH)` and exits 0 when bd is absent, and never prints a PASS marker in that case
- The round-trip script runs entirely inside a `mktemp -d` directory and removes it via a trap on EXIT, INT and TERM
- The round-trip script never invokes `gc init` or `gc stop`
- The round-trip script exercises acceptance_criteria as both a string and a JSON array and asserts no raw JSON text reaches `## Acceptance hints`
- `.kranz/merge-gates.json` includes the round-trip script as a gate and remains valid JSON

### 2.2 Correct status verbs, bidirectional map, and type-correct field translation

Harden the three bridge scripts under `packaging/gascity/bin/`. Read `docs/scoping/beads-bridge-dialect.md` FIRST and use only command shapes it marks VERIFIED.

Three changes:

(1) STATUS VERBS. `packaging/gascity/bin/kranz-run-bead` line 25 currently reads:
      bd set-state "$ID" blocked >/dev/null 2>&1 || bd update "$ID" --status blocked >/dev/null 2>&1
`set-state` is stale (docs/scoping/beads-workstore.md:98 records it as drift against current upstream, and the fallback `bd update --status` is the correct form). Replace it with the verified `bd update --status blocked` form ONLY — remove the stale verb and the `||` fallback chain entirely, so no `set-state` string survives anywhere under packaging/gascity/bin/. Note that `kranz-dispatch` line 75 already uses the correct `gc bd update "$ID" --status in_progress` form; leave that shape intact.

(2) BIDIRECTIONAL STATUS MAP. Add an explicit mapping table to the header comment of both `kranz-dispatch` and `kranz-run-bead`, covering all four bead statuses in both directions: open <-> kranz Queued, in_progress <-> kranz Running, blocked <-> kranz Blocked-report, closed <-> kranz Done. Document honestly that the kranz exit-code mapping is many-to-one on `open`: exit 3 (refused as underspecified) and exit 1/other (mission FAILED) BOTH return the bead to `open` for re-readying, and say why. Keep the existing exit-code contract comment (0 close, 2 escalate, 3 refine-and-reopen, 1 reopen) consistent with the new table. `packaging/gascity/test/check-bridge-hygiene.sh --status-map` checks this table is present — read that script to see exactly what it looks for.

(3) TYPE-CORRECT acceptance_criteria. `kranz-dispatch` line 61 currently reads:
      ACCEPT=$(printf '%s' "$BEAD" | jq -r '(if type == "array" then .[0] else . end) | .acceptance_criteria // ""')
The `if type == "array"` guard there unwraps the OUTER `bd show --json` envelope, not the criteria value. If `acceptance_criteria` is itself list-typed, `jq -r` emits raw JSON array text straight into the brief's `## Acceptance hints`. That is a real corruption risk: docs/gascity.md:31 records that a malformed brief made an orchestrator confidently build the wrong thing and pass its own contract. Fix it so a string value is carried verbatim and an array value is unwrapped into newline-joined entries (one per line), with no brackets or quotes leaking. Preserve the existing outer-envelope unwrap, the existing `// ""` empty-default behaviour, and the downstream fallback text `${ACCEPT:-The goal above is demonstrably met.}` on line 69. Apply the same type-correct treatment to `title` and `description` extraction if the dialect doc shows either can be non-scalar; otherwise leave them.

Constraints: these are POSIX `/bin/sh` scripts — match the surrounding idiom (the existing `set -u`, the `bd()` wrapper in kranz-run-bead, quoting style, comment density). Do not restructure the dispatch/spool/worker architecture, do not add dependencies beyond the already-required `jq`, and do not add claim or lease logic (that is the next milestone). Do not add `external_ref` provenance or write the kranz mission id back as a comment — that is the sister ticket `beads-bridge-provenance-return-path` (Brief 2), explicitly out of scope here.

Verify your work by running `bash packaging/gascity/test/check-bridge-hygiene.sh`, `bash packaging/gascity/test/check-bridge-hygiene.sh --status-map`, and `bash packaging/gascity/test/kranz-dispatch-roundtrip.sh` (the harness is committed by the sibling feature in this milestone; if it is not yet present, state that in your report rather than writing your own copy of it).

Touch only: `packaging/gascity/bin/kranz-run-bead`, `packaging/gascity/bin/kranz-dispatch`, and if a header-comment fix is needed `packaging/gascity/bin/kranz-city-worker`. Do not modify anything under `packaging/gascity/test/`.

Done when:
- No occurrence of the string `set-state` remains under packaging/gascity/bin/
- kranz-run-bead sets the blocked state via the verified `bd update --status blocked` form with no fallback chain
- Both kranz-dispatch and kranz-run-bead header comments contain a bidirectional table covering open, in_progress, blocked and closed with their kranz-side meanings
- The header comments document that exit 3 and exit 1/other both return the bead to `open`, and why
- A string-valued acceptance_criteria reaches `## Acceptance hints` verbatim
- A JSON-array-valued acceptance_criteria reaches `## Acceptance hints` as newline-joined entries with no brackets or quotes
- The outer `bd show --json` envelope unwrap and the `${ACCEPT:-...}` fallback both still work
- No claim, lease, or external_ref logic was added


## Milestone 3 — Claims are lease-aware with liveness-first dead-claim recovery

### 3.1 Atomic claim at dispatch with heartbeat during mission execution

Add lease-aware claiming to the Gas City bridge. Read `docs/scoping/beads-bridge-dialect.md` FIRST.

HARD PRECONDITION — read this before writing any code: if the dialect document records that `bd update --claim` does NOT exist, or that no lease/TTL/heartbeat mechanism exists, then STOP. Do not implement a substitute (no emulating leases with comments, metadata, sentinel files, or status abuse). Write a WorkerReport whose result reflects the block, state the specific missing capability in knownGaps, and change nothing under packaging/gascity/bin/. An honest block here is the correct outcome; an invented mechanism is a failure.

Current state: there is NO claim at all today. `kranz-dispatch` does `gc bd ready --label "$LABEL" --json` (line 43), spools a brief, then flips `gc bd update "$ID" --status in_progress` (line 75). Status is not first-wins, so two dispatchers can both spool the same bead.

The architecture constrains where the heartbeat can live. `kranz-dispatch` MUST exit fast — `gc order exec` enforces a context deadline and killed a mid-mission dispatcher at ~60s (docs/gascity.md:24). The mission itself runs minutes-to-hours later under `kranz-city-worker` (long-lived serial drain loop, 15s sleep when the spool is empty) calling `kranz-run-bead`. So:

(1) CLAIM AT DISPATCH. In `kranz-dispatch`, claim each ready bead atomically using the verified `bd update --claim` shape BEFORE spooling it. If the claim fails (another dispatcher won), skip that bead silently and continue the loop — do not comment, do not spool. Claim before writing spool files so a lost race leaves no orphan spool entry. Keep the existing scrutiny-floor refusal and rig-routing checks ahead of the claim so a refused bead is never claimed.

(2) HEARTBEAT DURING EXECUTION. In `kranz-run-bead`, hold the claim alive for the mission's duration: start a background heartbeat subshell that renews the lease at an interval safely under the TTL, and kill it with a `trap ... EXIT INT TERM` so a dying runner stops renewing and the lease lapses. Use the verified renew/heartbeat shape from the dialect doc. The heartbeat must not write to stdout — `kranz-run-bead`'s stdout discipline matters because `kranz-city-worker` reads its output.

(3) LIVENESS-FIRST RECOVERY. Mirror kranz's own queue posture (see .kranz/tickets/queue-liveness-over-age.md): a claim held by a verifiably LIVE holder must never be stolen; expiry is only the backstop for an ambiguous or dead holder. Document this posture in the header comment. Also document honestly the one residual exposure: a bead claimed and spooled but not yet drained is covered by TTL alone, because nothing heartbeats between dispatch exit and drain start.

(4) RELEASE ON TERMINAL STATE. Ensure reaching any terminal state (close, blocked, reopen) stops the heartbeat and leaves no lease dangling.

Then extend the lease cases in `packaging/gascity/test/kranz-dispatch-roundtrip.sh`, replacing its `LEASE: SKIP (not yet implemented)` stub. Prove both halves: a claim whose holder is dead becomes claimable again, AND a claim whose holder is verifiably live is NOT stolen. Also prove a second claim attempt by a different holder on a live claim fails, while repeating a claim you already hold is idempotent. On success the script must print exactly `LEASE: PASS (dead-claim recovered, live-claim preserved)`. Keep all existing markers (`ROUNDTRIP: PASS (live bd <version>)`, `FIELDS: PASS (acceptance_criteria string+array)`) working and keep the skip path emitting no PASS marker. Keep the whole test inside its `mktemp -d` sandbox and never invoke `gc init` or `gc stop`.

Constraints: POSIX `/bin/sh`, match surrounding idiom, no new dependencies beyond `jq`. Do not add `external_ref` provenance or mission-id return comments — that is Brief 2 (`beads-bridge-provenance-return-path`), out of scope.

Touch only: `packaging/gascity/bin/kranz-dispatch`, `packaging/gascity/bin/kranz-run-bead`, `packaging/gascity/bin/kranz-city-worker`, `packaging/gascity/test/kranz-dispatch-roundtrip.sh`, `packaging/gascity/README.md`.

Done when:
- kranz-dispatch claims each bead atomically via the verified claim shape before writing any spool file, and skips silently on a lost race leaving no orphan spool entry
- The scrutiny-floor refusal and rig-routing checks still run before any claim, so a refused bead is never claimed
- kranz-run-bead renews the lease from a background heartbeat killed by a trap on EXIT, INT and TERM, and writes nothing to stdout
- Reaching any terminal state stops the heartbeat and leaves no dangling lease
- The header comments document the liveness-first posture and the residual spooled-but-not-drained TTL-only exposure
- The round-trip test prints `LEASE: PASS (dead-claim recovered, live-claim preserved)` on a live run and proves a dead claim is recovered while a live claim is preserved
- A competing claim on a live claim fails, and re-claiming a claim already held is idempotent
- All pre-existing round-trip markers still pass and the bd-absent skip path still emits no PASS marker
- If the dialect doc records no claim or lease support, nothing under packaging/gascity/bin/ was changed and the block is reported in knownGaps
