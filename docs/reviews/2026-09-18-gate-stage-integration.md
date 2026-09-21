# Gate stage integration review

This extends the initial approval checkpoint through proposed revisions,
milestone acceptance, final deliverable validation and local merge. S6 ACP
worker containment and S7 governed ACP acceptance remain separate release gates.
This is a five-axis self-review, not an independent audit or release approval.

## Correctness and authority

The first sealed approval pins checker declarations and dependencies. Live
configuration cannot remove that set; live-base manifest/checker drift refuses
merge even if no check executable changed. The proposed-revision path evaluates
the new plan under original checker authority and fresh explicit consent. Legacy
partial replanning refuses external gates because it does not replace the whole
approved plan of record.

Mechanical checks retain real process exits and source/environment bindings.
A boolean success is never converted into a claimed child exit. Bounded scrubbed
output travels with the receipt; generic commands do not invent assertion counts.
Milestone command assertions keep their existing advisory status. Final command
assertions and applicable tracked merge commands remain nonwaivable. Existing
native validation, Flight Rules, waiver rules and independent-review floors
still run. No checker result substitutes for required human stage consent.

Final checks see source after lesson/report writes and require engine-recorded
feature commits plus accepted independent review. The full source inventory is
rechecked after commands and after external evaluation, including dirty and
nonignored untracked bytes. Merge binds the scratch integration snapshot, live
base, candidate and actual integration tree. Consumption records an attempted
action; a separate outcome records whether local merge completed.
The committed report explicitly says it predates external final evaluation;
its native contract results do not claim mission completion.

## Recovery and security

The async stage driver shares the exact retention, containment and disposition
logic used by synchronous approval/merge. Its future can be dropped while the
existing subprocess guard owns cleanup. Resume closes unconsumed records instead
of rerunning checks or replaying effects. Passing stale attempts are explicitly
closed. A held event-log lock covers merge audit and the local ref update.
Host death cannot run a Drop handler. Private daemon-recovery ledgers remain an
operator responsibility; replay closure does not certify container cleanup.

Snapshots exclude provider state, authority files, Git metadata and mission
transcripts; commands use existing env-cleared and sandboxed runners. External
checkers retain their Docker namespace and pinned image/dependency restrictions.
Pending views escape untrusted output and offer investigation/retry, not a new
consent or failure-override channel. Unsupported external permission-stage
execution and Windows mission evaluators fail closed.

## Architecture, readability and performance

The adapters reuse existing lifecycle records, source snapshots, command runners,
PTY evidence, event replay and audit export. A small authority module centralizes
sealed-plan recovery and accepted feature provenance. Async stages keep the
runtime available; boxed stage futures avoid expanding the already large mission
state machine onto the thread stack. Source inventory and event folds add bounded
I/O at a gate boundary, not in the worker stream.

Merge commands currently execute twice when an external merge gate is configured:
the existing gate ladder, then the typed receipt pass. Both use existing bounded
runners. Capturing typed receipts in the legacy injectable executor is a future
optimization; removing either pass without preserving its authority and test
seam would change behavior.

## Validation

The synthetic Docker mission has passed initial approval, native independent
review, milestone acceptance, final evaluation and local merge. Primary source
and HEAD stay unchanged until merge. Separate tests cover blocking failures,
interruption without effect replay, revision consent, and a live-base advance
while a passing merge checker runs. Existing approval, snapshot, input-builder,
process-containment and replay tests remain required.
Runtime pack removal is rejected at config admission; a sealed historical
config change is also rejected against original approval authority at merge.

The dashboard has 247 passing tests, including pending/closed obligations,
expired attempts, escaped evaluator output and legacy empty states. Type checking,
build, embedded bundle synchronization/comparison and lint passed (existing lint
warnings remain outside these changes). The full workspace passed with real
Docker evaluator tests enabled: 3,010 passed, zero failed, ten existing ignored.
Workspace Clippy with warnings denied, formatting and build passed, as did all
16 schema checks, domain lint (971 tracked files before the roadmap update),
the staged secret scan and knowledge refresh. The prior Linux CI evaluator
failure remains unclassified; this checkpoint exposes the underlying refusal
in its diagnostic. CI must pass before acceptance. No provider calls or Keychain
access were used for this work; the already-reviewed Claude proof was brought
forward from PR #60 through the stack.
