# Kranz handoff

> Historical snapshot from 2026-07-03. For current priorities and shipped
> status, use [roadmap.md](roadmap.md) and [roadmap-options.md](roadmap-options.md).

State as of 2026-07-03. Everything below is built, merged to `main`, and
**CI-green on all three jobs — `rust (ubuntu-latest)`, `rust (windows-latest)`,
and `dashboard`** (github.com/craigcode/kranz). 445 workspace tests, `clippy
-D warnings` clean. The Windows Job-Object process-tree kill is now
**runtime-validated on windows-latest**, not just cross-compiled. Two live
acceptance missions completed end-to-end (opus and Fable orchestrators).

## Shipped

- **v1 (plan phases 1–3)** — engine, resumability, CLI + planning TUI,
  dashboard + Tauri, hardened by live missions + adversarial review.
- **M1** — `report.md` at completion, estimate calibration from actuals,
  §5 acceptance proven twice.
- **M2** — `kranz abandon` / `kranz clean` (mission hygiene), environment
  preflight, and mid-mission re-planning (tested safe subset).
- **M2.5** — full mission lifecycle from the web UI (create/plan/approve/
  start) behind a per-serve mutation token.
- **M2.75** — mission backlog (`kranz ticket`/`draft`/`queue`/`work`) +
  Slack Socket Mode bridge (`kranz serve --slack`), incl. `/kranz help`.
- **M2.9 Slice 1** — Slack lifecycle commands (`/kranz new/status/plan/
  approve`) + spend allowlist.
- **M3** — parallel workers, worktree-isolated, flag-gated behind
  `maxParallelWorkers > 1`; the sequential path is byte-identical.
- **M4** — Windows process-tree kill (Job Objects, **CI-validated**), release
  workflow, Homebrew formula, Tauri bundle config, [runbook](releasing.md).
- **M5** — `kranz exec -f mission.md` (headless CI missions) + hardened
  credential `scrub` (entropy heuristic, vendor patterns, allowlist).
- **M6** — scoped `kranz/*` push primitive, Dockerfile, [deploy runbook](deploy.md),
  and `kranz exec --push <remote>` (the cloud handoff).

Deferred by choice (in the roadmap): OTEL export (heavy dep tree), computer-use
QA, skill capture, and Slack control slices 2–3 (App Home, config modals).
Two engine contract-change requests are logged for a fuller M3 (a
`feature.conflict` event; a shared/interior-mutable event log for true
concurrent worker overlap).

## Gates you can close

Two of the original three are now closed: the **GitHub remote** is live
(github.com/craigcode/kranz, CI green on all platforms) and **Slack** is
configured and verified (the bridge posts to `#kranz`). What remains:

### 1. First public release (crates.io / Homebrew)

Prepared but not published. Follow [docs/releasing.md](releasing.md): bump the
version, tag `vX.Y.Z`, push the tag → `release.yml` builds and attaches
per-platform binaries; then update the Homebrew `sha256` and `cargo publish` in
order (engine → server, slack → cli). The `OWNER` placeholders can be replaced
with `craigcode` now that the repo exists.

### 2. Cloud deploy (M6, when you want it)

The pieces exist ([Dockerfile](../Dockerfile), scoped push, `kranz exec --push`,
[deploy runbook](deploy.md)) but no live deploy has run. A Railway/Fly/VPS host
running `kranz serve --slack` with a `.kranz` volume + the mutation token over
TLS is the persistent-host shape; a container-per-mission running `kranz exec -f
… --push` is the ephemeral CI shape.

### (historical) Git remote → CI + Windows validation — DONE

The Windows Job-Object kill is now runtime-validated on windows-latest CI. The
matrix ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs the full
suite on ubuntu + windows on every push. Windows-latest runs the
`#[cfg(windows)]` process-tree-kill regression test — that's the real
validation. Fix whatever windows-latest surfaces (likely none; it's clean on
`x86_64-pc-windows-gnu` locally). Replace the `OWNER` placeholder in
`Cargo.toml`, `packaging/homebrew/kranz.rb`, and README at the same time.

