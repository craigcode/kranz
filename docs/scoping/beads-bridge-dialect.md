# Beads bridge dialect — verified against a live install

Probed 2026-07-29 against the actual `bd` and `gc` binaries on PATH
(`/opt/homebrew/bin/bd`, `/opt/homebrew/bin/gc`), per docs/scoping/beads-workstore.md:176
("Brief 1 must verify against a live gc install before relying on any command shape").
All probes below are read-only (`--version` / `--help` only). No bead was created,
updated, claimed, commented on, or closed. No existing bead store (in-repo or otherwise)
was queried. `gc init` and `gc stop` were never invoked.

## 1. Probed versions

```
$ bd --version
bd version 1.0.5 (Homebrew)

$ gc --version
gc: unknown flag: --version
(gc has no --version flag; use the `version` subcommand instead)

$ gc version
1.3.2
```

## 2. Verified command shapes

Flags below are copied verbatim from live `--help` output. Global flags common to every
`bd` subcommand (`--json`, `--db`, `-C/--directory`, `--actor`, `--readonly`, etc.) are
omitted from the per-row list for brevity; every subcommand shown accepts `--json`.

| Subcommand | Verdict | Verified flags relevant to the bridge |
|---|---|---|
| `bd ready` | VERIFIED | `-l, --label strings` (AND filter), `--label-any strings` (OR filter), `--json`, `--claim` ("Atomically claim the first ready issue matching the filters"), `-a/--assignee`, `-u/--unassigned`, `-n/--limit` (default 100), `--mol`, `--gated`, `--explain` |
| `bd show` | VERIFIED | `[id...]` positional or `--id stringArray`, `--json`, `--current`, `--short`, `--long`, `--include-comments`, `--include-dependents`. The `--help` text does **not** state whether `--json` with a single positional id returns a bare object or a single-element array — this could not be resolved from `--help` alone, and probing a live store was out of scope for this read-only-only feature. Left **unresolved** — flagged for a downstream feature to confirm against a real fixture. |
| `bd update` | VERIFIED | `-s, --status string` ("New status") — **accepted**, matches spike usage; `--claim` ("Atomically claim the issue (sets assignee to you, status to in_progress; idempotent if already claimed by you)") — **accepted**. No `--lease`, `--lease-ttl`, or `--heartbeat` flag exists anywhere in the flag list. No `lease_expires_at` / `heartbeat_at` field appears in the help text. |
| `bd comment` | VERIFIED | Usage: `bd comment <id> [text...] [flags]` — text is positional (confirms docs/gascity.md:45). Also `--file`, `--stdin`. No `-m` flag. |
| `bd close` | VERIFIED | `-r, --reason string` ("Reason for closing") — confirms docs/gascity.md:45. Also `--reason-file`, `--claim-next`, `--force`, `--continue`. No `-m` flag. |
| `bd set-state` | VERIFIED — verb exists | Usage: `bd set-state <issue-id> <dimension>=<value> [flags]`, plus `--reason string`. Description: "Atomically set operational state on an issue" — creates an event bead, updates a `<dimension>:<value>` label. Takes a `dimension=value` pair (e.g. `patrol=muted`, `health=healthy`), **not** a bare status string. |

## 3. Claim / lease support: **NO** lease/TTL/heartbeat mechanism exists

- `bd ready --claim` and `bd update --claim` both exist and are documented (verbatim,
  from live `--help`) as: "Atomically claim the issue (sets assignee to you, status to
  in_progress; idempotent if already claimed by you)" / "Atomically claim the first ready
  issue matching the filters". This is a one-shot atomic claim (assignee + status) — not
  a leased claim.
- There is **no** lease, TTL, or heartbeat flag or field anywhere in `bd update --help`,
  `bd ready --help`, `bd show --help`, or the top-level `bd --help` subcommand list. No
  `--lease`, `--lease-ttl`, `--heartbeat`, `lease_expires_at`, or `heartbeat_at` string
  appears in any of the probed `--help` output.
