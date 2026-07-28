# M6 scoping — Workspace contract and provider seam

Status: **accepted** 2026-07-25 — D-A…D-H accepted as proposed, with two
implementation notes recorded below and one delivery re-sequence
(`workspace-provider-pin-at-approval` ahead of `local-container-workspace`).
Implement the blocked-by chain in `.kranz/tickets/`.

Implementation notes (recorded with acceptance):
1. **Egress scoping.** Bootstrap/readiness is local-worktree-first. Tier-3
   fails closed on fs+net missions with non-empty egress, and bootstrap
   needs registry egress — so `local-container-workspace` waits for the
   egress proxy OR ships scoped to fs-tier (bridge network) with the
   limitation documented in its ticket.
2. **Mount/cache-dir convention.** The schema must cover service/bootstrap
   writes outside the worktree (package caches, DB dirs) via `extra_write`
   or named volumes — without it every container bootstrap fails on first
   fetch. Design into `workspace-contract-schema`, not later.
3. **Local-container network model (shipped).** `local-container-workspace`
   implements D-B implementation #2 scoped to the fs-tier bridge (the
   runtime's default NAT): registry egress works for bootstrap, and the
   tier-3 `--network none` egress boundary stays in the sandbox layer (the
   APIs stay separate). The isolation unit is one compose project per
   mission (`kranz-ws-<sanitized-mission-id>`, never shared); `dynamic`
   ports publish host port 0 (OS-assigned) and are read back from the
   runtime for previews (substituted only when actually assigned, never
   fabricated); `fixed: N` refuses at provision when N is already bound on
   the host (naming service and port, never silently rebinding). Contract
   bootstrap/readiness run INSIDE the container network via
   `compose exec -T workspace`. All containers run the shared default image
   in v1 — per-service images are a later additive contract field.
4. **Remote adapter shape (shipped).** `workspace-remote-coder-provider`
   implements D-B implementation #3 as a thin adapter over an injectable
   `SubstrateClient` (create/status/delete/stop — the minimal Coder-shaped
   surface; no VM scheduling inside kranz). `workspace.provider: "remote"`
   resolves ONLY with complete `workspace.remote.{baseUrl,template,
   tokenEnv}` config — a missing key fails closed at approval AND run start
   with the key named; the token comes from the env var NAMED by `tokenEnv`
   (read lazily at provision, never a value in config/logs/events). The pin
   records the configured template id + adapter version (`coder-v1`).
   Contract `secrets[]` cross as NAMES only (recorded as injected names);
   per D-A, **OIDC workload identity with mission-scoped claims remains the
   preferred direction** where the substrate supports it (recorded, not
   built — ampcode.md §3). Substrate URLs name-match onto `previews[]` and
   ride `workspace.provisioned` with the substrate's auth-fronting report
   (never disabled — ampcode.md §8); the takeover URL surfaces in the
   workspace endpoint. Substrate failures block with owner `provider`
   (distinct from repo-setup/operator, not gate-auto-lifted). v1 honesty:
   readiness is substrate-reported only (contract commands do not execute
   remotely — no exec channel yet), agent sessions still run in the local
   worktree, and there is NO public-IP requirement (VPN/SSH reachability is
   an operator/network concern, noted on the config keys).

Companion: `docs/roadmap.md` M6, `docs/scoping/worker-sandboxing.md` Tier 3,
Monaco post https://www.monaco.com/blog/agent-developer-workspaces
(2026-07-09), Amp orbs cross-check `docs/reviews/ampcode.md` §3/§8
(2026-07-27 — an independent shipping product converged on this contract
shape nearly field-for-field). Product notes already absorbed the headline
("a worktree is not a workspace"); this doc turns that into a shippable
contract.

## Why

Kranz isolates **source** well (dedicated git worktrees, primary checkout
byte-untouched, parallel feature worktrees). That does not isolate
**runtime**: ports, Docker daemon state, dependency bootstrap, databases,
preview URLs, or human takeover of the same environment.

Monaco's local "Million Interns" phase failed company-wide on exactly those
gaps (tmux UX, port clashes, Docker contention, flimsy worktree setup, disk
pruning, awkward HITL). Their recovery (Monacoder) bought a substrate
(Coder), baked an AMI, ran docker-compose per VM, cloned a golden Postgres,
and wired Linear/GitHub triggers — not a better agent orchestrator.

