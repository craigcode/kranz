# M7 scoping — Worker sandboxing (containment, not just detection)

Status: M7 complete 2026-08-23; tiers 1–3 shipped. Dedicated worktrees, write
auditing, env hygiene, macOS Seatbelt filesystem enforcement, Linux bubblewrap
filesystem/network isolation, fail-closed preflight behavior, the container
provider, a macOS-capable network boundary (the filtering egress proxy, 3.3a),
and the validator egress-grant flow that consumes its denial signal (3.3b) are
implemented. Windows now ships the stable AppContainer process boundary and a
protected hosted normal-gate/overhead receipt; its operator-controlled Windows
11 completion receipt is captured. The hard per-host container egress boundary
and live Linux overhead proof are also complete.

## Why

Everything kranz does today about worker misbehaviour is **detection and
policy**: scrutiny validators judge the diff, tool patterns gate what the CLI
*agrees* to run, `KRANZ_BASE_SHA` pins what "the change" means. But the
worker's `claude` process runs with the operator's full user privileges. The
holes this leaves are not hypothetical — each has a receipt:

1. **Letter-over-spirit compliance.** The Gas City spike's
   forbidden-to-invent mission CREATED its own source-of-truth file from git
   config and passed (docs/gascity.md lesson 3). The scrutiny floor was the
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
  write allowlist = session worktree + a per-session private scratch root
  (`kranz-worker-home-<session_id>`; NOT the shared `TMPDIR`, which would
  expose every sibling mission's worktrees) + opt-in toolchain caches
  (`extraWrite`: `~/.cargo`, npm cache). The mission dir is not writable;
  its engine-owned metadata (`events.jsonl`, `state.json`, the lock file,
  `control/`, `runs/*.jsonl` transcripts) carries explicit write denies so
  it stays read-only even in checkout mode, where the writable session cwd
  is an ancestor of the mission dir (P1, ticket `sandbox-writable-scope`).
  `enforce: "fs"` is supported.
  `enforce: "fs+net"` is supported via the filtering egress proxy
  (`crates/engine/src/egress_proxy.rs`): live verification showed Seatbelt
  rejects hostname egress rules such as
  `(remote tcp "api.anthropic.com:443")` with `host must be * or localhost`,
  so the SBPL instead restricts outbound TCP to loopback and the per-run
  proxy — the only reachable way out — enforces the per-host allowlist
  (default Anthropic endpoints + configured `egress[]` + operator egress
  grants) at CONNECT time and appends structured denials to
  `runs/egress-denials.jsonl`.
- **Linux**: bubblewrap equivalent for the same filesystem allowlists (the
  mission-metadata write denies become spawn-time `/dev/null` masks over the
  metadata files + each existing transcript, and an empty `--tmpfs` shadow
  over `control/` — bwrap has no per-path write deny to stack over an rw
  bind).
  `enforce: "fs+net"` fails closed with `--unshare-net` because bwrap alone
  cannot express a hostname allowlist; if `bwrap` is missing, kranz refuses
  requested enforcement rather than falling back to unsandboxed execution.
  The egress proxy does NOT cover bwrap in v1: its all-or-nothing netns
  cannot reach a host-side proxy without veth plumbing, so process-provider
  `fs+net` on Linux stays `--unshare-net` with no denial signal.
- **Windows**: the process provider now uses a stable less-privileged
  AppContainer (LPAC) token, path-specific disposable-SID ACL lease, and
  pre-resume Job Object assignment for `fs`/`fs+net`. Opting out of
  `ALL APPLICATION PACKAGES` prevents ambient regular-AppContainer grants from
  widening the path allowlist. An elevated host-preparation script adds only
  Microsoft's non-inheriting drive-root metadata ACEs and reapplies the
  documented null-device descriptor once per boot; the launcher verifies both
  prerequisites read-only and otherwise fails closed. Both postures explicitly
  grant read-only `registryRead` so LPAC tools can create descendants; `fs`
  additionally grants internet-client access, while `fs+net` is hard offline.
  Rust commands pin the already-installed active standard rustup toolchain by
  absolute path, grant that protected root read/execute directly, and prefer
  its real binaries over the rustup proxy, with auto-install disabled; the
  operator's `RUSTUP_HOME` remains read-only. The launcher also pre-creates
  Windows' package-private `AC/Temp` under the redirected, scratch-owned
  `LOCALAPPDATA`, refusing any redirect outside the writable roots.
  The same launcher wraps workers, validators, and engine-run gates, and
  protected CI proves authority/real-checkout denial plus DACL restoration.
  The container provider remains fail-closed even when `docker.exe` is present:
  the shipped mounts assume POSIX guest paths and `/dev/null` authority masks.
  Native `.exe` agent backends are required; batch shims are refused. The open
  normal-gate/overhead/Windows-11 completion bar is recorded in
  [`m7-windows-containment.md`](m7-windows-containment.md).
- **Authority directory views** on Linux/bubblewrap and containers hide
  existing, future, and replaced credentials without creating host placeholders.
  Existing non-secret policy/cache entries remain readable but immutable; the
  source tree and private scratch retain their declared writes. Nested mounts
  cannot reopen global authority or sibling mission metadata. Symlink entries
  are omitted from these private views. On macOS, Seatbelt also pins protected
  ancestor names against renaming.
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
- **Shipped**: Seatbelt profile generation for macOS `enforce: "fs"`
  (`crate::sandbox::generate_profile`), bubblewrap wrapping for Linux
  `enforce: "fs"` / `"fs+net"`, the per-role `sandbox` config surface, the
  enforced `claude` spawn wrap, and the mission preflight probe described
  above (`MissionEngine::preflight` runs each distinct contract `command`
  assertion under the generated worker profile and surfaces a `warn`
  `PreflightIssue` for any that fail under it — advisory only, never
  blocking). Requested enforcement that cannot resolve to an OS sandbox now
  fails closed before launching a worker or validator. macOS `fs+net`
  per-host egress allowlisting shipped as the filtering egress proxy
  (`crate::egress_proxy`, ticket 3.3a): Seatbelt restricts outbound TCP to
  loopback, the proxy enforces the hostname allowlist at CONNECT time, and
  denials surface as structured records (`runs/egress-denials.jsonl` →
  `RunOutcome.denied_egress`). The validator grant flow that consumes the
  signal shipped as 3.3b. Windows production process containment and its hosted
  phase-5 gate receipt are shipped; the operator-controlled Windows 11 receipt
  repeats the production hostile and overhead proof and closes parity (see the
  Windows scoping doc).

### Tier 3 — container workspace provider (the Gas City / fleet stepping stone)

- Workspace-provider option running any selected agent backend inside an M6
  image: mount worktree + mission dir only, container-level network policy.
  Slow path, opt-in, aimed at autonomous dispatch and later cross-machine
  execution (the "heterogeneous fleet" future docs/gascity.md says is one of
  the three that make City integration earn its keep). Provisioning remains a
  separate seam from model/CLI selection.
- **Shipped (v1)**: per-role `sandbox.provider: "container"` (default
  `"process"`) plus optional `sandbox.image`. With `enforce: "off"` nothing
  changes; with `"fs"`/`"fs+net"` the session spawn is wrapped in
  `<runtime> run` (`crates/engine/src/sandbox_container.rs`). Runtime
  detection walks PATH in preference order docker → podman → nerdctl →
  Apple `container`; `provider: "container"` with no runtime on PATH fails
  closed, same contract as tiers 1–2. The release-supported host is Linux.
  macOS and Windows fail closed even when a runtime is found; macOS operators
  use the native Seatbelt `process` provider.
- Write policy: the container root fs is `--read-only`; the writable set is
  exactly the declared mounts — session worktree (rw), mission dir (ro),
  the scratch tmpdir (rw, also `HOME`/`TMPDIR` inside), and each
  `extraWrite` entry (rw). The container analogue of the tier-2 write
  allowlist.
- **Network policy**: `fs` keeps the runtime default bridge/NAT
  (same permissiveness as the tier-2 fs tier). `fs+net` with an EMPTY
  egress list maps to `--network none` — a hard egress boundary on the
  supported Linux path. A 2026-08-21 macOS operator proof remains useful
  evidence, but GitHub-hosted macOS cannot renew it because the runner exposes
  no virtualization for Colima; both macOS and Windows therefore refuse this
  provider in the public support matrix. The tradeoff is honest: `none` also
  blocks the agent's API egress, so `fs+net` suits offline gates/validation
  while API-driven workers use `fs`. `fs+net` with a NON-EMPTY egress list is
  now Docker-only and non-bypassable: the worker joins a unique
  [`--internal` network](https://docs.docker.com/engine/network/), with no
  default route. Its only peer is a trusted relay that Docker also attaches
  to the ordinary bridge. The relay injects a random per-run bearer token and
  forwards CONNECT to the host-side filtering proxy; the worker receives the
  relay URL but never the token. Removing `HTTP(S)_PROXY` therefore removes
  its only usable path instead of reopening NAT. The proxy keeps the existing
  extend-only allowlist and per-run structured denial records; requests that
  do not carry the relay credential get 407 and cannot forge a grant signal.
  Podman, nerdctl, and Apple `container` refuse this non-empty-list posture
  until their network primitives have a separate live proof.
- Boundary lifecycle is owned by the run. The worker container, relay,
  stopped credential loader, internal network, private credential volume, and
  transient staging directory all have unique names. Docker's local copy API
  moves the 0700/0600 files into the loader's private volume without executing
  image code or bind-mounting the host path. The loader and staging directory
  are deleted before worker spawn, and only the relay later mounts the volume,
  read-only, running as the files' exact numeric owner.
  Explicit shutdown verifies their removal. The same bounded removal runs
  from `Drop` after backend failure/cancellation/timeout, including a forced
  remove of the daemon-owned worker whose runtime client may already be
  dead. Docker labels carry the owner PID plus an immutable process-identity
  hash; the next boundary start reaps only mismatched/dead owners, providing
  kill-9 recovery without touching a live sibling run. The pinned relay image
  and live proof are recorded in
  [the M7 container egress receipt](../reviews/m7-container-per-host-egress-live-proof.md).
- Worker image: the default `alpine:3` proves the isolation boundary but
  cannot run an agent. A production worker image needs the agent CLI + Node
  on PATH plus the mission toolchain — the same layering the repo's
  `Dockerfile` comment block spells out ("What this image intentionally
  does NOT bundle"). Point `sandbox.image` at such an image for real runs.

## Sequencing (kranz missions, one feature per brief)

1. Worktree-always for workers + primary-checkout-clean assertion (Tier 1).
2. Out-of-contract write audit + finding class (Tier 1).
3. Seatbelt profile generation + `sandbox` config surface, `enforce: "fs"`
   (Tier 2, macOS).
4. Egress allowlist → `enforce: "fs+net"` (Tier 2, macOS): SHIPPED (ticket
   3.3a) as loopback-only Seatbelt egress + the filtering egress proxy —
   per-host enforcement at CONNECT time with a structured denial signal on
   `RunOutcome`. The egress grant flow that consumes the signal is 3.3b.
5. Linux bwrap parity (Tier 2): implemented as filesystem allowlists plus
   `--unshare-net` for `fs+net`; a live ubuntu runner remains the preferred
   external proof.
6. Container workspace provider (Tier 3) — may defer into a City-driven milestone.

Briefs 1–2 are pure-Rust engine work (gate `--workspace`, per the M5
meta-lesson). Briefs 3–5 need macOS/Linux runners to validate for real.

## Done when

A deliberately hostile brief (instructed to write outside its scope and to
call out to the network) run under a supported `enforce: "fs+net"` backend
leaves **zero writes outside its worktree + private scratch**, its blocked attempts
surface as `out-of-contract-write` / preflight findings; a normal mission's
contract commands (cargo test, npm build) still pass under the sandbox at
under ~10% wall-clock overhead; and the primary checkout never changes branch
during any mission, sequential included. On macOS the `fs+net` boundary is
the loopback-only Seatbelt profile plus the egress proxy (3.3a).

## Open questions

1. **Resolved 2026-07-06** — minimal file/dir set the `claude` CLI needs to
   authenticate and run headless: see
   `docs/scoping/claude-cli-min-env.md` for the full probe (credential
   path, `CLAUDE_CONFIG_DIR` relocation semantics, macOS Keychain-vs-file
   distinction). The entry set is encoded as
   `crates/engine/src/backend_claude.rs::claude_min_config_entries` for the
   scratch-HOME-seeding feature to consume.
2. Seatbelt is deprecated-but-ubiquitous; acceptable dependency? (Fallbacks
   are all heavier.)
3. Do MCP servers spawned inside a session inherit the sandbox? (Should, as
   child processes — verify, don't assume.)
4. Shared toolchain caches vs hermeticity: opt-in writable caches are a real
   escape hatch — is the perf worth the hole for autonomous runs?
5. Does `enforce` ever default on for attended runs, or is the floor-for-
   autonomous / freedom-for-attended split (scrutiny precedent) permanent?

## Design note — policy scaling (2026-07-06)

Enforcement rigor should be a function of two independent inputs, not a
single global setting: **declared blast radius** (touch-set breadth,
diff scale, milestone shape) and **run autonomy** (interactive vs
queued vs headless). The scrutiny floor for autonomous runs and the
sandbox tiers above are points on that surface; the deferred per-ticket
priced consent (pipeline-view.md D-D, `auto-queue-under`) rejoins it
once calibration matures. When tier-2 config lands, prefer expressing
defaults as this function (e.g. a mission whose touch-set spans crates/
gets `enforce: fs` by default; a docs-only touch-set may run lighter)
over hand-set per-mission values.
