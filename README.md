# Kranz

**Git-native mission control for headless coding agents.** Kranz is a local
governance and evidence harness—named for Gene Kranz, the Apollo flight
director. An orchestrator plans, fresh-context workers implement one feature
at a time, independent validators judge each milestone, and a human steers as
project manager. The harness never touches the spacecraft; it runs the room.

The product is the few seconds between an agent wanting to act and a human
trusting it. The consent surface — plan approval, command grants, the merge
gate — is kranz's answer to that interval, and the flight-surgeon console
(`kranz outcomes`) measures it: grant-latency buckets turn "do you actually
review?" into data.

Built in Rust with Claude Code as the default runtime. Role-specific backends
also support Codex CLI, Factory Droid, Kimi Code, Cursor, ACP-compatible
agents, and OpenAI-compatible local inference. Claude sessions can use a
repository's `CLAUDE.md` and `.claude/skills`; a repository's own
`.claude/settings.json`, hooks, and `.mcp.json` are never loaded, because they
run commands before the model's first turn (only the operator's user-level
settings apply). Git is the source of truth; an append-only, hash-chained event
log makes every mission `kill -9`-safe.

```
┌───────────┐   plan/judge    ┌────────────────────────────────┐
│ you (PM)  │◄──────────────► │ orchestrator (opus, read-only) │
└─────┬─────┘                 └───────────────┬────────────────┘
      │ kranz msg / dashboard                 │ engine loop (§4.5)
      ▼                                       ▼
  events.jsonl ◄── single writer ──  workers (sonnet, fresh session per
  state.json                         feature) → validators (scrutiny +
  plan.json (committed)              functional, fresh per milestone)
```

## Quick start

