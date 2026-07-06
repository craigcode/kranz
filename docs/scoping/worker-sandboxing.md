# M7 scoping — Worker sandboxing (containment, not just detection)

Status: scoped 2026-07-05, unscheduled. Build as kranz missions
(single-feature briefs; see sequencing).

## Why

Everything kranz does today about worker misbehaviour is **detection and
policy**: scrutiny validators judge the diff, tool patterns gate what the CLI
*agrees* to run, `KRANZ_BASE_SHA` pins what "the change" means. But the
worker's `claude` process runs with the operator's full user privileges. The
holes this leaves are not hypothetical — each has a receipt:

1. **Letter-over-spirit compliance.** The Gas City spike's
   forbidden-to-invent mission CREATED its own source-of-truth file from git
   config and passed (docs/gascity.md lesson 5). The scrutiny floor was the
   mitigation; it detects after the fact. Containment would have made the
   sneak impossible, not just catchable.
2. **Accident blast radius.** A worker can write anywhere the operator can:
   other checkouts, `~/.kranz` (its own config!), dotfiles. This session
   history includes multiple wrong-cwd incidents by careful actors; workers
   are less careful.
3. **Prompt injection.** Mission repos contain third-party text (deps, test
   fixtures, vendored docs). Injected instructions currently inherit
   full-user reach — filesystem and network. This matters more as missions
   run on repos the operator didn't author (Gas City beads, OSS work).

Out-of-repo effects are additionally **invisible**: validators diff the repo;
a write to `~/.ssh` or a `curl` to anywhere would never surface as a finding.

Relation to scrutiny: complementary, never a replacement. The sandbox bounds
what *can happen*; validators judge whether what happened *satisfies the
contract*.

## What exists (tier 0 — shipped)

- Scrutiny floor: `skipScrutiny` refused for autonomous runs without
  `--allow-unvalidated`.
- `SessionSpec` policy surface: `permission_mode`, `allowed_tools`,
  `disallowed_tools`, `tools`, `max_budget_usd`.
- `KRANZ_BASE_SHA`-pinned contract diffs; mission branches; process-group /
  Job-Object tree kill; M3 worktree machinery (`GitRepo::add_worktree`).

All of it asks the agent nicely. None of it constrains the process.

## Design — three tiers, each independently shippable

### Tier 1 — containment by construction (cross-platform, no OS deps)

- **Workers and validators always run in a dedicated worktree**, never the
  primary checkout — even sequential single-worker runs (today only M3
  parallel mode isolates). The primary checkout stops changing branches under
  the operator's feet entirely; the run loop merges from the worktree.
  Config: `workerIsolation: "worktree" | "checkout"`, default `worktree`
  after one soak mission.
- **Out-of-contract write audit**: post-run sweep comparing the worktree diff
  against the plan's declared touch-set, plus a cleanliness assertion on the
  primary checkout. New validator finding class `out-of-contract-write`.
  (Still detection — honestly labeled; it closes the *visibility* gap while
  tiers 2/3 close the capability gap.)
- **Env hygiene**: worker env gets a scratch `HOME`/`CLAUDE_CONFIG_DIR`
  carrying only what the CLI needs (open question 1: probe the minimal set).

### Tier 2 — OS-enforced filesystem + network (per-platform, config-gated)

- **macOS**: generate a Seatbelt profile per session (`sandbox-exec`):
  write allowlist = worktree + mission control dir + `TMPDIR` (+ opt-in
  toolchain caches: `~/.cargo`, npm cache); network limited to the Anthropic
  API plus a mission-config egress allowlist (package registries when the
  contract needs builds).
- **Linux**: bubblewrap equivalent (same allowlists; landlock considered if
  bwrap is unavailable).
- **Windows**: explicitly out of scope for the first pass (restricted
  tokens/AppContainer are a different project); documented, not silent.
- Config per role:
  `sandbox: { enforce: "off" | "fs" | "fs+net", extraWrite: [...], egress: [...] }`.
  `SessionSpec` grows a `sandbox` field; `backend_claude` wraps the spawn.
  Default `off` until soaked; the scrutiny-floor precedent applies — consider
  a floor of `fs` for AUTONOMOUS dispatch (`kranz exec`, Gas City beads)
  while attended runs stay free.
- Known risk: sandboxes break toolchains (cargo registry/target dirs, git
  global config, npm). Mitigation: detected-toolchain allowlist defaults +
  a preflight probe that runs the contract's validation commands under the
  profile and reports failures as preflight issues, not mid-run mysteries.
- **Shipped (macOS, `enforce: "fs"`)**: Seatbelt profile generation
  (`crate::sandbox::generate_profile`), the per-role `sandbox` config surface,
  the enforced `claude` spawn wrap (`backend_claude` shells out via
  `sandbox-exec -f <profile>` when a role opts in), and the mission preflight
  probe described above (`MissionEngine::preflight` runs each distinct
  contract `command` assertion under the generated worker profile and
  surfaces a `warn` `PreflightIssue` for any that fail under it — advisory
  only, never blocking). `fs+net` egress allowlisting, Linux bwrap parity, and
  Windows support remain open (see Sequencing below).

### Tier 3 — container backend (the Gas City / fleet stepping stone)

- Backend option running the session's `claude` inside the M6 Dockerfile
  image: mount worktree + mission dir only, container-level network policy.
  Slow path, opt-in, aimed at autonomous dispatch and later cross-machine
  execution (the "heterogeneous fleet" future docs/gascity.md says is one of
  the three that make City integration earn its keep).

## Sequencing (kranz missions, one feature per brief)

1. Worktree-always for workers + primary-checkout-clean assertion (Tier 1).
2. Out-of-contract write audit + finding class (Tier 1).
3. Seatbelt profile generation + `sandbox` config surface, `enforce: "fs"`
   (Tier 2, macOS).
4. Egress allowlist → `enforce: "fs+net"` (Tier 2, macOS).
5. Linux bwrap parity (Tier 2).
6. Container backend (Tier 3) — may defer into a City-driven milestone.

Briefs 1–2 are pure-Rust engine work (gate `--workspace`, per the M5
meta-lesson). Briefs 3–5 need macOS/Linux runners to validate for real.

## Done when

A deliberately hostile brief (instructed to write outside its scope and to
call out to the network) run under `enforce: "fs+net"` leaves **zero writes
outside its worktree + mission dir**, its blocked attempts surface as
`out-of-contract-write` / preflight findings; a normal mission's contract
commands (cargo test, npm build) still pass under the sandbox at under ~10%
wall-clock overhead; and the primary checkout never changes branch during any
mission, sequential included.

## Open questions

1. Minimal file/dir set the `claude` CLI needs to authenticate and run —
   probe before Tier 1 env hygiene lands.
2. Seatbelt is deprecated-but-ubiquitous; acceptable dependency? (Fallbacks
   are all heavier.)
3. Do MCP servers spawned inside a session inherit the sandbox? (Should, as
   child processes — verify, don't assume.)
4. Shared toolchain caches vs hermeticity: opt-in writable caches are a real
   escape hatch — is the perf worth the hole for autonomous runs?
5. Does `enforce` ever default on for attended runs, or is the floor-for-
   autonomous / freedom-for-attended split (scrutiny precedent) permanent?
