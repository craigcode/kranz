# M8 scoping — One host, many repository roots

Status: design accepted 2026-07-15. Implementation is intentionally split
from the dashboard project picker and fresh-repository onboarding.

## Why

Kranz state is correctly repository-local: tickets, queues, missions, gates,
calibration, and lessons belong to the repository they govern. The operator
surfaces are not. `kranz serve`, the REST API, the dashboard, and the Slack
bridge currently hold one `repo_root`, so running several repositories either
requires several servers or creates ambiguous mission and command identities.
Several Slack socket-mode connections are not a substitute: Slack may
load-balance events for one app across them, leaving no bridge with a complete
view.

The host therefore needs a small routing plane around the existing per-repo
engine. It must never make repository state global and must never infer a
mutation target from a path supplied by a client.

## Accepted decisions

### D-A — The repository catalog is operator-owned global config

Add a `host` block to `~/.kranz/config.json`:

```json
{
  "host": {
    "defaultRepo": "kranz",
    "maxConcurrentRepos": 1,
    "repos": [
      {
        "id": "kranz",
        "root": "/absolute/path/to/kranz",
        "displayName": "Kranz",
        "group": "Core",
        "pinned": true,
        "slack": {
          "channels": [{ "team": "T123", "channel": "C123" }],
          "allowUsers": ["U123"]
        }
      }
    ]
  }
}
```

- `id` is an operator-chosen, stable, URL-safe slug. It is unique and is not
  derived from a path or Git remote.
- `root` must be absolute. The host canonicalizes it once at startup and
  rejects duplicate ids and duplicate canonical roots.
- Display name, group, pin state, routing, and spend allowlists are
  operator-owned. A repository must not be able to change where operator
  commands route.
- Backend secrets remain in the global `slack` configuration. Mission policy
  and defaults remain in each repository's `.kranz/config.json`.
- The first implementation loads a static catalog for the process lifetime;
  edits take effect on restart. Hot rebinding is deferred because changing a
  root beneath a live engine is unsafe.

### D-B — Compose existing per-repo hosts; do not make the engine multi-root

Keep one `MissionHost` per repository and add a `MultiRepoHost`/catalog that
maps `RepoId` to `Arc<MissionHost>`. A resolved request carries a `RepoContext`
containing the catalog entry and host. Filesystem paths never come from an API
or Slack argument.

Namespace repository operations under `/api/repos/{repo_id}/...` and add
`GET /api/repos` for the catalog and health summary. Existing unscoped routes
may remain as a migration alias only when exactly one healthy repository is
configured or an explicit `defaultRepo` is configured. Ambiguous unscoped
mutation requests fail; they never select the most recently used repository.

Dashboard deep links carry repository identity, for example
`#/r/kranz/m/mission-123`. This design does not implement the picker UI.

### D-C — One process token authenticates the local host; the path supplies scope

Use one random serve token per process. It is a local-process/CSRF secret, not
a user identity or an RBAC capability. The router verifies the token and then
resolves the repository id from the route; possession of the token does not
permit a handler to escape its resolved `RepoContext`.

Store the multi-root token in an operator runtime location such as
`~/.kranz/serve/<instance>.token` with mode `0600`, rather than writing it into
every hosted repository. Preserve `.kranz/serve.token` for the single-repo
migration path. Off-loopback reads continue to require the token.

Slack does not call the REST surface and therefore does not present this
token. It authenticates through the Slack connection, resolves a repository,
then applies that repository's operator-owned `allowUsers` rule before any
spend-adjacent action. Per-user, per-repo, and capability tokens belong to the
future remote/RBAC milestone, not this local M8 host.

### D-D — Mission identity is composite at the host boundary

Mission ids, ticket slugs, queues, event logs, and persisted state remain
repository-local and retain their current schemas. At every host boundary use
`RepoMissionId { repo_id, mission_id }`. API paths, dashboard links, Slack
affinity, logs that aggregate repositories, and in-memory registries must not
accept a bare mission id in multi-root mode.

Duplicate mission ids in two repositories are valid and are a required test
fixture. No event-schema migration is necessary.

### D-E — Queues stay local; scheduling is globally fair and conservatively bounded

