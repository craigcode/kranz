# Beads bridge dialect — verified against a live install

Probed 2026-07-29 against the actual `bd` and `gc` binaries on PATH
(`/opt/homebrew/bin/bd`, `/opt/homebrew/bin/gc`), per docs/scoping/beads-workstore.md:176
("Brief 1 must verify against a live gc install before relying on any command shape").
Probes below are: read-only `--version` / `--help` against the installed binaries; a
read-only `bd show`/`bd list --json` attempt against an out-of-repo existing store that
returned no data (see Appendix); and, added 2026-07-30, mutating probes confined to a
throwaway `mktemp -d` sandbox (see §2, §3, §5). No in-repo store and no other real
pre-existing store was ever created, updated, claimed, commented on, or closed against.
`gc init` and `gc stop` were never invoked.

**Line-number note:** `path:line` citations into `packaging/gascity/bin/` or
`packaging/gascity/test/` below were verified against commit `329010f` (the tip this
feature branched from). Line numbers drift as unrelated header/comment growth lands in
those scripts, so each citation below also names the actual code construct it points at
(a flag, a function signature, a literal command) so it stays findable by search even
after the number moves.

> **Correction note (2026-07-30):** commit `14a49ab` ("record live bd/gc dialect probe",
> the second commit under that identical message) retroactively deleted the Appendix
> disclosure below about the out-of-repo `bd show`/`bd list --json` attempt and replaced
> it with the opposite claim ("No existing bead store (in-repo or otherwise) was
> queried."), with no correction note explaining the change. It also tightened "read-only
> (`--version` / `--help`)" to "read-only (`--version` / `--help` **only**)" and reframed
> the unresolved object-vs-array question in §2 as out of scope rather than attempted-but-
> inconclusive. This commit restores the original disclosure verbatim, removes the three
> contradicting statements `14a49ab` introduced, and adds the executed mutating probe
> (§2, §3, §5) that was still outstanding at the time of that dispute.

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

### Version gap: installed `bd` is a minor version behind the researched dialect

The installed binary is **`bd` 1.0.5**. This mission's research base,
`docs/scoping/beads-workstore.md:10`, records beads as **`v1.1.0` (2026-07-04)** at the
time of that investigation, and `docs/scoping/beads-workstore.md:66` records claim
**leases with TTL + heartbeat (`lease_expires_at`, `heartbeat_at`)** verified there
against `internal/types/types.go` (i.e. against the beads source, not `--help` text).
The installed 1.0.5 binary therefore predates the version that research examined by at
least one minor release. A read-only `brew info beads` / `brew outdated` query (run
2026-07-30, no mutation) confirms an upgrade path exists and was not taken:

```
$ brew info beads
==> beads: 1.0.5 → stable 1.1.2 (bottled), HEAD
Installed Versions
beads 1.0.5 → 1.1.2 (12 files, 133.2MB) [Linked]

$ brew outdated | grep beads
beads
```

`brew outdated` confirms beads is flagged outdated locally, and `brew info` confirms
`1.1.2` is available as a direct upgrade target (`1.0.5` → `1.1.2`, one hop, no
intermediate pin needed). Whether to run that upgrade is left to the operator — this
feature did not run `brew upgrade` per its constraints. See §3 for the executed
data-model probe against the *installed* 1.0.5 binary, which is the version the bridge
actually runs against today regardless of what upgrade path exists.

## 2. Verified command shapes

> **Scope caveat:** the table below is verified against **upstream `bd` 1.0.5 invoked
> directly**. Every bridge script actually calls `bd` through the `gc bd` wrapper:
> `packaging/gascity/bin/kranz-dispatch` calls `gc bd ready`, `gc bd show`, `gc bd
> update` (claim, status transitions, and the `--reclaim` sweep's release calls), and
> `gc bd list`; `packaging/gascity/bin/kranz-run-bead` defines a `bd()` shell function
> wrapping `gc --city "$CITY" bd`.
> The `gc bd` pass-through's fidelity to the shapes below is unverified (see §4 —
> `gc bd --help` could not even be run without an initialized city, which this feature
> was barred from creating). A downstream milestone must not read this table as
> covering the production (`gc bd`-mediated) path. (Line references deliberately
> omitted: they pin to line numbers that move with every edit.)

Flags below are copied verbatim from live `--help` output. Global flags common to every
`bd` subcommand (`--json`, `--db`, `-C/--directory`, `--actor`, `--readonly`, etc.) are
omitted from the per-row list for brevity; every subcommand shown accepts `--json`. Each
row is backed by a fenced transcript of that subcommand's live `--help` output,
immediately below the table.

| Subcommand | Verdict | Verified flags relevant to the bridge |
|---|---|---|
| `bd ready` | VERIFIED | `-l, --label strings` (AND filter), `--label-any strings` (OR filter), `--json`, `--claim` (atomically claim the first ready issue matching filters — see §3), `-a/--assignee`, `-u/--unassigned`, `-n/--limit` (default 100), `--mol`, `--gated`, `--explain` |
| `bd list` | VERIFIED (added 2026-07-31 for the `--reclaim` sweep) | `-s, --status string` — comma-separated for multiple (`--status open,in_progress`) — the sweep's candidate-claim filter; `--json` (returns a JSON array of issue objects, each carrying `id`, `status`, `assignee`, `updated_at` — `updated_at` is UTC ISO-8601 with a trailing `Z`, confirmed by executed probe below); `-a, --assignee string`, `--no-assignee`, `-l, --label strings`, `--label-any strings`, `-n, --limit int` (default 50, `0` = unlimited), `--ready`. |
| `bd show` | VERIFIED | `[id...]` positional or `--id stringArray`, `--json`, `--current`, `--short`, `--long`, `--include-comments`, `--include-dependents`. **Resolved by executed probe (2026-07-30, §3 sandbox):** `bd show <id> --json` with a single positional id returns a **single-element JSON array**, not a bare object — confirmed against a real fixture bead, both with and without `--long`. |
| `bd update` | VERIFIED | `-s, --status string` ("New status") — **accepted**, matches spike usage; `--claim` ("Atomically claim the issue (sets assignee to you, status to in_progress; idempotent if already claimed by you)") — **accepted**. `-a, --assignee string` — **accepted**; empty string clears it, confirmed by an executed probe below. No `--lease`, `--lease-ttl`, or `--heartbeat` flag exists. No `lease_expires_at` / `heartbeat_at` field appears anywhere in `--help` output, and none appeared in the executed `bd show --json` output either (§3). |
| `bd comment` | VERIFIED | Usage: `bd comment <id> [text...] [flags]` — text is positional (confirms docs/gascity.md:45). Also `--file`, `--stdin`. No `-m` flag. |
| `bd close` | VERIFIED | `-r, --reason string` ("Reason for closing") — confirms docs/gascity.md:45. Also `--reason-file`, `--claim-next`, `--force`, `--continue`. No `-m` flag. |
| `bd set-state` | VERIFIED — verb exists | Usage: `bd set-state <issue-id> <dimension>=<value> [flags]`, plus `--reason string`. Sets state as `<dimension>=<value>` (e.g. `patrol=muted`), **not** a bare status string. |

<details><summary><code>bd ready --help</code> (full)</summary>

```
Show ready work (open issues with no active blockers).

Excludes in_progress, blocked, deferred, and hooked issues. This uses the
GetReadyWork API which applies blocker-aware semantics to find truly claimable work.

Note: 'bd list --ready' uses the same blocker-aware ready-work semantics.

Use --mol to filter to a specific molecule's steps:
  bd ready --mol bd-patrol   # Show ready steps within molecule

Use --gated to find molecules ready for gate-resume dispatch:
  bd ready --gated           # Find molecules where a gate closed

Use --claim to atomically claim the first ready issue matching the filters:
  bd ready --claim --json

This is useful for agents executing molecules to see which steps can run next.

Usage:
  bd ready [flags]

Flags:
  -a, --assignee string              Filter by assignee
      --claim                        Atomically claim the first ready issue matching the filters
      --exclude-label strings        Exclude issues that have ANY of these labels
      --exclude-type strings         Exclude issue types from results (comma-separated or repeatable, e.g., --exclude-type=convoy,epic)
      --explain                      Show dependency-aware reasoning for why issues are ready or blocked
      --gated                        Find molecules ready for gate-resume dispatch
      --has-metadata-key string      Filter issues that have this metadata key set
  -h, --help                         help for ready
      --include-deferred             Include issues with future defer_until timestamps
      --include-ephemeral            Include ephemeral issues (wisps) in results
  -l, --label strings                Filter by labels (AND: must have ALL). Can combine with --label-any
      --label-any strings            Filter by labels (OR: must have AT LEAST ONE). Can combine with --label
  -n, --limit int                    Maximum issues to show (use 0 for unlimited) (default 100)
      --metadata-field stringArray   Filter by metadata field (key=value, repeatable)
      --mol string                   Filter to steps within a specific molecule
      --mol-type string              Filter by molecule type: swarm, patrol, or work
      --parent string                Filter to descendants of this bead/epic
      --plain                        Display issues as a plain numbered list
      --pretty                       Display issues in a tree format with status/priority symbols (default true)
  -p, --priority int                 Filter by priority
  -s, --sort string                  Sort policy: priority (default), hybrid, oldest (default "priority")
  -t, --type string                  Filter by issue type (task, bug, feature, epic, decision, merge-request). Aliases: mr→merge-request, feat→feature, mol→molecule, dec/adr→decision
  -u, --unassigned                   Show only unassigned issues
```
(Global flags omitted — identical set listed once in §2 preamble.)
</details>

<details><summary><code>bd list --help</code> (full)</summary>

```
List issues

Usage:
  bd list [flags]

Flags:
      --all                          Show all issues including closed (overrides default filter)
  -a, --assignee string              Filter by assignee
      --closed-after string          Filter issues closed after date (YYYY-MM-DD or RFC3339)
      --closed-before string         Filter issues closed before date (YYYY-MM-DD or RFC3339)
      --created-after string         Filter issues created after date (YYYY-MM-DD or RFC3339)
      --created-before string        Filter issues created before date (YYYY-MM-DD or RFC3339)
      --defer-after string           Filter issues deferred after date (supports relative: +6h, tomorrow)
      --defer-before string          Filter issues deferred before date (supports relative: +6h, tomorrow)
      --deferred                     Show only issues with defer_until set
      --desc-contains string         Filter by description substring (case-insensitive)
      --due-after string             Filter issues due after date (supports relative: +6h, tomorrow)
      --due-before string            Filter issues due before date (supports relative: +6h, tomorrow)
      --empty-description            Filter issues with empty or missing description
      --exclude-label strings        Exclude issues that have ANY of these labels
      --exclude-type strings         Exclude issue types from results (comma-separated or repeatable, e.g., --exclude-type=convoy,epic)
      --flat                         Disable tree format and use legacy flat list output
      --format string                Output format: 'digraph' (for golang.org/x/tools/cmd/digraph), 'dot' (Graphviz), or Go template
      --has-metadata-key string      Filter issues that have this metadata key set
  -h, --help                         help for list
      --id string                    Filter by specific issue IDs (comma-separated, e.g., bd-1,bd-5,bd-10)
      --include-gates                Include gate issues in output (normally hidden)
      --include-infra                Include infrastructure beads (agent/rig/role/message) in output
      --include-templates            Include template molecules in output
  -l, --label strings                Filter by labels (AND: must have ALL). Can combine with --label-any
      --label-any strings            Filter by labels (OR: must have AT LEAST ONE). Can combine with --label
      --label-pattern string         Filter by label glob pattern (e.g., 'tech-*' matches tech-debt, tech-legacy)
      --label-regex string           Filter by label regex pattern (e.g., 'tech-(debt|legacy)')
  -n, --limit int                    Limit results (default 50, use 0 for unlimited) (default 50)
      --long                         Show detailed multi-line output for each issue
      --metadata-field stringArray   Filter by metadata field (key=value, repeatable)
      --mol-type string              Filter by molecule type: swarm, patrol, or work
      --no-assignee                  Filter issues with no assignee
      --no-labels                    Filter issues with no labels
      --no-pager                     Disable pager output
      --no-parent                    Exclude child issues (show only top-level issues)
      --no-pinned                    Exclude pinned issues
      --notes-contains string        Filter by notes substring (case-insensitive)
      --overdue                      Show only issues with due_at in the past (not closed)
      --parent string                Filter by parent issue ID (shows children of specified issue)
      --pinned                       Show only pinned issues
      --pretty                       Display issues in a tree format with status/priority symbols
  -p, --priority string              Priority (0-4 or P0-P4, 0=highest)
      --priority-max string          Filter by maximum priority (inclusive, 0-4 or P0-P4)
      --priority-min string          Filter by minimum priority (inclusive, 0-4 or P0-P4)
      --ready                        Show only ready issues (no active blockers, same semantics as bd ready)
  -r, --reverse                      Reverse sort order
      --skip-labels                  Skip label hydration. The labels field in output will be empty regardless of actual labels. Use only when the caller does not depend on label data. Cannot combine with --label, --label-any, --label-pattern, --label-regex, --exclude-label, or --no-labels.
      --sort string                  Sort by field: priority, created, updated, closed, status, id, title, type, assignee
      --spec string                  Filter by spec_id prefix
  -s, --status string                Filter by stored status (open, in_progress, blocked, deferred, closed). Comma-separated for multiple: --status open,in_progress
      --title string                 Filter by title text (case-insensitive substring match)
      --title-contains string        Filter by title substring (case-insensitive)
      --tree                         Hierarchical tree format (default: true; use --flat to disable) (default true)
  -t, --type string                  Filter by type (bug, feature, task, epic, chore, decision, merge-request, molecule, gate, convoy). Aliases: mr→merge-request, feat→feature, mol→molecule, dec/adr→decision
      --updated-after string         Filter issues updated after date (YYYY-MM-DD or RFC3339)
      --updated-before string        Filter issues updated before date (YYYY-MM-DD or RFC3339)
  -w, --watch                        Watch for changes and auto-update display (implies --pretty)
      --wisp-type string             Filter by wisp type: heartbeat, ping, patrol, gc_report, recovery, error, escalation

Global Flags:
      --actor string              Actor name for audit trail (default: $BEADS_ACTOR, git user.name, $USER)
      --db string                 Database path (default: auto-discover .beads/*.db)
  -C, --directory string          Change to this directory before running the command (like git -C)
      --dolt-auto-commit string   Dolt auto-commit policy (off|on|batch). 'on': commit after each write. 'batch': defer commits to bd dolt commit; uncommitted changes persist in the working set until then. SIGTERM/SIGHUP flush pending batch commits. Default: off. Override via config key dolt.auto-commit
      --global                    Use the global shared-server database (beads_global)
      --ignore-schema-skew        Proceed despite forward schema drift (some queries may fail)
      --json                      Output in JSON format
      --profile                   Generate CPU profile for performance analysis
  -q, --quiet                     Suppress non-essential output (errors only)
      --readonly                  Read-only mode: block write operations (for worker sandboxes)
      --sandbox                   Sandbox mode: disables Dolt auto-push
  -v, --verbose                   Enable verbose/debug output
```
(Global flags omitted — identical set listed once in §2 preamble.)
</details>

<details><summary><code>bd show --help</code> (full)</summary>

```
Show issue details

Usage:
  bd show [id...] [--id=<id>...] [--current] [flags]

Aliases:
  show, view

Flags:
      --as-of string         Show issue as it existed at a specific commit hash or branch (requires Dolt)
      --children             Show only the children of this issue
      --current              Show the currently active issue (in-progress, hooked, or last touched)
  -h, --help                 help for show
      --id stringArray       Issue ID (use for IDs that look like flags, e.g., --id=gt--xyz)
      --include-comments     Stream full comment bodies in JSON output (--json only; may be slow on issues with many comments)
      --include-dependents   Stream full dependent issues in JSON output (--json only; may be slow on hub beads)
      --local-time           Show timestamps in local time instead of UTC
      --long                 Show all available fields (extended metadata, agent identity, gate fields, etc.)
      --refs                 Show issues that reference this issue (reverse lookup)
      --short                Show compact one-line output per issue
      --thread               Show full conversation thread (for messages)
  -w, --watch                Watch for changes and auto-refresh display
```
(Global flags omitted.)
</details>

<details><summary><code>bd update --help</code> (full)</summary>

```
Update one or more issues.

If no issue ID is provided, updates the last touched issue (from most recent
create, update, show, or close operation).

Usage:
  bd update [id...] [flags]

Flags:
      --acceptance string            Acceptance criteria
      --add-label strings            Add labels (repeatable)
      --allow-empty-description      Allow empty description replacement when reading from stdin or file
      --append-notes string          Append to existing notes (with newline separator)
  -a, --assignee string              Assignee
      --await-id string              Set gate await_id (e.g., GitHub run ID for gh:run gates)
      --body-file string             Read description from file (use - for stdin)
      --claim                        Atomically claim the issue (sets assignee to you, status to in_progress; idempotent if already claimed by you)
      --defer string                 Defer until date (empty to clear). Issue hidden from bd ready until then
  -d, --description string           Issue description
      --design string                Design notes
      --design-file string           Read design from file (use - for stdin)
      --due string                   Due date/time (empty to clear). Formats: +6h, +1d, +2w, tomorrow, next monday, 2025-01-15
      --ephemeral                    Mark issue as ephemeral (wisp) - not exported to JSONL
  -e, --estimate int                 Time estimate in minutes (e.g., 60 for 1 hour)
      --external-ref string          External reference (e.g., 'gh-9', 'jira-ABC', Linear URL)
  -h, --help                         help for update
      --history                      Clear no-history flag (re-enable Dolt commit history)
      --metadata string              Set custom metadata (JSON string or @file.json to read from file)
      --no-history                   Mark issue as no-history (skip Dolt commits, not GC-eligible)
      --notes string                 Additional notes
      --parent string                New parent issue ID (reparents the issue, use empty string to remove parent)
      --persistent                   Mark issue as persistent (promote wisp to regular issue)
  -p, --priority string              Priority (0-4 or P0-P4, 0=highest)
      --remove-label strings         Remove labels (repeatable)
      --session string               Claude Code session ID for status=closed (or set CLAUDE_SESSION_ID env var)
      --set-labels strings           Set labels, replacing all existing (repeatable)
      --set-metadata stringArray     Set metadata key=value (repeatable, e.g., --set-metadata team=platform)
      --spec-id string               Link to specification document
  -s, --status string                New status
      --stdin                        Read description from stdin (alias for --body-file -)
      --title string                 New title
  -t, --type string                  New type (bug|feature|task|epic|chore|decision); custom types require types.custom config
      --unset-metadata stringArray   Remove metadata key (repeatable, e.g., --unset-metadata team)
```
(Global flags omitted. No `--lease`, `--lease-ttl`, or `--heartbeat` flag present.)
</details>

<details><summary><code>bd comment --help</code> (full)</summary>

```
Add a comment to an issue.

Shorthand for 'bd comments add <id> "text"'.

Examples:
  bd comment bd-123 "Working on this now"
  bd comment bd-123 Working on this now
  echo "comment from pipe" | bd comment bd-123 --stdin
  bd comment bd-123 --file notes.txt

Usage:
  bd comment <id> [text...] [flags]

Flags:
      --file string   Read comment text from file
  -h, --help          help for comment
      --stdin         Read comment text from stdin
```
(Global flags omitted.)
</details>

<details><summary><code>bd close --help</code> (full)</summary>

```
Close one or more issues.

If no issue ID is provided, closes the last touched issue (from most recent
create, update, show, or close operation).

When closing multiple issues, provide one --reason for all IDs or repeat
--reason once per ID. Reasons map positionally: the first --reason applies
to the first ID, the second --reason to the second ID, regardless of where
the flags appear in the command line.

Usage:
  bd close [id...] [flags]

Aliases:
  close, done

Flags:
      --claim-next           Automatically claim the next highest priority available issue
      --continue             Auto-advance to next step in molecule
  -f, --force                Force close pinned issues or unsatisfied gates
  -h, --help                 help for close
      --no-auto              With --continue, show next step but don't claim it
  -r, --reason string        Reason for closing
      --reason-file string   Read close reason from file (use - for stdin)
      --session string       Claude Code session ID (or set CLAUDE_SESSION_ID env var)
      --suggest-next         Show newly unblocked issues after closing
```
(Global flags omitted.)
</details>

<details><summary><code>bd set-state --help</code> (full)</summary>

```
Atomically set operational state on an issue.

This command:
1. Creates an event bead recording the state change (source of truth)
2. Removes any existing label for the dimension
3. Adds the new dimension:value label (fast lookup cache)

State labels follow the convention <dimension>:<value>, for example:
  patrol:active, patrol:muted
  mode:normal, mode:degraded
  health:healthy, health:failing

Examples:
  bd set-state agent-abc patrol=muted --reason "Investigating stuck worker"
  bd set-state agent-abc mode=degraded --reason "High error rate detected"
  bd set-state agent-abc health=healthy

The --reason flag provides context for the event bead (recommended).

Usage:
  bd set-state <issue-id> <dimension>=<value> [flags]

Flags:
  -h, --help            help for set-state
      --reason string   Reason for the state change (recorded in event)
```
(Global flags omitted.)
</details>

**`-a/--assignee` empty-string clear — EXECUTED PROBE, 2026-07-31.** A fresh,
independent `mktemp -d` sandbox was created, guarded by `trap 'rm -rf "$SANDBOX"' EXIT
INT TERM`, entirely under `$TMPDIR`, with its own `bd init --non-interactive` store.
Neither `gc init` nor `gc stop` was run, and no real bead store was touched. This probe
directly confirms the claim-clear-reclaim cycle `bin/kranz-dispatch` and
`bin/kranz-run-bead` rely on for releasing an abandoned claim:

```
$ bd create "Fixture bead for assignee-clear probe" --type task --json
{
  "id": "tmp_MzT4KEnbla-0if",
  ...
  "status": "open",
  ...
}

$ whoami
craigmartin

$ bd update tmp_MzT4KEnbla-0if --claim --json
[
  {
    "id": "tmp_MzT4KEnbla-0if",
    "status": "in_progress",
    "assignee": "craigmartin",
    ...
  }
]

$ bd update tmp_MzT4KEnbla-0if --status open --assignee "" --json
[
  {
    "id": "tmp_MzT4KEnbla-0if",
    "status": "open",
    ...
  }
]
```

(No `assignee` key appears in the row above — clearing it removes the field from the
JSON output entirely rather than emitting `"assignee": ""`.)

```
$ bd update tmp_MzT4KEnbla-0if --claim --actor other-actor --json
[
  {
    "id": "tmp_MzT4KEnbla-0if",
    "status": "in_progress",
    "assignee": "other-actor",
    ...
  }
]
```

**Confirmed by executed run:** `bd update <id> --status open --assignee ""` — the exact
shape both `bin/kranz-dispatch`'s `release_claim` and `bin/kranz-run-bead`'s exit-3/
exit-other cases issue — clears the assignee and reopens the issue in one call, and a
different actor's subsequent `--claim` then succeeds against that now-open,
now-unassigned issue. A bare `--assignee ""` with no accompanying `--status open` clears
the assignee field but leaves `status: in_progress`, which still blocks `--claim` for
another actor (`Error claiming ...: issue not claimable: status in_progress`, observed in
an earlier run of this probe) — confirming the bridge's combined `--status open
--assignee ""` call is the correct shape, not `--assignee ""` alone. The sandbox was
destroyed by the `trap`-based cleanup on script exit; nothing persists outside `$TMPDIR`.

## 3. Claim / lease support: executed probe **CONFIRMS no lease/TTL/heartbeat field** on installed `bd` 1.0.5

- `bd ready --claim` and `bd update --claim` both exist and are documented as
  "**Atomically** claim the issue (sets assignee to you, status to in_progress;
  idempotent if already claimed by you)" — this is a one-shot atomic claim (assignee +
  status), verified from live `--help` text above.
- **2026-07-30 executed data-model probe** (not just a `--help` text scan): a sandbox was
  created with `mktemp -d`, guarded by `trap 'rm -rf "$SANDBOX"' EXIT INT TERM`, entirely
  under `$TMPDIR`. Inside it, and nowhere else:
  1. `bd init --non-interactive` — created a fresh, self-contained store (see §5).
  2. `bd create "Fixture bead for dialect probe" --type task --json` — created a fixture
     issue, id `tmp_GKui4poa9n-llc`.
  3. `bd update tmp_GKui4poa9n-llc --claim --json` — claimed it.
  4. `bd show tmp_GKui4poa9n-llc --json` and `bd show tmp_GKui4poa9n-llc --json --long` —
     dumped the raw complete field set.

  Raw claimed-issue output (`bd update --claim --json`):

  ```json
  [
    {
      "id": "tmp_GKui4poa9n-llc",
      "title": "Fixture bead for dialect probe",
      "status": "in_progress",
      "priority": 2,
      "issue_type": "task",
      "assignee": "craigmartin",
      "owner": "ci@kranz.local",
      "created_at": "2026-07-30T02:23:51Z",
      "created_by": "craigmartin",
      "updated_at": "2026-07-30T02:23:52Z",
      "started_at": "2026-07-30T02:23:52Z",
      "dependent_count": 0,
      "dependency_count": 0,
      "comment_count": 0
    }
  ]
  ```

  **Complete field set observed: `id`, `title`, `status`, `priority`, `issue_type`,
  `assignee`, `owner`, `created_at`, `created_by`, `updated_at`, `started_at`,
  `dependent_count`, `dependency_count`, `comment_count`.** No `lease_expires_at`, no
  `heartbeat_at`, no field of any name carrying expiry/TTL/heartbeat/lease semantics is
  present — including with `--long`, which the `--help` text claims shows "extended
  metadata, agent identity, gate fields, etc." but which produced an identical field set
  for this claimed task issue.

  **2026-07-30, second executed probe — `bd show --json` itself actually run.** A
  fresh, independent `mktemp -d` sandbox was created (store name `bd_probe_aWzwPk`,
  issue prefix `bd-probe-aWzwPk`), guarded by `trap 'rm -rf "$SANDBOX"' EXIT INT TERM`,
  entirely under `$TMPDIR`, with its own `bd init --non-interactive` store. Neither
  `gc init` nor `gc stop` was run, and no real bead store was touched. A fixture issue
  `bd-probe-aWzwPk-1li` was created and claimed exactly as in steps 1–3 above, then the
  two `bd show` commands were run directly, back to back, and their raw stdout is
  pasted verbatim below (whitespace, key order, and values unmodified):

  ```
  $ bd show bd-probe-aWzwPk-1li --json
  [
    {
      "id": "bd-probe-aWzwPk-1li",
      "title": "Fixture bead for dialect probe",
      "status": "in_progress",
      "priority": 2,
      "issue_type": "task",
      "assignee": "craigmartin",
      "owner": "ci@kranz.local",
      "created_at": "2026-07-30T17:49:57Z",
      "created_by": "craigmartin",
      "updated_at": "2026-07-30T17:49:58Z",
      "started_at": "2026-07-30T17:49:58Z",
      "dependent_count": 0,
      "dependency_count": 0,
      "comment_count": 0
    }
  ]
  ```

  ```
  $ bd show bd-probe-aWzwPk-1li --json --long
  [
    {
      "id": "bd-probe-aWzwPk-1li",
      "title": "Fixture bead for dialect probe",
      "status": "in_progress",
      "priority": 2,
      "issue_type": "task",
      "assignee": "craigmartin",
      "owner": "ci@kranz.local",
      "created_at": "2026-07-30T17:49:57Z",
      "created_by": "craigmartin",
      "updated_at": "2026-07-30T17:49:58Z",
      "started_at": "2026-07-30T17:49:58Z",
      "dependent_count": 0,
      "dependency_count": 0,
      "comment_count": 0
    }
  ]
  ```

  These two transcripts independently confirm, from `bd show --json` itself rather
  than inference from `update --claim --json`: (1) `bd show <id> --json` with a single
  positional id returns a **single-element JSON array**, not a bare object; and (2) the
  field set carries no `lease_expires_at`, `heartbeat_at`, or any other expiry/TTL/
  heartbeat/lease-named field, identically with and without `--long`.
- There is also **no** lease, TTL, or heartbeat flag anywhere in `bd update --help`,
  `bd ready --help`, `bd show --help`, or the top-level `bd --help` subcommand list (see
  Appendix for the full top-level list). No `--lease`, `--lease-ttl`, `--heartbeat` was
  found as a flag either.
- **Version-gap caveat**: this executed probe ran against the **installed 1.0.5**
  binary. `docs/scoping/beads-workstore.md:66` records `lease_expires_at`/`heartbeat_at`
  as verified against **v1.1.0**'s `internal/types/types.go` source — a newer minor
  version than what is installed (see §1). This probe does not contradict that upstream
  v1.1.0 evidence; it establishes that **the currently-installed 1.0.5 binary this bridge
  actually runs against today does not expose those fields**, at least not on a plain
  `task`-type issue via `bd show --json`/`--long`. An upgrade path to 1.1.2 exists via
  `brew` (§1) and was not taken by this feature.
- **Escalation, updated for executed evidence**: against installed `bd` 1.0.5, there is
  no live mechanism (TTL, heartbeat, expiry timestamp) observable via `bd show --json`
  to detect a dead claimant and recover the bead. The mission's "lease-aware claim with
  liveness-first recovery" milestone **cannot be implemented as specified against bd
  1.0.5** without either (a) an operator-approved upgrade to 1.1.0+ where the research
  base's lease fields were verified against source, or (b) explicit operator sign-off on
  a different mechanism. Per the feature spec's explicit instruction, no substitute
  (emulating leases via comments, metadata, sentinel files, or status abuse) has been
  invented here. This finding is also recorded in the WorkerReport's `knownGaps`.

- **RESOLUTION NOTE, 2026-07-31:** resolved client-side per operator decision D-BW-2
  (accepted 2026-07-29) — NOT the rejected `updated_at`-staleness heuristic
  (`dbe5cf8`, reverted in `5a7585c`). The bridge now ships its own lease:
  `kranz-run-bead` renews `${KRANZ_LEASE_DIR:-.gc/kranz-leases}/<id>.lease`
  (`<pid> <unix-ts>`) while a mission runs (trap-cleaned on terminal exit), and
  `kranz-dispatch --reclaim` sweeps liveness-first — a live pid's claim is never
  reaped; a dead pid's claim is released; a lease-less claim is released only past
  `KRANZ_CLAIM_TTL` (default 120s, expiry strictly as backstop). Proven live by the
  `LEASE: PASS`, `LEASE-TTL: PASS`, and `LEASE-PRODUCER: PASS` cases in
  `packaging/gascity/test/kranz-dispatch-roundtrip.sh`. bd 1.0.5 still has no
  server-side lease field; an upstream mechanism (1.1.0+) remains the tracked path
  and this note should be revisited when it ships.

## 4. Shapes the spike used, at probe time, that the live binary did NOT accept

Read from `packaging/gascity/bin/kranz-run-bead` and `packaging/gascity/bin/kranz-dispatch`
at the time each bullet below was probed (read-only comparison only). Bullets below are
findings as of their stated date; where a later milestone has since changed the code, a
dated resolution note says so explicitly — absence of such a note means the finding is
still current.

- **[PROBE FINDING, 2026-07-29 — HISTORICAL]** `packaging/gascity/bin/kranz-run-bead:25`:
  `bd set-state "$ID" blocked` — **wrong shape**. Live `bd set-state` requires
  `<issue-id> <dimension>=<value>` (e.g. `bd set-state "$ID" mode=blocked`), not a bare
  status word. As written this call would fail against the live binary (it falls through
  to `bd update "$ID" --status blocked` via `||`, which is the shape that actually works).

  > **Resolution note (2026-07-30, ms-2-fix-2-3):** this defect no longer exists in the
  > shipped script. `packaging/gascity/bin/kranz-run-bead:46` now reads a bare `bd update
  > "$ID" --status blocked >/dev/null 2>&1` (verified by reading the current file) — the
  > `set-state` call and its `||` fallback chain were removed entirely, not merely
  > reordered. `packaging/gascity/test/check-bridge-hygiene.sh` (default mode) now greps
  > `packaging/gascity/bin/` for `set-state` and fails the build if it reappears, so this
  > regression is guarded going forward. The probe finding above is preserved verbatim as
  > historical evidence of what the live binary accepts; it no longer describes the
  > current tree.
- `packaging/gascity/bin/kranz-dispatch` calls `gc bd ready --label "$LABEL" --json`
  (line 64), `gc bd show "$ID" --json` (line 79), and `gc bd update "$ID" --status
  in_progress` (line 96) — these could
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

## 5. `bd init` in an arbitrary empty directory — EXECUTED, 2026-07-30

`bd init --help` describes `bd init` as: "Initialize bd in the current directory by
creating a `.beads/` directory and Dolt database." It defaults to an **embedded Dolt
engine** ("no external server needed") with the issue prefix defaulting to the current
directory name, and `--non-interactive` is auto-detected in CI / non-TTY environments.

This was **actually run** on 2026-07-30, inside the same `mktemp -d` sandbox used for
§3, guarded by the same `trap ... EXIT INT TERM`:

```
$ bd init --non-interactive
  ✓ Initialized git repository
  Repository ID: 9104953a
  ...
✓ bd initialized successfully!

  Backend: dolt
  Mode: embedded
  Database: tmp_GKui4poa9n
  Issue prefix: tmp_GKui4poa9n
  Issues will be named: tmp_GKui4poa9n-<hash> (e.g., tmp_GKui4poa9n-a3f2dd)
```

**Confirmed by executed run, not inferred from `--help` text:** `bd init
--non-interactive` (no other flags) successfully creates a self-contained, standalone
store — embedded Dolt engine, no external server, no Gas City rig registration required
— in an arbitrary empty directory, with the issue prefix auto-derived from the directory
name. The `bd create` and `bd update --claim` calls in §3 against this exact store
succeeded immediately afterward, further confirming the store was immediately usable.
This resolves the "not actually run" gap the previous version of this document left open;
the sandbox was destroyed by the `trap`-based cleanup on script exit and nothing persists
outside `$TMPDIR`.

## Appendix: other probes

**Full `bd --help` top-level subcommand listing** (captured 2026-07-30; this is the
complete list §3's lease negative and §2's verb-existence claims are checked against):

```
Issues chained together like beads. A lightweight issue tracker with first-class dependency support.

Usage:
  bd [flags]
  bd [command]

Working With Issues:
  assign          Assign an issue to someone
  children        List child beads of a parent
  close           Close one or more issues
  comment         Add a comment to an issue
  comments        View or manage comments on an issue
  create          Create a new issue (or batch from markdown/graph JSON)
  create-form     Create a new issue using an interactive form
  delete          Delete one or more issues and clean up references
  edit            Edit an issue field in $EDITOR
  gate            Manage async coordination gates
  label           Manage issue labels
  link            Link two issues with a dependency
  list            List issues
  merge-slot      Manage merge-slot gates for serialized conflict resolution
  note            Append a note to an issue
  priority        Set the priority of an issue
  promote         Promote a wisp to a permanent bead
  q               Quick capture: create issue and output only ID
  query           Query issues using a simple query language
  reopen          Reopen one or more closed issues
  search          Search issues by text query
  set-state       Set operational state (creates event + updates label)
  show            Show issue details
  state           Query the current value of a state dimension
  tag             Add a label to an issue
  todo            Manage TODO items (convenience wrapper for task issues)
  update          Update one or more issues

Views & Reports:
  count           Count issues matching filters
  diff            Show changes between two commits or branches
  find-duplicates Find semantically similar issues using text analysis or AI
  history         Show version history for an issue
  lint            Check issues for missing template sections
  stale           Show stale issues (not updated recently)
  status          Show issue database overview and statistics
  statuses        List valid issue statuses
  types           List valid issue types

Dependencies & Structure:
  dep             Manage dependencies
  duplicate       Mark an issue as a duplicate of another
  duplicates      Find and optionally merge duplicate issues
  epic            Epic management commands
  graph           Display issue dependency graph
  supersede       Mark an issue as superseded by a newer one
  swarm           Swarm management for structured epics

Sync & Data:
  backup          Back up your beads database
  branch          List or create branches
  export          Export issues to JSONL format
  federation      Manage peer-to-peer federation with other workspaces
  import          Import issues from a JSONL file or stdin into the database
  restore         Restore full history of a compacted issue from Dolt history
  vc              Version control operations

Setup & Configuration:
  bootstrap       Non-destructive database setup for fresh clones and recovery
  config          Manage configuration settings
  context         Show effective backend identity and repository context
  dolt            Configure Dolt database settings
  forget          Remove a persistent memory
  hooks           Manage git hooks for beads integration
  human           Show essential commands for human users
  info            Show database information
  init            Initialize bd in the current directory
  kv              Key-value store commands
  memories        List or search persistent memories
  onboard         Display minimal snippet for agent instructions file
  prime           Output AI-optimized workflow context
  quickstart      Quick start guide for bd
  recall          Retrieve a specific memory
  remember        Store a persistent memory
  setup           Setup integration with AI editors
  where           Show active beads location

Maintenance:
  batch           Run multiple write operations in a single database transaction
  compact         Squash old Dolt commits to reduce history size
  doctor          Check and fix beads installation health (start here)
  flatten         Squash all Dolt history into a single commit
  gc              Garbage collect: decay old issues, compact Dolt commits, run Dolt GC
  migrate         Database migration commands
  ping            Check database connectivity
  preflight       Show PR readiness checklist
  prune           Delete old closed beads to reclaim space and shrink exports
  purge           Delete closed ephemeral beads to reclaim space
  rename-prefix   Rename the issue prefix for all issues in the database
  rules           Audit and compact Claude rules
  sql             Execute raw SQL against the beads database
  upgrade         Check and manage bd version upgrades
  worktree        Manage git worktrees for parallel development

Integrations & Advanced:
  admin           Administrative commands for database maintenance
  jira            Jira integration commands
  linear          Linear integration commands
  repo            Manage multiple repository configuration

Additional Commands:
  ado             Azure DevOps integration commands
  audit           Record and label agent interactions (append-only JSONL)
  blocked         Show blocked issues
  completion      Generate the autocompletion script for the specified shell
  cook            Compile a formula into a proto (ephemeral by default)
  defer           Defer one or more issues for later
  formula         Manage workflow formulas
  github          GitHub integration commands
  gitlab          GitLab integration commands
  help            Help about any command
  init-safety     Explain bd init flag semantics and the destroy-token format
  mail            Delegate to mail provider (e.g., gt mail)
  mol             Molecule commands (work templates)
  notion          Notion integration commands
  orphans         Identify orphaned issues (referenced in commits but still open)
  ready           Show ready work (open, no active blockers)
  rename          Rename an issue ID
  ship            Publish a capability for cross-project dependencies
  undefer         Undefer one or more issues (restore to open)
  version         Print version information
```

None of the ~90 top-level verb names above name a dedicated lease/heartbeat/expiry
command (no `lease`, `heartbeat`, `expire`, `ttl`, or similar verb exists), consistent
with §3's executed-probe finding that no such field exists in the data model observed.

- `bd --help` top-level subcommand list (above) confirms `ready`, `show`, `update`,
  `comment`, `close`, `set-state`, `init` all exist as top-level verbs on this version.
  `statuses` also exists (`bd statuses` — "List valid issue statuses"), useful for a
  downstream feature that wants to validate `--status` values against the live dialect
  rather than hardcoding them.
- `bd --help`: full output captured during the probe session and reproduced verbatim
  above; abbreviated to the subcommands relevant to the bridge in the discussion
  elsewhere in this document, but the complete listing is preserved here so nothing is
  taken on the author's word.
- No real bead was created, read from, updated, claimed, commented on, or closed against
  any pre-existing live database, in-repo or otherwise. `bd show`/`bd list --json` were
  attempted read-only against an **out-of-repo** existing store
  (`<operator-home>/rig-hello-world/.beads`, found via filesystem probe) to try to
  resolve the object-vs-array question in §2, but that store's Dolt server was not
  running and auto-start is disabled there (`Dolt server unreachable ... auto-start is
  disabled (dolt.auto-start: false)`), so no JSON body was actually observed. The
  object-vs-array question was subsequently resolved by the executed sandbox probe in §3
  instead (2026-07-30): `bd show --json` with a single positional id returns a
  single-element array.
