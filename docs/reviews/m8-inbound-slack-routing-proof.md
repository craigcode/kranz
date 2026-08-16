# M8 inbound Slack routing proof

Date: 2026-08-15 America/Los_Angeles (2026-08-16 UTC)

## Result

M8's final live-QA gap passed. A human entered the read-only
`/kranz status` command in the configured Slack channel. The event crossed one
authenticated Socket Mode connection, routed through the exact operator-owned
workspace/channel mapping to `kranz-proof`, and produced a Kranz status response
for that repository while a second healthy repository was present.

No GUI automation or new macOS permission was used. The operator entered the
command directly in Slack.

## Evidence

- Host: one loopback-only `kranz serve --slack` process.
- Socket Mode: authenticated once; bridge reported `connect_count=1` and
  remained healthy.
- Catalog: `kranz-proof` and `kranz-secondary` both reported `healthy` before
  the command; both had zero queued and zero running work.
- Route: exact configured workspace/channel mapping (`T03…645` / `C0B…LJ0`)
  selected `kranz-proof`.
- Human command: `/kranz status`, observed at 23:35 local time.
- Response: `Running — No mission is running`, `Queue — 0 waiting`, and the
  repository-specific stage summary `captured 6 · ... · landed 211 · failed 0
  · abandoned 1`, with `UNMERGED none`.
- Routing discriminator: immediately after the response, the canonical proof
  checkout listed six `NEW` tickets while the secondary checkout listed two;
  the returned `captured 6` therefore identifies the mapped proof repository
  rather than a sole-repository fallback or the secondary root.
- The bridge health log observed a frame three seconds before its 06:35:28 UTC
  heartbeat, consistent with the human command/response timestamp.

The command was read-only. It did not approve, queue, start, merge, or spend.
The pre-existing routing integration suite remains the ambiguity-refusal proof;
the live action was deliberately limited to the positive exact-route case so
the operator's Slack workspace received no artificial error traffic.

## Cleanup

The proof host was stopped cleanly. Its temporary HOME, copied credentials,
catalog rows, serve tokens, and route were deleted. The operator's real
`~/.kranz/config.json` was never modified. No Slack credential, message content
outside the proof response, or private workspace metadata is recorded here.
