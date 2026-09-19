# ACP MCP descendant proof and CI scheduling

The containment proof now includes a detached stdio MCP server launched by the
synthetic ACP peer. It negotiates the fixed 2025-03-26 protocol, receives an
initialized notification, lists its tool and executes a tools/call request.
The tool attempts the same thirteen authority, filesystem, supervisor and network
violations as the direct hostile peer, including its own shell and detached
Python child. The test rejects any ACP permission or tool-use event: these
effects deliberately bypass the cooperative ACP callback surface.

The tool must also write actual state into its private HOME and a feature file
inside the worktree. Its response must contain all thirteen successful denials,
the expected private HOME/TMPDIR relationship and absence of Docker control and
SSH-agent variables. The host verifies those files, creates one real feature
commit, and checks the primary bytes, protected base ref, Git configuration and
mission authority file. It independently confirms namespace removal. The new
proof has a 90-second outer deadline and is required by exact name in Linux CI;
the skip marker cannot satisfy that job.

This is a synthetic process-boundary proof. It does not certify a vendor's MCP
configuration, tool permissions or complete protocol implementation. The ordinary
enforced-ACP admission refusal remains in place. No vendor adapter, credential,
Keychain entry or paid model is used by this follow-up.

## Retained CI failure

[Linux job 105942887576](https://github.com/craigcode/kranz/actions/runs/35460318903/job/105942887576)
failed on `8e03511fedc3edf33e859922c88b7c4b060ecf5d` before reaching the ACP or
mount-helper proofs. Four of eight concurrent gate lifecycle fixtures exceeded
the five-second Docker-create control deadline. The identical engine code had
passed [the earlier job](https://github.com/craigcode/kranz/actions/runs/35459712133/job/105941259008).
The logs establish startup timeouts, not their exact host/daemon cause; daemon
contention is a plausible explanation, not a proven root cause.

That lifecycle CI step now schedules unrelated tests sequentially. This bounds
its incidental load without retrying evaluations or extending production
deadlines. Purpose-built concurrency, abort, timeout and ownership cases retain
their internal concurrency. All three synthetic proof logs are uploaded even
after failure, so future incidents retain evidence outside console output.
Neither this scheduling change nor a later pass converts the failed run into
a pass or qualifies evaluator throughput.

## Native handshake regression

The first full-workspace attempt in this follow-up again hit the native
`acp_compat_v1_handshake_rejects_version_and_missing_session_identity` test's
three-second outer timeout. All container unit proofs passed in that attempt.
The failure is retained separately in the receipt. The exact source of the
elapsed time was not captured; it is not attributed to the container wrapper.

That test previously accepted any startup error. Its outer timer also covered
synchronous private toolchain-home preparation and two bounded cleanup waits,
not just protocol parsing. It now requires the specific version-999 or missing
session-ID rejection, refuses unconfirmed cleanup, records the fixture PID and
requires `kill(pid, 0)` to report ESRCH after startup returns. A generic handshake
timeout or early peer exit cannot pass those assertions. The test-only outer
budget is 35 seconds, matching the adjacent unresponsive-peer test; the runtime's
30-second handshake deadline and cleanup/write deadlines are unchanged. This
corrects the acceptance criterion without claiming a startup performance proof.

## Five-axis self-review

Correctness: require a real handshake and tool response, thirteen explicit
denials, actual permitted writes, a nonempty commit and confirmed removal.
Native handshake rejection must have the expected reason and leave no live or
zombie peer.
The tool-written deliverable is not overwritten by the ACP parent. Readability:
the existing hostile probes and worktree assertions are shared with the direct
case; the small MCP exchange stays inside the test fixture. Architecture: no
production client capability, sandbox primitive, dependency or contract changes.
Security: the fixture uses only generated local repositories and canary authority
files, receives no vendor credentials, and demonstrates that mediation callbacks
are not the containment boundary. Performance: one extra real Docker proof;
sequential lifecycle CI takes longer but preserves each production deadline.
This is a self-review, not an independent review or a release approval.

Validation and source fingerprints are recorded in the
[proof receipt](../compatibility/acp/mcp-descendant-proof.json). Final workspace
validation passed 3,041 tests with zero failures and ten existing ignores, with
all three Docker proof opt-ins enabled; Clippy with warnings denied, formatting
and build also passed. The serialized evaluator suite passed 17 tests; the
vendor-image synthetic containment filter passed 15, including all nine required
daemon proofs. The final daemon inventory was empty and Colima was restored to
stopped. Broader S6 vendor
qualification, production admission and S7 mission acceptance remain open.
