# Review-effort pilot protocol

Status: prepared on 2026-09-22, before pilot case execution. The three cases
below are a convenience sample for finding evidence gaps, not an efficiency
benchmark. The [pilot ticket](../../.kranz/tickets/review-effort-pilot.md) stays
open until the cases and final assessment are reviewed.

Use the shipped [review packet](../review-packets.md),
[baseline/candidate observations](../contract-controls.md#baseline-and-candidate-observations),
[outcome reasons](../outcome-reasons.md), and existing evidence export. No new
analytics service or execution primitive is proposed.

## Admission and identities

Start case execution after PRs #73, #74 and #75 have merged. Pin the actual
merged commit, CLI version, source tree, OS/architecture, tool versions,
containment profile and local fixture repository identities in the run record.
The integration head `3fb8a61815d97fa7542fe63a25480d4d642a2426` has tree
`4e9e2d256625b5956c1302ca6e2d63d1e8b3c1fb`; these identify the preparation
baseline, not an assertion that a later checkout is identical.

Select a named reviewer before opening a case and record whether that person
authored or saw the fixture or its answer key. Record any assistant's separate
contribution. Automated checks and the assistant's self-review do not count as
human review observations. This first exercise is not blinded or randomized;
disclose prior knowledge and order effects.

The ordinary ticket dependency gate requires Complete missions. These features
were delivered through PRs; closing their tickets does not invent those records.
If running the pilot through `kranz ticket approve`, first document the merged
prerequisites and obtain the operator's explicit choice of the documented
`--force` dependency override. Preparing this protocol does not invoke that path.

## Predeclared cases

Freeze each case's source, checking inputs, expected behavior and answer key
before review. Record their SHA-256 digests and Git identities. Later repairs
are new numbered rounds; retain every prior packet and receipt.

| Case | Task and risk | Required evidence | Seed and expected operator response |
| --- | --- | --- | --- |
| D1 — documentation | Low-risk clarification of a fixture CLI's local-only behavior; no executable change. | Approved scope, diff covering every changed document, links to the fixture CLI behavior and an explicit reason a baseline pair is not applicable. | A later edit adds an unsupported claim that a command publishes remotely. A check belongs to the earlier candidate. Identify the unsupported statement and stale evidence; request correction and current checks. |
| B1 — reproduced bug | Medium-risk synthetic authorization predicate: a wrong nonempty credential incorrectly permits a mutation. No real identities or services. | Freeze the checker before the fix. Positive and deliberately defective controls, exact failing baseline observation, passing candidate observation, matching source/check/configuration identities. | In a separate preparation variant, remove a required checker dependency. Report setup failure as inconclusive, never as reproduction of the authorization defect. The behavioral case must fail for the declared defect and pass after repair. |
| Y1 — brownfield parity | Medium-risk replacement of a small fixture's legacy parser while preserving its documented quirks. | A characterization pass committed before the replacement, fixed replay inputs and expected outputs, baseline/candidate parity, plus evidence that the production entry point no longer uses the old parser. | A candidate passes parity through the new function while the entry point still calls the old implementation. Identify the incomplete migration; parity alone cannot establish cutover. |

Use separate scratch repositories with no production data or network dependency.
Keep tasks small enough for one reviewer to inspect the entire diff. Do not turn
synthetic authorization or parser fixtures into changes to Kranz's production
authorization or parsing code. D1 has no behavioral pair; B1 establishes a
specific failure; Y1 expects parity. Their different workloads prevent a causal
comparison of elapsed time.

## Evidence and review procedure

1. Assign the case/reviewer and freeze the selection record, accepted scope,
   fixtures, expected observations and answer key. Keep the key outside the
   review packet until the reviewer records an initial decision. Record if the
   reviewer already knows the seed.
2. Collect checks through the existing contained runners. Record actual command,
   source identity, environment identity, positive check count where supported,
   exit status and retained receipts. Stop a case if containment is unavailable;
   label it incomplete. Do not weaken containment to get a result.
3. Generate a human packet with `kranz --repo <fixture> review-packet <mission>
   --json`, an outcomes report, and `kranz --repo <fixture> evidence-bundle
   <mission> --out <empty-directory>`. Record the hash and size of each retained file.
   Keep the original export directory and a separate inventory for any review copy that
   deliberately omits evidence. Never reseal or present a modified record as
   the original engine export.
4. Put a unique transcript marker in the worker transcript. Inspect the actual
   retained gate input manifest and all its input bytes for its absence. Keep
   the human packet, transcript, answer key and other reviewer notes out of
   fresh-validator inputs. A human audit export can contain permitted history;
   it is not itself the validator input bundle.
5. The reviewer records a decision before seeing the answer key: accept,
   request repair, or insufficient evidence. Name every extra artifact fetched,
   question asked, missing receipt and manual source/check comparison. Record
   the specific seed caught or missed after unsealing the key. If evidence is
   absent, uncertainty is an acceptable decision; do not substitute a guess.
6. Retain any repair as the next round and regenerate evidence against its new
   candidate. Record code delivery, local merge, release and cutover separately.
   These fixtures do not create a product release or production cutover.
7. Close the observed interval at the end of the case's final reviewed round.
   This is the initial escape-observation period; later discoveries are dated
   addenda. It does not measure production escapes. Publish incomplete cases
   alongside completed cases, with reasons.

## Measurement definitions

Use [the record template](../reviews/evidence/review-effort-pilot/record-template.json).
One record represents one case round. Give every measured value a source
reference; `null` means unavailable. Use `0` only when a named observer actually
measured no occurrences. The template is a manual record, not an ingestion or
telemetry contract.

| Measure | Definition and source | Denominator |
| --- | --- | --- |
| Active human review | Sum of reviewer-recorded intervals spent reading, checking evidence, or deciding. Pause the timer for breaks and unrelated work. Retain interval start/end and the reviewer's declaration. | Rounds with actual timing; report that sample size. |
| Approval/queue waiting | Separate receipt-derived intervals waiting for approval or execution. Report any overlap; never subtract elapsed wait from wall time to infer attention. | Rounds with both endpoints. |
| Manual evidence collection | Count and list artifacts or facts the reviewer had to obtain beyond the supplied packet, with reason and source. | Observed rounds. |
| Questions and interventions | Separately list clarification questions, requested repairs and human control actions, linked to their decision/control records. | Observed rounds; do not infer unobserved activity. |
| Repair rounds | Number of new candidate/evidence rounds after the first review, with before/after identities. | Cases with observed review history. |
| Seed detection | Each predeclared seed marked caught, missed or not evaluated. Name its truth reference and decision evidence. | Evaluated seeds; report omitted seeds separately. |
| Observed escapes | Defects found after an acceptance decision during the declared interval, linked to discovery evidence. | Accepted cases and duration actually observed. |

The report must name total selected, executed, reviewed, timed and incomplete
cases separately. A test's runtime is not review time. Passing tests, mission
completion and fewer review rounds are not proof of lower risk or higher
productivity.

## Fixture readiness before the exercise

The preparation check can rerun these existing exact tests. It verifies available
mechanisms, not case execution, human judgment or an efficiency result:

- `review_packet_invalidates_pass_after_dirty_edit_and_exposes_previous_review_binding`
- `review_packet_missing_tampered_zero_assertions_error_and_expiry_never_pass`
- `review_packet_old_logs_are_explicit_and_never_read_worker_transcripts`
- `contract_controls::tests::pair_tests::baseline_pair_actual_revisions_overlay_and_tamper_evidence`
- `contract_controls::tests::pair_tests::baseline_pair_parity_and_broken_checker_do_not_change_policy`
- `contract_controls::tests::contract_readback_control_setup_zero_checks_and_timeout_are_inconclusive`

Use `cargo test --workspace <exact-name> -- --exact --nocapture`. Check each raw
exit code, positive executed-test count, and absence of capability/containment
skip markers. Resolve exact module names against the pinned source before
execution. Record command, tested commit/tree and output hash in the preparation
receipt. Unit fixture temporaries are not retained mission exports; the actual
cases still require the packets and receipts above.

## Completion and scope limits

The final reviewed artifact links case records and hash inventories and assesses
correctness, readability, architecture, security and performance. Separate
recurring evidence gaps from mechanisms already available. Recommend specific
follow-ups for review; create no automatic tickets or policy changes.

The initial budget is local deterministic fixtures only: no provider invocation,
credentials or model spend. A live-provider extension needs a separately reviewed
plan naming exact adapters/versions, prompts, repository/environment identities,
containment, invocation/retry limits, wall deadline, credential source, enforceable
cost ceiling (or an explicit statement that none exists), and cleanup evidence.
No live extension is prepared or authorized by this document. This pilot adds no
release requirement to the completed ACP stream.