- This scan covered every subcommand the bridge touches (`ready`, `show`, `update`,
  `comment`, `close`, `set-state`) plus the top-level command list; it was not an
  exhaustive scan of every one of `bd`'s ~60 subcommands, but none of the top-level verb
  names (see `bd --help` in the appendix) suggest a dedicated lease/heartbeat command
  either.
- **Escalation**: this means the mission's "lease-aware claim with liveness-first
  recovery" milestone **cannot be implemented as specified** against this bd dialect.
  There is no live mechanism (TTL, heartbeat, expiry timestamp) to detect a dead claimant
  and recover the bead. Per the feature spec's explicit instruction, no substitute
  (emulating leases via comments, metadata, or sentinel files) has been invented here.
  This finding is also recorded in the WorkerReport's `knownGaps`.

## 4. Shapes the spike currently uses that the live binary does NOT accept

Read from `packaging/gascity/bin/kranz-run-bead` and `packaging/gascity/bin/kranz-dispatch`
(not modified in this feature — read-only comparison only):

- `packaging/gascity/bin/kranz-run-bead:25`: `bd set-state "$ID" blocked` — **wrong
  shape**. Live `bd set-state` requires `<issue-id> <dimension>=<value>` (e.g.
  `bd set-state "$ID" mode=blocked`), not a bare status word. As written this call would
  fail against the live binary (it falls through to `bd update "$ID" --status blocked`
  via `||`, which is the shape that actually works).
- `packaging/gascity/bin/kranz-dispatch:43,58,75` calls `gc bd ready --label ... --json`,
  `gc bd show "$ID" --json`, `gc bd update "$ID" --status in_progress` — these could
  **not be verified as a faithful pass-through** in this probe. `gc bd --help` was run
  from outside a Gas City rig (no `city.toml`/`.gc/` in this checkout) and failed with:

  ```
  $ gc bd --help
  gc bd: not in a city directory (no city.toml or .gc/ found)
  ```

  `gc bd` requires an initialized/registered city context to even show its own help, so
  its flag-passthrough fidelity to the shapes verified in §2 could not be confirmed here
  without registering a city — which this feature was explicitly barred from doing
  (`gc init` is forbidden). This is a gap for a downstream feature to close inside an
  already-registered rig, not something fabricated here.
- Neither script uses `bd update --claim` or `bd ready --claim`; both mutate status
  directly (`--status in_progress` / `--status blocked` / `--status open`), which is a
  valid live shape but is not atomic claim semantics.

## 5. `bd init` in an arbitrary empty directory (probed via `--help` only — not run)

`bd init --help` describes `bd init` as: "Initialize bd in the current directory by
creating a `.beads/` directory and Dolt database." It defaults to an **embedded Dolt
engine** ("no external server needed") with the issue prefix defaulting to the current
directory name, and `--non-interactive` is auto-detected in CI / non-TTY environments.
Based on this description, `bd init` (no flags) appears able to create a self-contained,
standalone store in an arbitrary empty directory without requiring a pre-existing
external Dolt server or a Gas City rig registration — but this was **not actually run**
in this feature (per the read-only-probe constraint), so it is a reading of `--help`
text, not an executed verification. A downstream feature that needs this guarantee
should run `bd init --non-interactive` in a real scratch directory to confirm.

## Appendix: other probes

- `bd --help` top-level subcommand list confirms `ready`, `show`, `update`, `comment`,
  `close`, `set-state`, `init` all exist as top-level verbs on this version. `statuses`
  also exists (`bd statuses` — "List valid issue statuses"), useful for a downstream
  feature that wants to validate `--status` values against the live dialect rather than
  hardcoding them.
- `gc --version` is not a valid flag; version is available via `gc version` (subcommand),
  confirmed above.
- No real bead was created, read from, updated, claimed, commented on, or closed against
  any live database, in-repo or otherwise. The `bd show --json` object-vs-array question
  (§2) remains unresolved because resolving it would require running a read command
  against an actual store, which was out of scope for this probe-only feature.
