# ACP and gate review remediation

The supplied review covers public `a5442c557a2cefde58263525b6497a4cdd78e177`
(PRs #70–90). This response verifies the findings against that tree and records
fixes and remaining limits. Synthetic tests use no provider login, Keychain or
model calls. The implementation and five-axis review below are assistant
self-review, not an independent security certification.

## Main findings

| Finding | Disposition and regression evidence |
|---|---|
| Implicit Sgian write authority and PATH execution | Disabled by default. An explicit absolute installed helper must resolve outside both checkouts; enforced workers never receive a token. Issuance and revocation leave the async executor. `resolve_bin_requires_explicit_absolute_opt_in` and `run_worker_issues_and_revokes_a_sgian_credential` cover trusted opt-in, refusal and normal/cancelled cleanup. Sgian's committed `c78378fb` source confirms write-scoped `RunProcess` executes on its host; actual reachability through every Kranz sandbox was not proved. |
| Ambiguous approval text | CLI, Slack and dashboard escape display controls visibly. Engine answer admission and backend response capabilities refuse allow on ambiguous original values, including JSON keys, options and binding text. Policy denial retains the original action/digests. Historical requests still validate and replay. `live_permission_ambiguous_values_cannot_be_allowed_but_old_requests_still_validate`, `permission_card_escapes_hidden_text_and_never_offers_allow` and dashboard adversarial cases cover this boundary. |
| Review-packet terminal controls | Controls are escaped before Markdown rendering. `review_packet_never_emits_terminal_controls_or_invisible_unicode` covers ESC reset, CR, C1 and bidi controls. Standard OSC-52 was already changed by Markdown escaping of `]`; arbitrary terminal control passthrough was nevertheless real. |
| ACP deny matching | Wildcard suffix matching handles repeated literals; command arguments and all reported file locations participate. Paths are compared as original, normalized, absolute and workspace-relative labels. Malformed subject shapes fail closed. `backend_acp_policy_covers_repeated_suffix_arguments_and_every_normalized_path` covers the reported bypasses and a legitimate command. This remains cooperative adapter policy, not a shell parser or containment boundary. |
| Excluded changed source | External acceptance refuses changed private/excluded source rather than omitting it or disclosing credentials. Selection also uses the existing secret scanner. Current mission record files remain a narrowly enumerated metadata class, matching the existing deliverable policy; arbitrary files under mission directories are not exempt. `gate_snapshot_acceptance_refuses_hidden_flags_dirty_bytes_and_excluded_changes` covers both path and PEM-marker omissions. |
| Ordinary path compatibility | v1 labels now accept Unicode and ordinary framework punctuation, retaining traversal, control, reserved-device and alias checks. The wire schema is relaxed compatibly; event/state layouts are unchanged. `gate_snapshot_acceptance_supports_web_and_unicode_paths` and schema fixtures cover this contract adjustment. |
| Buffered audit loss | The existing single-writer relay persists bounded progress batches with acknowledgement/backpressure, even without consent requests. Spawn and final/error paths flush. `live_permission_relay_persists_long_permissionless_runs_including_transport_failure` preserves all 4,200 messages in both successful-stream and transport-failure cases, plus a terminal run record. Durability failure still stops the run; no unlimited buffer or fabricated success is introduced. |
| Stale mount-proof cache | Process-wide memoization is removed. Each admission proves its current runtime, image and mount; repaired failures can retry without restarting serve. Identical roots are deduplicated only within one call. This deliberately trades additional probe cost for fresh evidence. Existing real mount proof fixtures still exercise ownership, timeout and cleanup. |
| Working bytes versus accepted commits | Capture refuses hidden index flags; external milestone/final/merge acceptance additionally requires clean tracked bytes and no untracked source before and after evaluation. Diagnostic snapshots may still capture ordinary dirty/untracked files. Native validators already refused hidden flags, so the supplied report's broad end-to-end acceptance exploit was not established. The new regression proves the external stage now refuses that mismatch itself. |
| Completion with unanswered consent | Completion checks pending permission requests before any optional terminal-provider cleanup. `live_permission_unanswered_cannot_complete_without_a_terminal_provider` drives a real subprocess peer through request → premature prompt result and observes failure. Production terminal capability remains disabled. |

## Additional findings

- **Consume binding:** callers already recheck stage-specific source, authority, base/candidate refs and registrations before consuming the batch. The copied binding is not itself a freshness check. That caller obligation remains explicit in `driver.rs`; snapshot checks are now strengthened as above. No demonstrated missing caller check was supplied or reproduced.
- **Unrelated pack edits:** full registration/manifest pinning deliberately fails closed on policy drift. Narrowing that identity would change the approval contract; this patch retains it.
- **Generic credential files:** source selection now applies the engine's secret scanner as well as private path/PEM exclusions. A changed excluded source blocks acceptance. Unknown or encoded secret formats remain outside any complete DLP claim.
- **Credential chunks:** configured credential matching now retains bounded suffixes for ACP agent message and thought text streams, across intervening notifications. `acp_profile_refuses_a_credential_split_over_text_frames` covers the reported normal chunk boundary. Arbitrary encoding, reordering or fragments spread across unrelated fields remain outside this defense-in-depth check.
- **Resource limits:** no memory/CPU quota is newly claimed. [Resource qualification](../../.kranz/tickets/acp-resource-budget-qualification.md) owns explicit profile budgets and production terminal lifetime/request semantics; it blocks Sgian qualification.
- **Container identity:** attachment/start now uses the full ID returned by successful creation, rather than the reusable name. Cleanup already verifies owner labels and removes inspected IDs.
- **Create/start crash gap:** a never-started container cannot run its guest watchdog. The retained ownership ledger supports explicit reconciliation, not an automatic reaper. [Recovery and credential lifecycle](../../.kranz/tickets/acp-recovery-and-credential-lifecycle.md) owns a qualified recovery operation; documentation now names this gap for workers as well as mount helpers.
- **Codex refresh rotation:** already documented; worker-private refreshes are not written back to the selected login. The source can become superseded. The same lifecycle ticket owns a trusted renewal design; copying a worker-authored auth file into the operator home is not an acceptable quick fix.
- **Sgian token format:** verified against committed Sgian source: 32 random bytes encoded as `sgc_` plus 64 hex characters. The existing `scrub_sgian_client_credentials_in_bare_and_structured_output` regression already covers this format. The review's claim of an unverified assumption is resolved by source evidence.
- **Blocking Sgian calls:** fixed with blocking-pool issuance/revocation and bounded asynchronous cancellation cleanup, while retaining the existing subprocess deadline and pipe supervision.
- **Binary artifact export:** retained byte hashes are now checked before lossy text/redaction transformations. The bundle hashes its actual exported, scrubbed bytes; mismatched originals remain unresolved. The lifecycle export regression includes non-UTF-8 bytes and later substitution.
- **Read-token scope:** on default loopback binds, legacy `/events` and `/state` remain readable. `--read-auth` and non-loopback binds protect those endpoints. Human-review route authentication is a route boundary, not a claim that all underlying mission data is private in default loopback mode. No read-auth check was removed.
- **Terminal fixture limits:** the 30-second guest lifetime and 1,024-request bound are real but apply to a deliberately qualified synthetic fixture. Released profiles do not advertise terminals. The resource ticket requires production semantics before rollout.
- **Python assertions:** probe checkers explicitly reject optimized Python before doing work; CI tests this under `-O`.
- **Adapter lock drift:** CI now verifies installation/image/native proof lock hashes and provider versions/integrity. Negative tests reject changed lock bytes or version claims. Historical proof receipts are not rewritten.

## Five-axis self-review

Correctness distinguishes current evidence from retained diagnostics and refuses
ambiguous approvals. Readability centralizes visible-control handling and names
operator recovery limits. Architecture reuses the permission relay, source
snapshot, scrubber, Git guards and bounded helper supervisor. Security keeps
credentials out of snapshots and removes implicit host authority without adding
new terminal capabilities. Performance uses bounded progress batches and suffix
buffers; fresh mount probes add known per-admission work. There is no new provider
qualification or benchmark claim.

## Validation

Verification uses the full workspace suite with explicit synthetic ACP, gate and
mount-container proofs, Clippy, formatting/build, strict API docs, the standalone
Tauri check and all dashboard gates. The dashboard has 264 passing tests; the
schema suite has 17 and the offline ACP receipt suite has four. Isolated ACP
consumer/conformance checks, dependency policy, tree/history secret audits,
package contents, notices and the five-crate publication dry run are also part
of the candidate evidence. No live provider, Keychain or model spend is used.
Exact-source final workspace counts and cross-platform CI are recorded on the
release PR before merge; archive provenance/smoke and registry receipts are
separate release gates.

Early local attempts found an obsolete Sgian fixture installing its helper
inside the repository, a placeholder secret intentionally ignored by the
scanner, and an assertion tied to an old refusal message. Those fixtures were
corrected without relaxing production checks. Colima refused macOS's unshared
default temporary root; explicit proofs use dedicated shared home directories.
One unchanged native-Claude subprocess fixture timed out at 30 seconds, then
passed alone and with its complete 40-test group (one ignored live smoke).
The cause of that single timeout is unconfirmed; a full rerun uses eight test
threads, and CI retains its normal platform configuration.
