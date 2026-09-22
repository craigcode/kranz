# Sgian coordination lane

[Sgian](https://github.com/craigcode/sgian) is a terminal multiplexer whose
daemon supervises coding agents in a repository and records who did what: each
client connects under a holder name, takes leases on agent panes, and leaves
attributed records in the daemon's ledger. When a Kranz mission runs inside a
repository that a Sgian daemon also serves, every worker session identifies
itself to that daemon as its own principal instead of borrowing the
operator's.

## What happens

Before a worker session starts, the engine asks the daemon for a credential:

```sh
sgian ctl --workspace <repo root> --json identity issue --holder kranz:<run-id> --scope write
```

The token from the reply enters the session environment as
`SGIAN_CLIENT_TOKEN`, which is the variable Sgian's own clients and `sgian ctl`
read. Anything the worker does through Sgian, whether it drives a pane, takes
a lease, or reports status, is attributed to `kranz:<run-id>` with the `write`
scope: no `admin` scope, no impersonation of other holders, and no
broadcasts. When the session ends, whether it passed, failed, or was
cancelled, the engine attempts to revoke the credential:

```sh
sgian ctl --workspace <repo root> --json identity revoke <credential id>
```

An owned guard attempts revocation when a running worker future is dropped as
well as on normal return. Each control call has a five-second execution/output
deadline and bounded reply buffers; it supervises descendants that hold pipes
open after the leader exits. The helper receives the existing discovery
environment allowlist (including HOME for daemon lookup), not ambient provider
secrets. Reply bytes are not included in error logs. The shared scrubber also
recognizes the fixed Sgian client-token shape in worker output and evidence;
this remains defense in depth, not a guarantee against arbitrary disclosure.

Revocation is best-effort. A daemon refusal, failed call or engine crash can
leave a credential outstanding; a killed engine cannot run its cleanup guard.
Inspect the daemon and revoke outstanding run identities explicitly after
uncertain cleanup. A successful revocation ends the credential's subscriptions.
The token is passed in the session environment, not as a command argument or
intentional log field. This lane does not grant access through a sandbox or
mount a host daemon socket into a contained worker.

## When it is off

The lane is best-effort and never a reason to fail a spawn:

- `sgian` is not on `PATH`, or no daemon serves the repository root: the
  session starts without the variable and the engine logs at `debug`.
- A refused request, failed control call or deadline produces the same outcome,
  logged at `debug`. An unusable credential record is logged at `warn`.
- `KRANZ_SGIAN_BIN` set to a path uses that binary instead of searching
  `PATH`; set to an empty string it disables the lane entirely.

The engine calls `sgian ctl` with the discovery allowlist and without any
`SGIAN_CLIENT_TOKEN`, so the request is made as the workspace owner, the only
identity that can issue credentials under Sgian's default `open` policy.

## Where it lives

`crates/engine/src/sgian.rs` holds the helper; `build_worker_spec` in
`crates/engine/src/runner.rs` issues the credential next to the other
per-session environment, and the two worker entry points own the revocation
guard across `run_session_to`. Validator and orchestrator sessions do not get a
credential: they never drive panes.
