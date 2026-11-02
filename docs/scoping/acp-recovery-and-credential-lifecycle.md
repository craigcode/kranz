# ACP recovery and credential lifecycle: scope and contract

Status: scoped and implemented 2026-11-01.

Ticket: [acp-recovery-and-credential-lifecycle](../../.kranz/tickets/acp-recovery-and-credential-lifecycle.md).
Prior contracts: [ACP containment](../acp-containment.md),
[ACP resource budgets](acp-resource-budgets.md).

## Problem statement

Two operational lifecycle edges remained open after the initial ACP containment
and resource-budget slices:

1. **Dead-owner Docker ledger recovery:** `OwnedContainer::prepare` creates a
   private `kranz-acp-owned-*` directory in the engine scratch root containing
   `container.json` (`version`, `name`, `owner`, `image`, `docker`, `ownerPid`,
   `supervisorSha256`, `ownerIdentity`, `cleanupConfirmed`, `creationFinished`).
   If the engine dies via `SIGKILL` or a Docker removal call fails during
   teardown, the guest PID 1 lease expires within five seconds, stopping any
   running container, and the `kranz-acp-owned-*` ledger remains on disk so
   unconfirmed state is never hidden. A deterministic recovery pass is needed
   to reap dead-owner containers and remove their ledgers only after daemon
   absence is confirmed.
2. **File-only credential renewal and refresh-token rotation:** Contained ACP
   profiles (`claude-acp-0.77.0-arm64-v1` and `codex-acp-1.11.0-arm64-v1`) use
   explicit, operator-selected credential files (`credentialFile`) seeded into
   a disposable session `HOME`. Long-lived OAuth credentials eventually expire
   or rotate refresh tokens inside the disposable `home/.codex/auth.json`. The
   lifecycle contract must define how credentials are renewed without ever
   copying worker-writable files back to the operator's home directory or
   re-enabling Keychain, browser, or ambient environment discovery.

## D-L: operator decisions

| Decision | Rule | Consequence |
|---|---|---|
| D-L1: Dead-owner verification | A ledger is eligible for recovery only when `container.json` records a non-empty `ownerIdentity` and `process_identity_token(ownerPid)` no longer matches it | Live sibling sessions are never interrupted; unverifiable ledgers are retained |
| D-L2: Daemon and container matching | Recovery inspects only the exact canonical `docker` binary recorded in `container.json`, filters by `label=com.kranz.acp-owner=<owner>` with `--no-trunc`, and removes only 64-hex container IDs | Container name reuse or an alternate Docker context cannot cause an unowned container to be removed |
| D-L3: Interrupted creation grace | `OwnedContainer::create` marks `creationFinished: true` in `container.json` once `docker create` returns a 64-hex ID; if `creationFinished` is `false` and the ledger is younger than `CREATE_TIMEOUT` (30 s) with an empty daemon inventory, recovery retains the ledger | Prevents deleting a ledger while an interrupted `docker create` is still completing in the daemon |
| D-L4: One-way credential seeding | `home/.codex/auth.json` inside the session scratch directory is worker-writable and untrusted after launch; the engine never copies it back to `credentialFile` | A compromised worker cannot persist poisoned tokens, config, or symlinks into operator state |
| D-L5: Out-of-band operator renewal | Credential renewal happens on the host outside the container by atomically replacing `credentialFile` (`0600`, single hardlink, regular file) before the next session starts | Each session starts from a verified operator-owned snapshot while keeping browser, Keychain, and ambient env channels closed |

## Dead-owner Docker ledger recovery contract

`acp_container::recover_dead_owner_ledgers(scratch_root, expected_docker)`
scans `kranz-acp-owned-*` directories in `scratch_root`:

1. **Symlink and file validation:** Symlinked ledger directories are refused and
   recorded as `unconfirmed`. `container.json` is opened with
   `O_NOFOLLOW | O_NONBLOCK` and parsed via `strict_json::parse`.
2. **Liveness check:** If `ownerPid` and `ownerIdentity` match the running
   process identity returned by `event_log::process_identity_token`, the ledger
   belongs to a live owner and is counted in `skipped_live` without running any
   Docker command.
3. **Daemon pin check:** The recorded `docker` path in `container.json` must
   match `expected_docker`. Mismatched daemon paths are retained in
   `unconfirmed`.
4. **Creation-in-flight guard:** When `creationFinished` is `false` and the
   ledger age is within `CREATE_TIMEOUT`, an empty `owned_ids` listing is
   treated as uncertain (`unconfirmed`) rather than absent.
5. **Confirmed removal:** Recovery calls `remove(&client, &owner, true, finished)`,
   which lists containers by `label=com.kranz.acp-owner=<owner>`, removes any
   64-hex container ID, and polls until `owned_ids` returns an empty list. Only
   after `remove` succeeds does recovery delete the `kranz-acp-owned-*`
   directory and record `owner` in `removed`.

## Credential renewal and refresh-token rotation contract

1. **No write-back from disposable home:** During a Codex ACP session, the
   adapter may rewrite `$CODEX_HOME/auth.json` inside the disposable `home`
   directory when rotating a refresh token. Because the worker shares write
   access to `$CODEX_HOME`, those bytes are untrusted. `PreparedProfile::close`
   destroys the disposable home directory at session exit and never copies
   `$CODEX_HOME/auth.json` back to `credentialFile`.
2. **Out-of-band renewal verification:** `AcpWorkerProfile::verify_credential_source`
   validates `credentialFile` before session preparation (`O_NOFOLLOW | O_NONBLOCK`,
   regular file, `uid == geteuid()`, `nlink == 1`, mode `0600`, size `<= 48_000`
   bytes, outside repository, worktree, and scratch roots, unique-key JSON,
   required OAuth token structure, and no `OPENAI_API_KEY` fallback) and returns
   a non-secret `CredentialSourceReceipt` containing the SHA-256 digest of the
   credential file and the count of validated secret tokens.
3. **Session isolation across renewal:** When the operator atomically replaces
   `credentialFile` on the host between sessions, the next `prepare()` seeds the
   renewed credential into a fresh disposable home and registers the new token
   values with the session scrubber.
