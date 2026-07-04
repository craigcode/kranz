# Kranz cloud deploy runbook

> **PREVIEW — not yet exercised end-to-end.** This is the M6 groundwork
> runbook. The `Dockerfile`, the scoped-push primitive
> (`GitRepo::push_mission_branch`), and the flows below are built and unit-
> tested, but no cloud mission has run start-to-finish yet. Treat every command
> here as a sketch to verify, not a guarantee. Track M6 in
> [docs/roadmap.md](roadmap.md).

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
| `KRANZ_SLACK_BOT_TOKEN` / `KRANZ_SLACK_APP_TOKEN` / `KRANZ_SLACK_CHANNEL` | secrets | Only for a persistent host running `--slack` (§D). The cloud host is its **own Kranz instance and needs its own Slack app** — clone the app and use *that clone's* tokens, never a Mac's (see [docs/slack-management.md](slack-management.md) "Running multiple instances"). |
| `KRANZ_SLACK_INSTANCE` | plain (e.g. `cloud`) | Optional instance label for a `--slack` host: every message the cloud bridge posts gets a leading `[cloud]` tag so it is distinguishable from your other Kranz instances. |
| a deploy key scoped to `kranz/*` | SSH key / GitHub App | Lets the container push the mission branch and **nothing else**. See §3 and §5. |

The `claude` CLI and the mission's target toolchain (cargo / node / go / …) are
**not** in the base image — they are deployment-specific layers. See the
Dockerfile TODOs and roadmap M6 "Workspace provisioning" (devcontainer.json
when present, a fat default image otherwise; that part is deliberately
timeboxed and messy).

---

## 3. The scoped-push flow

The **only** push path in Kranz is `GitRepo::push_mission_branch(remote,
branch)` in [`crates/engine/src/git_ops.rs`](../crates/engine/src/git_ops.rs).
It is cloud-opt-in: no mission loop, CLI verb, or server endpoint calls it today
— it exists for M6 wiring. What it guarantees:

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
container: kranz exec -f mission.md         # builds kranz/mission-<id> locally
container: push_mission_branch("origin", "kranz/mission-<id>")   # scoped push
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

## 4. Persistent-host sketch (Railway / Fly / VPS)

Goal: a browser-driven `kranz serve` on rented compute. Concretely:

1. **Build & push the image** (see [`Dockerfile`](../Dockerfile)), with the
   `claude` CLI and any needed toolchain layered on per the Dockerfile TODOs.
2. **Attach a volume** mounted at the mission working tree so `.kranz`
   (`events.jsonl`, `state.json`) persists across restarts. Killing the host
   mid-mission otherwise loses the loop's progress (see roadmap M2.5).
3. **Run the server**, optionally with the Slack bridge:

   ```sh
   docker run \
     -e ANTHROPIC_API_KEY \
     -e KRANZ_SLACK_BOT_TOKEN -e KRANZ_SLACK_APP_TOKEN -e KRANZ_SLACK_CHANNEL \
     -e KRANZ_SLACK_INSTANCE=cloud \
     -v kranz-data:/work \
     kranz-image \
     serve --port 4560 --token "$KRANZ_MUTATION_TOKEN" --slack
   ```

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

4. **Terminate TLS in front of it.** `kranz serve` binds **`127.0.0.1` only**
   (`crates/server/src/lib.rs`) — it never listens on a public interface. So a
   persistent host needs a reverse proxy (the platform's HTTPS router, Caddy,
   nginx) terminating TLS and forwarding to `127.0.0.1:4560`. There is no raw-
   public-bind mode, by design.
5. **Carry the mutation token over TLS.** Every `POST /api/…` must send the
   token in the `x-kranz-token` header (protocol: "Authority: mutation token").
   Pin it with `--token` (as above) so the platform's secret store holds a
   stable value; otherwise `serve` prints a fresh one per boot.

For the Slack control surface (thread-centric approve / steer, `/kranz`
commands), an always-on `serve --slack` is the host — see
[docs/slack-management.md](slack-management.md) and
[docs/backlog-and-slack.md](backlog-and-slack.md).

---

## 5. Security notes (read before exposing anything)

Transcripts are source code. Treat the whole surface as sensitive.

- **Token on reads too.** Locally the token gates only mutations because the
  `127.0.0.1` bind is the real fence. Remotely that fence is gone, so a remote
  deployment must require the token on **reads** as well — transcripts,
  plans, and diffs are all confidential (roadmap M6, "Auth grows up"). Until
  read-auth ships, do not expose a remote server that serves transcripts to
  unauthenticated GETs.
- **Never expose the raw server.** `kranz serve` has no TLS and (today) no
  read-auth. It must sit **behind a reverse proxy that enforces TLS + the
  token**. A leaked dashboard URL without the token must reveal nothing and
  mutate nothing — that is the M6 acceptance bar.
- **Scope the push credential.** The deploy key / GitHub App must be able to
  push `kranz/*` and nothing else — no `main`, no force, no merges. The
  in-code guard in `push_mission_branch` backs this up but is not a substitute
  for a correctly scoped credential.
- **Secrets are runtime-only.** `ANTHROPIC_API_KEY`, Slack tokens, and the
  deploy key are passed at run time via the platform's secret store — never
  committed, never baked into the image, never in `.kranz` on the volume.
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