### 2. Slack app + tokens → the bridge (M2.75)

The bridge is built and pure-tested but **inert until configured** (opt-in by
design). To activate:

1. Create a Slack app (api.slack.com/apps) with **Socket Mode** enabled.
   Scopes: `chat:write`, `commands`, `channels:history` (+ `groups:history`
   for private channels). Create a `/kranz` slash command. Install to the
   workspace.
2. Put the tokens in `~/.kranz/config.json` (never the repo):
   ```json
   { "slack": { "botToken": "xoxb-…", "appToken": "xapp-…", "channel": "C0123456789" } }
   ```
   or export `KRANZ_SLACK_BOT_TOKEN` / `KRANZ_SLACK_APP_TOKEN` / `KRANZ_SLACK_CHANNEL`.
3. `kranz serve --slack`. Unconfigured, `--slack` is a harmless no-op with a
   log line.

Then missions post plan-ready / blocked / complete to the channel, one thread
each; approve buttons queue, threaded replies become orchestrator guidance,
`/kranz ticket <title>` scaffolds a ticket. See
[docs/backlog-and-slack.md](backlog-and-slack.md).

## Try the new surfaces now (no gates needed)

```sh
# Backlog pipeline (in any git repo):
kranz ticket new rate-limit --title "Rate-limit the API" --goal "per-token limits"
kranz ticket list                 # NEW
kranz draft rate-limit            # orchestrator drafts a plan → plan.md → REVIEW
kranz ticket approve rate-limit   # → QUEUED
kranz work --once                 # runs it (per-repo serialized)

# Web lifecycle:
kranz serve --open                # + new-mission / plan / approve / start in-browser

# Config (layers: defaults <- ~/.kranz/config.json <- <repo>/.kranz/config.json):
kranz config show                 # effective merge; --global/--project = one layer
kranz config set worker.model opus       # edit one key (validated before writing)
kranz config unset worker.model          # remove one key (siblings preserved)
kranz config role worker opus xhigh      # MID-MISSION: control-inbox config change
```

## Closed finding: piped line-mode readline (not applicable)

`kranz plan`'s line-mode REPL (`cmd_plan` in `crates/cli/src/commands.rs`,
~lines 517-624, reading via `StdinLines`) was long-parked as "no readline in
piped line mode." Investigated and closed as not-applicable, with no
zero-dependency improvement warranted:

- **Both stdin and stdout TTY** — the full-screen `planning_tui` (crossterm
  raw mode) already owns the interaction and provides complete interactive
  editing: arrow keys, history, word-nav, Ctrl-U/A/E. Line-mode never runs in
  this case.
- **Piped stdin** — lines are delivered up-front by the OS/pipe; there is no
  live keystroke stream to attach editing to, so there is fundamentally
  nothing to add. This path must also never discard lines (scripts depend on
  it), so any change here is out of bounds.
- **Real-TTY stdin, piped stdout** — the OS's cooked-mode terminal line
  discipline already supplies basic editing (backspace, Ctrl-U kill-line,
  Ctrl-W word-erase) for free, before the process ever sees the line.

Richer editing beyond cooked-mode defaults would require raw-mode input
handling, which is what the TUI already is — duplicating it in line-mode
would mean re-implementing the TUI without a terminal to draw it in. No
crate-free enhancement clears that bar, so this item is closed with no
functional change.

## Suggested next (historical; superseded)

- **M2** — mission hygiene (`kranz clean`/`abandon`), mid-mission re-planning,
  environment preflight. The natural **first dogfood mission** (additive,
  low-risk) once you want Kranz building itself.
- **M3** — parallel workers (the marquee deferred capability).
- **M6** — cloud missions; M2.5's HTTP lifecycle is already its control plane.