Prerequisites: Git and at least one configured agent backend. Cargo and source
installs also require Rust 1.88+; [prebuilt binaries](#install) are available.
The default backend is the [Claude Code CLI](https://claude.com/claude-code)
(`claude`), installed and authenticated. Kranz discovers it on `PATH` and in
the usual install locations, or you can set `KRANZ_CLAUDE_BIN` / `claudeBinary`
in configuration. Other backends have role and sandbox restrictions; run
`kranz ready` before the first mission.

```sh
cargo install kranz --version 0.3.0 --locked
cd /path/to/your/repo             # must be a git repo

# 0. Onboard the repo. This detects common Rust/Node/Python gates, adds the
#    runtime ignore template, and optionally registers it with multi-repo serve.
kranz init --register
# Unfamiliar toolchain: repeat --gate, e.g. kranz init --gate "make verify"

# 1. Plan interactively — contract first, then milestones/features.
#    /plan renders the proposal + cost estimate; approval commits plan.json
#    onto a new branch kranz/mission-<id>.
kranz plan "Add a REST endpoint with auth, then a small CLI client"

# 2. Execute unattended. Live role-tagged event tail; Ctrl-C (or kill -9) is
#    safe — rerun to resume from the event log.
kranz run

# 3. Watch / steer from another terminal or the dashboard:
kranz status                      # terminal tree + tokens/cost
kranz msg "swap feature 4 for X"  # queued between worker runs
kranz msg --interrupt "stop"      # aborts the current worker first
kranz pause | kranz resume
kranz config show                 # effective merged config (+ the layer files)
kranz config set worker.model opus       # edit .kranz/config.json (validated)
kranz config role worker opus xhigh      # MID-MISSION: applies at next worker spawn
kranz serve --open                # Mission Control dashboard (browser)
```

`kranz serve` includes an embedded dashboard bundle. When developing the UI,
run `cd <kranz checkout>/apps/dashboard && npm install && npm run build && npm run sync-embedded`;
`kranz serve` will prefer that fresh build automatically (searching
`--dashboard DIR`, `$KRANZ_DASHBOARD_DIST`, `<repo>/apps/dashboard/dist`,
installed asset dirs such as `~/.kranz/dashboard/dist`, then the kranz checkout
used to build the binary, before falling back to the embedded bundle).

Desktop app: `cd apps/dashboard && npm install && npm run build && npx tauri dev`
(reads `KRANZ_REPO`; see `apps/dashboard/README.md`).

## Install

**With Cargo.** Install the published
[v0.3.0 release](https://crates.io/crates/kranz/0.3.0), including the embedded
web dashboard:

```sh
cargo install kranz --version 0.3.0 --locked
```

Most users install only `kranz`; Cargo builds its three library dependencies
automatically.

**Prebuilt binaries.** [Download v0.3.0 from GitHub Releases](https://github.com/craigcode/kranz/releases/tag/v0.3.0)
for Linux x86_64, macOS Apple Silicon or Intel, and Windows ARM64 or x86_64.
Extract the archive and put `kranz` (`kranz.exe` on Windows) on your `PATH`.
The release includes checksums, build provenance attestations, an SBOM, and
license notices. `kranz licenses` prints the bundled project, Rust dependency,
and dashboard notices.

**From source.** To build the current public repository:

```sh
git clone https://github.com/craigcode/kranz.git
cargo install --path kranz/crates/cli --locked
```

If you already have a checkout, run `cargo install --path crates/cli --locked`
from its root.

Homebrew distribution is not available yet.

### Agent runtime and authentication

Kranz orchestrates agent CLIs; it does not install them or sign into their
accounts. Install at least one supported runtime from its vendor, authenticate
it outside Kranz, and leave its native executable discoverable on `PATH`.
Claude Code is the default. Codex, Factory Droid, Kimi Code, Cursor, ACP peers,
and OpenAI-compatible local endpoints can be selected per role. API-key
authentication is passed only through each backend's sanctioned variable, not
through the ambient environment. See [Agent backends](docs/agent-backends.md),
then run `kranz ready` before the first mission.

### Crates and API stability

Most users install only `kranz`. Cargo fetches the other crates because they
are reusable components of the product:

| Crate | Provides |
|---|---|
| [`kranz`](https://crates.io/crates/kranz/0.3.0) | The CLI, `kranz serve`, and embedded Mission Control dashboard. |
| [`kranz-engine`](https://crates.io/crates/kranz-engine/0.3.0) | Mission orchestration, isolation, gates, validation, evidence, and controlled local merging. |
| [`kranz-server`](https://crates.io/crates/kranz-server/0.3.0) | The REST/WebSocket mission host used by `kranz serve` and custom front ends. |
| [`kranz-slack`](https://crates.io/crates/kranz-slack/0.3.0) | The Slack Socket Mode bridge for operating and observing missions. |

The three library crates are public for reuse, but their Rust APIs are early
and evolving in the v0.3 line. Pin exact versions if embedding them; semantic
compatibility is not yet promised beyond Cargo's normal pre-1.0 rules.

## The four roles

| Role | Job | Lifetime | Sees | Touches | Default |
|---|---|---|---|---|---|
| Orchestrator | Plans contract-first; judges worker reports; converts or waives findings; decides respawns, dirty trees, unblocks; judges `agent-judgement` assertions at the final gate | One long-lived session per mission (resumable/re-seedable) | Digest, plan, reports, findings — never raw transcripts | Nothing — read + git-inspect only; the engine writes/commits `plan.json`/`plan.md` | opus · high |
| Worker | Implements exactly one feature: tests first, implement until green, lint/build, commit `[feature-id]`-prefixed, end with a `WorkerReport` | Fresh per feature (bounded respawns) | Its spec + criteria + goal — never the mission transcript | Edits + Bash in the repo; denied push/publish/network/sudo | sonnet · medium |
| Validator · scrutiny | Adversarial review of the milestone diff: tests asserting implementation, dead criteria, integration seams, out-of-intent regressions | Fresh per validation round | Milestone spec, contract, `start-sha..HEAD` diff | Read-only + inspect commands | opus · high |
| Validator · functional | Judges the engine-captured results of contract commands and configured test/build/lint scripts; reports pass/fail from bounded verbatim evidence | Fresh per validation round | Contract results + milestone criteria | Read-only; optional explicitly configured live-QA tools | sonnet · medium |

Two validators catch different failures: functional judges whether the
engine-run gates worked; scrutiny catches "it runs but it's wrong"—passing
tests that assert the implementation, unwired criteria, and broken seams
between features.

## How it works

- **Contract first.** Planning defines behavioural assertions *before* any
  feature exists. Command assertions gate mission completion mechanically; the
  rest are judged by the orchestrator against the full mission diff.
- **Fresh contexts.** No worker ever sees the mission transcript — only its
  feature spec, criteria, and a plan excerpt. Validators never see the
  worker's reasoning, only the diff and the contract.
- **Bounded loops.** Failing validation converges to a *blocked* milestone
  (loud in UI and CLI) after `maxFixCyclesPerMilestone`, never to infinite
  spend. Worker respawns are capped; per-run dollar budgets are enforced by
  the CLI itself.
- **Permissions per role (§4.7).** Orchestrator: read + git-inspect only.
  Workers: edits allowed, deny-listed from push/publish/network. Validators:
  read-only + the contract's commands. Denials surface as events (the
  dashboard shows guardrails firing). `--dangerously-allow-all` exists, is
  loud, and is never the default.
- **Kranz never pushes by default.** The mission branch is the deliverable;
  you review and open the PR. The sole exception is an explicit cloud handoff:
  `kranz exec --push <configured-remote>` may publish one completed `kranz/*`
  branch for human review, never a base branch, tag, force-push, or merge.

## Layout

| Path | What |
|---|---|
| `crates/engine` | orchestration core: event log, reducer, backends, runners, mission loop |
| `crates/cli` | the `kranz` binary |
| `crates/server` | axum REST + WebSocket ([docs/protocol.md](docs/protocol.md)) |
| `apps/dashboard` | Mission Control UI (React/Vite) + Tauri shell |
| `docs/` | design notes, protocol, dashboard reference |
| `.kranz/missions/<id>/` | per-repo mission data (events.jsonl, state.json, plan.json, transcripts) |

Config: `.kranz/config.json` (project) over `~/.kranz/config.json` (global) —
per-role models/effort/budgets, fix-cycle caps, deny patterns. Defaults use
model aliases (`opus`, `sonnet`) so they track the latest releases. Inspect and
edit the layers with `kranz config show|set|unset` (`--global` targets the home
file; edits are validated before writing and only shape *future* missions);
`kranz config role <role> <model> [effort]` is the mid-mission path — it queues
a config-change on the running mission, like Slack's `/kranz config`.

`kranz init` is additive and idempotent: it never replaces an existing gate
suite, and it preserves existing `.gitignore` and global-config keys. Pass
`--register [--id ID] [--display-name NAME]` to add the canonical root to the
dashboard/Slack host catalog. Its first-run report calls out the safe
`worktree` isolation default and a zero-mission calibration cold start.

The human-triggered Merge action uses the tracked
`.kranz/merge-gates.json` from the live base branch (schema and examples:
[docs/merge-gates.md](docs/merge-gates.md)). Each gate declares a
command, optional repo-relative working directory, and optional changed-path
prefixes; missing or invalid suites fail closed. Because the mission branch
does not supply the suite that judges it, a mission cannot weaken its own
merge checks or secret waivers. Gates run on the exact pinned mission/base
integration commit in a scratch worktree; only that tested commit can advance
the base.

## Development

```sh
cargo test --workspace          # full workspace suite; no model calls by default
cargo test -p kranz-engine -- --ignored   # + live smoke test (spawns claude)
node scripts/mock-server.mjs    # dashboard dev harness with a canned mission
```

The full contributor gate and pull-request expectations are in
[`CONTRIBUTING.md`](CONTRIBUTING.md). Security issues belong in the private
reporting channel described by [`SECURITY.md`](SECURITY.md), not a public issue.

## Cloud (preview)

Kranz is local-first and never pushes during ordinary local missions or gated
merge. Cloud missions (M6, preview — not yet exercised end-to-end) may use the
explicit `kranz exec --push <configured-remote>` handoff to publish the
completed mission branch as a reviewable `kranz/*` ref only — never `main`, a
tag, force-push, or merge. See the [`Dockerfile`](Dockerfile) and the
[cloud deploy runbook](docs/deploy.md).