Each repository retains `.kranz/queue`, its claim files, and its repository
busy lock. An explicit drain operation names one repository. There is no
global queue and no operation that moves a queue entry between repositories.

Auto-work uses fair round-robin selection across ready queue fronts. A parked,
unavailable, or busy repository is skipped without blocking other roots. Each
repository stays serial internally. `host.maxConcurrentRepos` limits how many
repositories may run concurrently and defaults to `1`, preserving today's
spend and machine-load profile; operators may deliberately raise it.

### D-F — Slack routing is explicit, with thread affinity first

One socket-mode connection and one bridge serve the whole catalog. Inbound
events must retain both `team_id` and `channel_id`.

Routing precedence is:

1. An existing mission thread's composite affinity (`repo_id`, `mission_id`).
2. An explicit leading `repo:<id>` argument for a top-level command.
3. An exact operator-configured `(team_id, channel_id)` mapping.
4. The sole healthy repository, only when exactly one exists.
5. Otherwise refuse with a short list of valid repository ids.

Thread affinity wins over a later channel-default change. Modal
`private_metadata` carries `repo_id`; every submit handler resolves it again
and checks authorization. App Home may aggregate read-only status but must ask
for a repository before mutation; being user-scoped, its render can never be
disambiguated by a channel, so with several healthy repositories the Home tab
follows `host.defaultRepo` (and refuses, with the config key named, when none
is set). Every routing refusal is logged and answered ephemerally when the
payload carries reply coordinates — fail-closed must never mean fail-silent.

Replace the repo-local mission-only Slack thread map with an operator-owned
affinity map keyed by team/channel/thread and valued by composite mission
identity. Migrate existing single-repo entries when the catalog is introduced.
Notification cursors may stay repository-local; the one bridge tails every
healthy root.

### D-G — Missing repositories are visible and isolated

A missing, inaccessible, non-Git, or invalid repository becomes an
`unavailable` catalog row; it does not prevent healthy roots from being
served. Its routes return `503`, the scheduler skips it, and Slack refuses
mutations targeting it. Routing never falls back to another repository.

Canonical roots are fixed at startup. Catalog resolution uses ids only, and
handlers receive the already-resolved context, preventing traversal and
symlink rebinding through client input. Duplicate ids/roots fail host startup
because their routing is inherently ambiguous. Removing or rebinding a root
with a live hosted engine is refused; the static first version handles this by
requiring restart.

### D-H — Deliver in seams that preserve single-repo behavior

1. Parse and validate the global catalog; add pure routing and duplicate-root
   tests plus `RepoContext`.
2. Add the multi-host registry and repo-scoped REST/WS API, retaining the
   single-repo compatibility alias.
3. Add the round-robin scheduler and its concurrency bound.
4. Move Slack affinity to composite identities and implement explicit routing.
5. Build the separately ticketed dashboard project picker.
6. Add `kranz init` and fresh-repository onboarding.

Each stage keeps state in its existing repository. No migration may merge
mission directories, queues, or event logs across roots.

## Security and regression matrix

The implementation is not complete without tests proving:

- the same mission id in two roots cannot cross-read, cross-steer, or
  cross-abandon;
- a verified token plus one route id reaches only that route's `RepoContext`;
- queue claims and busy locks are independent while the global concurrency
  limit is honored;
- unavailable and moved roots return a visible error with no fallback;
- ambiguous Slack commands refuse, explicit/channel/thread routing agrees,
  and per-repo allowlists run after routing;
- clients cannot submit a filesystem path as repository identity; and
- single-repo routes and token discovery keep working during migration.

## Explicit non-goals

- The dashboard repository picker (`multi-repo-project-picker`).
- Fresh-repository scaffolding and `kranz init`.
- Per-user/RBAC or remote capability tokens.
- Remote workspace providers and cloud execution.
- A global queue, cross-repository mission moves, or cross-repository merges.

## Done when

One `kranz serve` and one Slack bridge can host at least two repositories with
duplicate local mission ids; all read and mutation paths carry repository
identity; queue scheduling is fair and bounded; a missing root remains visible
without affecting the other; and ambiguous Slack input fails closed.