Kranz should not build Monacoder. It should own a **workspace contract +
provider seam** so local worktrees, local containers, and remote/Coder-style
hosts can all satisfy the same readiness/preview/audit story while the
engine keeps plan, consent, validation, and delivery.

## Non-goals

- Building a VM scheduler, cloud IDE, or tmux session manager.
- Replacing `AgentBackend` with a workspace product.
- Devcontainer-in-Docker as the v1 remote path (Monaco rejected DinD/DooD
  cost; prefer baked images / compose on a single VM or container host).
- Skill-only "watch CI / address PR comments" loops without mission events.
- Weakening plan approval, empty-deliverable honesty, or the local
  never-push invariant.
- Machine-user credential sprawl; secret **values** never belong in the
  tracked contract or event log.

## Concepts (keep sharp)

| Plane | Owns | Does not own |
|-------|------|--------------|
| **Policy** (kranz today) | plan, touchSet, grants, events, validators, merge gates, spend consent | ports, DBs, preview hosts |
| **Workspace** (this doc) | bootstrap, services, readiness, ports, data clone/reset, previews, takeover links, provider/template identity | model selection, plan content |
| **Sandbox** (M7) | process blast radius (fs/net) for agent/validator CLIs | application service topology |
| **Substrate** (buy) | Coder / container runtime / DB clone operator | mission semantics |

A container may implement both Sandbox (Tier 3) and Workspace (runnable
app). The **APIs stay separate**: `sandbox.provider` is containment;
`workspace.provider` is the runnable environment.

## Proposed decisions

### D-A — Tracked workspace contract on the base branch

Add a base-branch-owned tracked artifact (proposed path
`.kranz/workspace.json`, schema versioned) describing:

- `bootstrap[]` — ordered setup commands (cwd relative to workspace root)
- `services[]` — name, start command or compose service, health check, port
  binding policy (`dynamic` | fixed with collision refusal)
- `readiness[]` — checks that must pass before the first worker turn
- `data` — optional `{ clone, migrate, reset, skewCheck }` hooks (commands
  or provider ops); secret **names** only
- `previews[]` — name → URL template or path once ready
- `secrets[]` — names the provider must inject; never values. For remote
  providers that support it, prefer **workload identity** over injection:
  short-lived OIDC tokens minted per workspace with mission-scoped claims
  (repo, mission id, profile), so services trust the issuer and no secret
  exists to leak — "names, never values" completed as "prefer no secret at
  all" (Amp orb precedent, docs/reviews/ampcode.md §3)
- `disk` — optional prune/retain hints for provider cleanup

Command hooks (`bootstrap[]`, `readiness[]`, `data`) carry bounded timeouts
(explicit or schema-defaulted) so a hung hook fails loud at preflight
instead of stalling provision; a future resume hook (idle-hibernate)
inherits the same rule (docs/reviews/ampcode.md §8).

Missing contract ⇒ local worktree-only missions keep working (today's
behavior). Present-but-invalid contract fails closed at draft/approve/
preflight with a clear owner (repo setup).

Mission branches must not weaken the base-branch contract that judges them
(same spirit as merge-gates).

### D-B — WorkspaceProvider seam ≠ AgentBackend

```
trait WorkspaceProvider {
  provision(spec) -> WorkspaceHandle   // cwd, env, previews, readiness
  readiness(handle) -> Ready | Failed
  teardown(handle, mode)               // keep | hibernate | destroy
}
```

Implementations, in order:

1. **local-worktree** — today's isolation cwd; runs bootstrap/readiness
   against the host; no service network isolation claim
2. **local-container** — compose/network/ports per mission (or per feature
   when parallel); shares runtime tech with M7 Tier-3 but exposes workspace
   semantics
3. **remote** — Coder/vendor adapter; thin; VPC/VPN/AMI concerns stay
   outside the engine

Pin effective `provider` + `template`/`image` + version at **plan approval**
(see ticket `workspace-provider-pin-at-approval`). Additive events record
readiness, preview URLs, takeover instructions, hibernate/destroy.

### D-C — Bootstrap and readiness before the first agent turn

When a contract exists, `run()` / preflight:

1. Provision workspace
2. Run bootstrap
3. Run readiness (incl. optional data skew check)
4. Only then start workers/validators in that cwd

