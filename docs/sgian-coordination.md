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
cancelled, the engine revokes the credential:

```sh
sgian ctl --workspace <repo root> --json identity revoke <credential id>
```

Revocation also ends any event subscription the worker still held, so the
token is dead the moment the run is over. Only the credential id and holder
appear in the engine's logs; the token crosses into the session environment
and nowhere else.

## When it is off

The lane is best-effort and never a reason to fail a spawn:

- `sgian` is not on `PATH`, or no daemon serves the repository root: the
  session starts without the variable and the engine logs at `debug`.
- The daemon refuses the request (for example the engine itself runs under a
  credential without `admin`), answers with something other than a credential
  record, or does not answer within five seconds: same outcome, logged at
  `warn`.
- `KRANZ_SGIAN_BIN` set to a path uses that binary instead of searching
  `PATH`; set to an empty string it disables the lane entirely.

The engine calls `sgian ctl` with its own environment minus any
`SGIAN_CLIENT_TOKEN`, so the request is made as the workspace owner, the only
identity that can issue credentials under Sgian's default `open` policy.

## Where it lives

`crates/engine/src/sgian.rs` holds the helper; `build_worker_spec` in
`crates/engine/src/runner.rs` issues the credential next to the other
per-session environment, and the two worker entry points revoke it after
`run_session_to` returns. Validator and orchestrator sessions do not get a
credential: they never drive panes.
