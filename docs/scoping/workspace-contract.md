# M6 scoping — Workspace contract and provider seam

Status: **proposed** 2026-07-25 (from Monaco agent-developer-workspaces
review). Accept or defer the D-X decisions via ticket
`workspace-contract-design`, then implement the blocked-by chain in
`.kranz/tickets/`.

Companion: `docs/roadmap.md` M6, `docs/scoping/worker-sandboxing.md` Tier 3,
Monaco post https://www.monaco.com/blog/agent-developer-workspaces
(2026-07-09). Product notes already absorbed the headline ("a worktree is
not a workspace"); this doc turns that into a shippable contract.

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
- `secrets[]` — names the provider must inject; never values
- `disk` — optional prune/retain hints for provider cleanup

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
appear in `report.md` / WorkspacePanel. Secrets never logged.

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
| 5 | `local-container-workspace` | 2 | Ports/Docker isolation for parallel missions |
| 6 | `workspace-provider-pin-at-approval` | 2 | Consent-time pin + artifact surfacing |
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