Failures map to preflight or Blocked with owner
`operator | provider | repo-setup`. Do not start spend on a half-ready app.

### D-D — Golden data is validation infrastructure

For DB-backed repos, a de-identified golden snapshot plus clone/migrate/reset
hooks is a first-class workspace input. Migration/version skew (Monaco's
main recurring pain) must surface as Blocked with a migrate/reset path —
not as flake in functional validators.

Repos without a `data` block are unaffected.

### D-E — Human takeover and previews are mission artifacts

Preview URLs, readiness status, provider/template identity, and takeover
instructions (SSH/Coder URL/dashboard link) are append-only event fields and
appear in `report.md` / WorkspacePanel. Secrets never logged. On remote
providers, preview URLs are **authenticated by default** — M6's "leaked URL
reveals nothing" applies to workspace previews, not just the dashboard
(docs/reviews/ampcode.md §8).

### D-F — External triggers create audited missions, not prompt loops

GitHub (CI failure, PR comments, labels), Linear, Slack webhooks may:

- create/draft a ticket, or
- queue an approved plan, or
- open a fix-feature / follow-up mission after consent

They must **not** silently merge, push, or run unbounded skill-only watch
loops. Spend gates and plan approval remain intact.

### D-G — Buy substrate; prefer baked images over nested Devcontainers

First remote provider should assume a replaceable VM/container with
dependencies baked (AMI/image), docker-compose (or equivalent) for app
services, and provider-owned idle hibernation. Nested Devcontainer + DinD
is deferred unless a repo already depends on it.

### D-H — Operator language: isolation ≠ workspace

Dashboard / `report.md` / API must say when only source isolation is active
("worktree source isolation; no workspace contract") versus when services/
data/previews are ready. Avoid implying a runnable environment exists when
it does not.

## Impact-ordered delivery

| Order | Ticket slug | Pri | Impact thesis |
|------:|-------------|----:|---------------|
| 1 | `workspace-contract-design` | 1 | Unblocks honest sequencing; locks D-A…D-H |
| 2 | `workspace-contract-schema` | 1 | Contract without provider still documents intent + validates at approve |
| 3 | `workspace-bootstrap-preflight` | 1 | **Highest local ROI** — fixes flimsy worktrees today |
| 4 | `workspace-provider-seam` | 1 | Trait + local-worktree impl + events; no cloud required |
| 5 | `workspace-provider-pin-at-approval` | 2 | Consent-time pin + artifact surfacing (re-sequenced ahead of local-container at acceptance: pinning the local-worktree provider needs no containers) |
| 6 | `local-container-workspace` | 2 | Ports/Docker isolation for parallel missions (gated on the egress story — see acceptance note 1) |
| 7 | `golden-data-hooks` | 2 | Monaco's efficacy lever for DB repos |
| 8 | `trigger-ci-pr-fix-mission` | 3 | AFK quality loop without surrendering gates |
| 9 | `workspace-remote-coder-provider` | 3 | Thin buy-substrate adapter |
| 10 | `workspace-idle-hibernate` | 3 | Cloud cost control; provider-owned |

`tier3-container-sandbox` remains the **sandbox** containment ticket. Prefer
sharing container runtime code with `local-container-workspace` but do not
merge the product concepts.

## Done when

- A repo with `.kranz/workspace.json` bootstraps and proves readiness before
  workers spend; failures are loud and owned.
- Local-worktree missions without a contract behave as today.
- A container (or remote) provider can be pinned at approval; previews and
  takeover links appear in the event trail and dashboard.
- Optional golden-data hooks clone/reset with skew → Blocked.
- A CI/PR trigger can open an audited fix mission; it cannot bypass approve
  or push.
- Primary checkout remains byte-untouched in worktree mode throughout.

## Open questions

1. Exact on-disk name: `.kranz/workspace.json` vs `workspace.toml` vs split
   files? (Default proposal: single JSON next to merge-gates.)
2. Per-feature workspaces vs one mission workspace with serial service reuse
   when `maxParallelWorkers > 1`?
3. Who owns compose project naming / port allocation on a shared CI host?
4. First remote target: Coder API vs generic "SSH + script" provider?
5. Should `kranz init` scaffold a minimal workspace contract stub?
