# Contained native shell qualification: second batch

Both separately authorized attempts passed using source `817180f` and its
reviewed immutable probe binary. The [projected receipts](../compatibility/acp/native-tool-proof-v2.json)
retain the permission requests, decisions, delivery, tool completion, host
checkpoints and final results, plus private source hashes and daemon inventories.
The earlier [failed batch](../compatibility/acp/native-tool-attempts-v1.json)
remains failed and was not overwritten or retried.

Claude ACP 0.77.0 / Agent SDK 0.3.270 passed in 21.978 seconds. Its fixed
nonessential-traffic setting was present at startup; there was no denied
telemetry connection. Codex ACP 1.11.0 / Codex 0.153.4 passed in 14.811 seconds
with the exact `/usr/bin/bash -lc` permission encoding accepted by the corrected
fixture. Neither changed the provider egress allowlist or accessed Keychain.

Each provider used one ACP prompt and one `allow_once` grant for
`echo kranz-acp-tool-fixture-v1 > fixture-result.txt`. The decision was synced
before response queuing; a separate `sent` receipt preceded successful tool
completion. The host then verified the exact report/file, unchanged primary
state and clean shutdown, and created exactly one feature commit for that file.
Those are host checkpoints, not proof of native Git commands or human merges.
Claude reported USD 0.0615526; Codex reported no cost. Underlying API request
counts and billing are not independently verified. Both no-retry prompt slots
are consumed; these receipts authorize no further provider calls.

Independent inventories before the batch and after each attempt found no
containers, relay networks or volumes. Colima was restored to its original
stopped state. The credential source files were not included in the projection.
The projection omits chat chunks, account/quota metadata and unrelated session
updates, retains original event indices, and records original receipt hashes.
Permission digests refer to original proposals before root-path scrubbing.

## Five-axis author review

- Correctness: both final results and deliverable events agree with the recorded
  grant/delivery sequence and one feature commit. The success path requires exact
  file/report checks and no denied egress. Failed receipts stay separate.
- Readability: current compatibility/containment docs and ticket state now point
  to the successful fixture and its limits; prior reviews are marked historical.
- Architecture: this update contains documentation and evidence only. No worker
  admission, backend, persisted schema, gate policy or release version changed.
- Security: private local credential channels and the same pinned image/egress
  policy were used. No Keychain, added destination, broadened command, durable
  permission or retry was required. Public projections are checked for secrets
  and private host paths; they are explicitly not complete transcripts.
- Performance: both runs stayed within their 120-second prompt, 180-second
  session and 240-second outer budgets. No production overhead was introduced.

This is an author self-review, not an independent validator verdict. The test
host was macOS ARM64 with a Linux ARM64 guest under Colima. It does not establish
native Linux engine-host compatibility, every vendor tool's behavior, or that
all commands must request permission. Ordinary enforced-ACP admission stays
closed. S6 still needs Linux vendor receipts and qualified mission dispatch;
S7 then proves independent review/repair, actual merge-tree judgment and export.

## Validation

The exact probe source and binary were already gated at `817180f`: all four
workspace gates passed, with 3,041 tests, zero failures and ten existing ignores;
29 synthetic cases passed on each pinned image, plus startup-policy and example
checks. The [Linux synthetic CI job](https://github.com/craigcode/kranz/actions/runs/35475795815/job/105984718138)
also passed at that exact head, including all 29 tool cases and the Claude
startup-policy cases. Its log hash is retained with the proof. This follow-up
changes only docs/evidence, so those Rust gates were not repeated. The new receipts were checked for successful terminal results, one
request/grant/delivery/checkpoint each, ordering, unchanged primary state, zero
denied egress, confirmed cleanup, approved hashes and absence of private paths.
Secret scans, domain lint, formatting and post-commit knowledge freshness cover
the published follow-up. CI on the new documentation commit remains a separate
result; no native Linux vendor proof is inferred from synthetic CI.
