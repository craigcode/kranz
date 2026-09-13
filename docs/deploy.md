# Kranz cloud deploy runbook

> **PREVIEW — not yet exercised end-to-end.** The `Dockerfile`, the
> ref-restricted push path, `kranz exec --push`, non-loopback serve mode, and
> separate read authority are shipped and tested. No cloud mission or hosted
> browser lifecycle has yet run start-to-finish. The remaining operator inputs
> and exact acceptance checks are recorded in the
> [M4/M6 readiness packet](reviews/m4-m6-operator-readiness.md).

Locally, **Kranz never pushes** — git is the source of truth but stays on your
machine (plan §4.4). Cloud missions are the one exception: the mission branch
must leave the container so a human can review it. The escape hatch is narrow
by construction — **only `kranz/*` refs, never `main`, never a merge** — and it
is opt-in: nothing pushes unless a caller explicitly invokes
`push_mission_branch`.

---

## 1. Two shapes (from roadmap M6)

Kranz's event-sourced core and the M2.5 HTTP lifecycle make it location-
independent, so the same binary serves both shapes.

### A. Ephemeral (CI-shaped) — one container per mission

The smallest new surface. A container runner (GitHub Actions, a Railway/Fly
cron job, any CI):

1. clones the target repo (the mission carries the repo URL + ref),
2. runs the mission headless: `kranz exec -f mission.md` (M5 headless mode),
3. **pushes the mission branch** (`kranz/mission-<id>`) via the scoped-push
   flow (§3),
4. exits. The container is disposable; the reviewable `kranz/*` branch on the
   remote is the only durable output.

Nothing stays running between missions. Good for "a mission file landed in the
repo, run it and hand me a branch."

### B. Persistent host — `kranz serve` on rented compute

A long-lived `kranz serve` on Railway / Fly / a VPS, with a volume for `.kranz`
so mission state (`events.jsonl`, `state.json`) survives restarts. Drive the
full M2.5 lifecycle — create / plan / approve / start — from the browser (or
Slack, §D) against a remote URL, with no local Kranz install. See
[docs/handoff.md](handoff.md) for the local shape of that lifecycle and
[docs/slack-management.md](slack-management.md) for the Slack surface.

Platform fit (roadmap M6): Railway / Fly / VPS are the right shape. RunPod CPU
pods only (the workload is API-bound, no GPU). AWS Lambda is a non-fit — these
are hours-long stateful processes, not 15-minute stateless invocations.

---

## 2. Required environment

Pass these at `docker run` / in the platform's secret store — **never bake them
into the image** (see the TODO block in the [`Dockerfile`](../Dockerfile)).

| Variable | Shape | Why |
| --- | --- | --- |
| `ANTHROPIC_API_KEY` | secret | Cloud auth for the `claude` CLI. Replaces local OAuth — the container has no browser to log in with. |
| `KRANZ_CLAUDE_BIN` | path | Where the `claude` CLI lives, if not on `PATH`. Kranz shells out to it for every agent turn. |
| `KRANZ_TOKEN` | secret | Stable mutation authority. Required for every API mutation; never give it to a read-only dashboard or agent. |
| `KRANZ_READ_TOKEN` | secret | Stable read-only authority for gated GET/HEAD and WebSocket access. Safe for an observing dashboard; rejected by mutations. |
| `KRANZ_SLACK_BOT_TOKEN` / `KRANZ_SLACK_APP_TOKEN` / `KRANZ_SLACK_CHANNEL` | secrets | Only for a persistent host running `--slack` (§D). The cloud host is its **own Kranz instance and needs its own Slack app** — clone the app and use *that clone's* tokens, never a Mac's (see [docs/slack-management.md](slack-management.md) "Running multiple instances"). |
| `KRANZ_SLACK_INSTANCE` | plain (e.g. `cloud`) | Optional instance label for a `--slack` host: every message the cloud bridge posts gets a leading `[cloud]` tag so it is distinguishable from your other Kranz instances. |
| a deploy key scoped to `kranz/*` | SSH key / GitHub App | Lets the container push the mission branch and **nothing else**. See §3 and §5. |

The `claude` CLI, Linux bubblewrap, and the mission's target toolchain (cargo /
node / go / …) are **not** in the base image — they are deployment-specific
layers. The checked-in image can host the API, but it cannot execute a normal
Claude mission safely by itself. The Railway proof image must add the agent
CLI, the selected repository's toolchain, and the containment primitive, then
prove that exact image digest before it is treated as a mission runner.

---

## 3. The scoped-push flow

The **only** push path in Kranz is `GitRepo::push_mission_branch(remote,
branch)` in [`crates/engine/src/git_ops.rs`](../crates/engine/src/git_ops.rs).
It is cloud-opt-in: `kranz exec --push <remote>` is the only shipped caller.
The interactive/local mission loop and server merge API still never push.
What the primitive guarantees:

- The branch **must** start with `kranz/`. `main`, `master`, `HEAD`, a bare
  sha, a `src:dst` refspec, a leading-dash flag, or anything with whitespace is
  rejected **before any git process runs** (no network). Mission branches are
  `kranz/mission-<id>`; mission tags live under `kranz/<id>/…`.
- The push is a plain `git push <remote> <branch>` — never `--force`, never a
  refspec, never `main`, never a merge.
- On failure git's stderr is surfaced verbatim, so a bad deploy key or a
  rejected non-fast-forward lands in the mission log with git's own words.

End-to-end review flow:

```
container: kranz exec -f mission.md --push origin  # COMPLETE, then scoped push
   remote: kranz/mission-<id> appears (no PR yet, nothing merged)
    human: reviews the kranz/* branch, opens a PR, merges (or not)
```

