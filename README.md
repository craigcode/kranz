# Kranz

**Mission control for Claude Code.** Kranz is a local orchestration harness —
named for Gene Kranz, the Apollo flight director — that runs long-horizon
software missions the way Factory.ai's Missions/Mission Control does: an
orchestrator plans, fresh-context workers implement one feature at a time,
independent validators judge each milestone, and a human steers as project
manager. The harness never touches the spacecraft; it runs the room.

Built in Rust on top of the `claude` CLI (headless stream-json sessions), so
every session inherits your repo's CLAUDE.md, `.claude/skills`, `.mcp.json`,
and hooks for free. Git is the source of truth; an append-only event log makes
every mission `kill -9`-safe.

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

Prerequisites: Rust 1.85+, git, and the [Claude Code CLI](https://claude.com/claude-code)
(`claude`) installed and authenticated — Kranz discovers it on PATH and in the
usual install locations, or set `KRANZ_CLAUDE_BIN` / `claudeBinary` in config.

```sh
cargo install --path crates/cli   # puts `kranz` on your PATH (~/.cargo/bin)
cd /path/to/your/repo             # must be a git repo

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
run `cd <kranz checkout>/apps/dashboard && npm install && npm run build`;
`kranz serve` will prefer that fresh build automatically (searching
`--dashboard DIR`, `$KRANZ_DASHBOARD_DIST`, `<repo>/apps/dashboard/dist`,
installed asset dirs such as `~/.kranz/dashboard/dist`, then the kranz checkout
used to build the binary, before falling back to the embedded bundle).

Desktop app: `cd apps/dashboard && npm install && npm run build && npx tauri dev`
(reads `KRANZ_REPO`; see `apps/dashboard/README.md`).

## Install

**From source (available today).** With a Rust toolchain on your machine:

```sh
cargo install --path crates/cli   # builds and installs the `kranz` binary
```

**Prebuilt binaries.** Every tagged release (`vX.Y.Z`) ships binaries built by
CI ([`.github/workflows/release.yml`](.github/workflows/release.yml)) for
Linux, macOS, and Windows, attached to the GitHub release as
`kranz-<os>-<arch>`. Download the one for your platform, `chmod +x` it (Unix),
and drop it on your `PATH`.

**Once published (planned).**

```sh
cargo install kranz            # from crates.io

brew tap craigcode/kranz               # Homebrew (from-source formula)
brew install kranz
```

The crates.io and Homebrew paths are not live yet — they wait on the first
public release (see [docs/releasing.md](docs/releasing.md)).

## The four roles

| Role | Job | Lifetime | Sees | Touches | Default |
|---|---|---|---|---|---|
| Orchestrator | Plans contract-first; judges worker reports; converts or waives findings; decides respawns, dirty trees, unblocks; judges `agent-judgement` assertions at the final gate | One long-lived session per mission (resumable/re-seedable) | Digest, plan, reports, findings — never raw transcripts | Nothing — read + git-inspect only; the engine writes/commits `plan.json`/`plan.md` | opus · high |
| Worker | Implements exactly one feature: tests first, implement until green, lint/build, commit `[feature-id]`-prefixed, end with a `WorkerReport` | Fresh per feature (bounded respawns) | Its spec + criteria + goal — never the mission transcript | Edits + Bash in the repo; denied push/publish/network/sudo | sonnet · medium |
| Validator · scrutiny | Adversarial review of the milestone diff: tests asserting implementation, dead criteria, integration seams, out-of-intent regressions | Fresh per validation round | Milestone spec, contract, `start-sha..HEAD` diff | Read-only + inspect commands | opus · high |
| Validator · functional | Actually **runs** the contract's commands + configured test/build/lint scripts; reports pass/fail with verbatim output as evidence | Fresh per validation round | Contract commands + milestone criteria | Read-only + exactly those commands | sonnet · medium |

Two validators because they catch different failures: functional catches "it
doesn't run"; scrutiny catches "it runs but it's wrong" — passing tests that
assert the implementation, unwired criteria, broken seams between features.

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
- **Kranz never pushes.** The mission branch is the deliverable; you review
  and open the PR.

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

## Development

```sh
cargo test --workspace          # 200+ tests, no model calls (mock backend seam)
cargo test -p kranz-engine -- --ignored   # + live smoke test (spawns claude)
node scripts/mock-server.mjs    # dashboard dev harness with a canned mission
```

Born from a v3 build plan cloning Factory.ai's opinionated core; deviations
are documented in [docs/design.md](docs/design.md).

## Cloud (preview)

Kranz is local-first and **never pushes** on your machine. Cloud missions (M6,
preview — not yet exercised end-to-end) run the same binary in a container and
publish the mission branch as a reviewable `kranz/*` ref only — never `main`,
never a merge. See the [`Dockerfile`](Dockerfile) and the
[cloud deploy runbook](docs/deploy.md).