The human is always the merge gate. Kranz publishes a branch; it never opens or
merges a PR. Back this up at the credential layer: the deploy key / GitHub App
should itself be scoped so it *can only* push `kranz/*` (the in-code guard is
defence in depth, not the sole line of defence). On GitHub, a repo ruleset
restricting push to the `kranz/*` ref pattern for that key is the belt to the
guard's braces.

---

## 4. Persistent-host sketch (Railway first)

Goal: a browser-driven `kranz serve` on rented compute. Concretely:

1. **Build & push a derived image** (see [`Dockerfile`](../Dockerfile)) with
   the `claude` CLI, bubblewrap, and the target repository's toolchain. Pin the
   resulting digest in the deployment receipt.
2. **Attach a persistent volume at `/work`** and clone or restore the target
   repository there. The Git checkout and `.kranz` runtime state must both
   survive restarts; an empty volume is not a runnable host.
3. **Run the server on the platform-assigned port**, optionally with Slack.
   The generic fixed-port Docker equivalent is:

   ```sh
   docker run \
     -e ANTHROPIC_API_KEY \
     -e KRANZ_TOKEN -e KRANZ_READ_TOKEN \
     -e KRANZ_SLACK_BOT_TOKEN -e KRANZ_SLACK_APP_TOKEN -e KRANZ_SLACK_CHANNEL \
     -e KRANZ_SLACK_INSTANCE=cloud \
     -v kranz-data:/work \
     kranz-image \
     serve --host 0.0.0.0 --insecure-lan --port 4560 --read-auth --slack
   ```

   On Railway, configure the equivalent start command with `--port "$PORT"`
   through the platform's shell/command configuration. `KRANZ_TOKEN` and
   `KRANZ_READ_TOKEN` are read directly by Kranz, avoiding secret values in
   the command line.

   A `--slack` cloud host is a full Kranz instance on the Slack side: give it
   **its own cloned Slack app** (its own tokens, its own slash-command name)
   and a `KRANZ_SLACK_INSTANCE` label — one app per instance is the supported
   topology, because Socket Mode load-balances inbound envelopes across a
   single app's connections. The ephemeral shape (§1.A) is exempt: it runs no
   bridge, so it is outbound-only and posting into a shared channel with a
   run-id tag is safe. The simplest consolidated topology is to make the cloud
   host the **only** Slack-connected instance. Details:
   [docs/slack-management.md](slack-management.md) "Running multiple
   instances".

4. **Use the platform's TLS router.** `kranz serve` supports non-loopback
   binding only with the explicit `--insecure-lan` acknowledgment. It still
   has no TLS, so Railway's HTTPS router must be the only public path to the
   container port. Do not expose the container port through a second raw TCP
   endpoint.
5. **Keep read and mutation authority separate.** Every `POST /api/…` must
   carry `KRANZ_TOKEN` through `x-kranz-token`. Gated GETs and the WS upgrade
   accept either authority, but browser/observer clients should receive only
   `KRANZ_READ_TOKEN` (header or WS `?token=`). `/api/health` remains
   unauthenticated for platform liveness checks. Off-loopback binding already
   arms the read gate; `--read-auth` makes the intended posture explicit and
   preserves it if the deployment is later moved behind a loopback proxy.

For the Slack control surface (thread-centric approve / steer, `/kranz`
commands), an always-on `serve --slack` is the host — see
[docs/slack-management.md](slack-management.md) and
[docs/backlog-and-slack.md](backlog-and-slack.md).

---

## 5. Security notes (read before exposing anything)

Transcripts are source code. Treat the whole surface as sensitive.

- **Use read-only authority for observers.** Off-loopback binds always gate
  reads; `--read-auth` also gates them on loopback. GET/HEAD and WS accept
  either token, but mutations accept only `KRANZ_TOKEN`. Give dashboards and
  observing agents `KRANZ_READ_TOKEN`; reserve mutation authority for the
  operator. `/api/health` stays unauthenticated for liveness checks.
- **Never expose the raw server.** `kranz serve` has no TLS. It must sit
  **behind a reverse proxy that enforces TLS**. Use `--insecure-lan` only to
  acknowledge the platform-internal non-loopback bind, not as permission to
  publish a raw TCP port. A leaked dashboard URL without a token must reveal
  nothing and mutate nothing (aside from `/api/health`) — that is the M6 bar.
- **Scope the push credential.** The deploy key / GitHub App must be able to
  push `kranz/*` and nothing else — no `main`, no force, no merges. The
  in-code guard in `push_mission_branch` backs this up but is not a substitute
  for a correctly scoped credential.
- **Secrets are runtime-only.** `ANTHROPIC_API_KEY`, both serve tokens, Slack
  tokens, and the deploy key are passed at run time via the platform's secret
  store — never committed, never baked into the image, never in `.kranz` on
  the volume. The serve process creates protected runtime token files while it
  is live; sandbox authority-deny paths keep workers from reading them.
- **`.kranz` is confidential.** The volume holds full transcripts. Restrict
  access to it as you would source code and secrets.

---

## References

- [docs/handoff.md](handoff.md) — current shipped state; the local lifecycle
  and the Slack-token gate this runbook builds on.
- [docs/slack-management.md](slack-management.md) — Slack as a full control
  surface; needs an always-on `serve --slack`.
- [docs/roadmap.md](roadmap.md) — M6 "Cloud missions" (the source of the two
  shapes, the scoped-push rule, and the acceptance bar).
- [`Dockerfile`](../Dockerfile) — the container image these flows run in.
- [`crates/engine/src/git_ops.rs`](../crates/engine/src/git_ops.rs) —
  `push_mission_branch` / `has_remote`, the scoped-push primitive.
