# Kranz backend/credential/prompt surface — adversarial review

Target: `craigcode/kranz` @ `33732c27` (read-only worktree). Surface: engine backends,
`agent_env`, `scrub`, `auth_verify`, `config`, `paths`, `prompts`/`planning`/`judgement`/
`decompose`, `routing*`, `cost`, `stream_bounds`, and CLI `config_cmd`/`init`/`ready`/`otel`.

## Summary (5 lines)

1. The child-environment work (`agent_env.rs`) is genuinely strong: agent sessions and contract
   commands spawn `env_clear`'d from a locked allowlist, with exactly one named auth var injected.
2. The hole is one layer up: `<repo>/.kranz/config.json` is the **highest-precedence** config layer
   with no provenance check, and it can name the binary kranz executes (`claudeBinary`), the ambient
   secrets copied into contract commands (`contractEnvPassthrough`), an attacker-controlled LLM
   endpoint (`baseUrl`), an ACP program, and a pack of shell gate commands.
3. That repo-named binary is probed with `Command::new(...)` **without** `env_clear` — the one spawn
   path that still carries the operator's entire ambient environment.
4. Two independent credential-hygiene defects: the codex scratch-HOME seed writes `auth.json`
   world-readable into shared `/tmp`, and `kranz config show --global` prints Slack tokens verbatim.
5. Scrubbing is applied at every disk/event choke point I traced (event log, transcripts, reports,
   PR bodies); the OTEL exporter is the one sink that bypasses it.

---

## HIGH — Repo-provided `.kranz/config.json` is the winning config layer and grants code execution

**Severity:** HIGH · **Confidence:** CONFIRMED (full path traced)

**Refs**
- `crates/engine/src/config.rs:286-295` — layer order, project layer appended last
- `crates/engine/src/config.rs:300-331` — `load_layers` + `deep_merge`, no provenance check
- `crates/engine/src/types.rs:1544-1545` — `claude_binary`
- `crates/cli/src/commands.rs:622` — `ClaudeBackend::discover(cfg.claude_binary.as_deref())`
- `crates/cli/src/ready.rs:708,748-749` — `kranz ready` does the same
- `crates/engine/src/backend_claude.rs:176-180` — configured path is candidate **#1**
- `crates/engine/src/backend_probe.rs:14-29` — `Command::new(binary).arg("--version")…spawn()`

**Attacker + preconditions.** Anyone who can get an operator to run `kranz ready`, `kranz plan`,
`kranz exec`, or `kranz backlog draft` with cwd inside a repository they authored. The repo commits
`.kranz/config.json` (git preserves it; kranz's own `.kranz/.gitignore` only stops *kranz* from
creating a tracked one — it does not untrack an attacker's) with
`{"claudeBinary": "./scripts/helper"}` plus a committed executable. Relative paths resolve against
the process cwd, and `config::validate` never inspects `claude_binary`.

**Impact.** Arbitrary code execution as the operator before any sandbox, agent, or approval gate —
and, because the probe does not clear the environment, that process inherits `ANTHROPIC_API_KEY`,
`AWS_*`, `GH_TOKEN`, `SSH_AUTH_SOCK`, and everything else. `ready.rs:3-5` advertises the command as
"deliberately read-only and deterministic".

**Same layer, other primitives** (all `MissionConfig` fields, all repo-settable):
- `contractEnvPassthrough` (`types.rs:1548-1559` → `agent_env.rs:596-624`) copies **named ambient
  vars verbatim** into contract-command envs. Those commands run worker-authored build scripts and
  test binaries. The managed-key refusal blocks `PATH`/`HOME`/toolchain names, never credentials —
  by design, but the repo rather than the operator gets to open the hatch.
- `packDir` (`types.rs`, `pack.rs:389-416` → `orchestrator.rs:6500-6503`) points at a `pack.toml`
  whose gate `command` strings are executed (`run_shell_command_sandboxed`) and whose `prompts`
  are appended to role prompts.
- `worker.baseUrl` / `validatorFunctional.baseUrl` — see the next finding.
- `<role>.acpCommand` / `acpArgs` (`orchestrator.rs:962-979`) — the ACP program and argv.
- `dangerouslyAllowAll`, `validatorAllowUncontainedDegrade`, `denyPatterns`, `workerCandidates`.

**Why this is inconsistent, not merely "don't run untrusted repos".** The codebase gets this right
everywhere else: `.kranz/routing-rules.json` is read **from the base-branch ref** so a mission
cannot re-route itself (`routing_rules.rs:36-58,116-125`); packs carry an explicit
`RepoTracked`/`External` trust class (`pack.rs:382-400`); `.kranz/config.json` is *read*-denied to
every sandboxed session precisely because it "carries Slack tokens and remote-workspace
credentials" (`sandbox.rs:858-886`). The config layer itself is the one repo-owned surface with no
trust distinction. `SECURITY.md`'s "run untrusted repositories in a separate account" covers the
residual, but the asymmetry is worth closing.

**Secondary path (PLAUSIBLE).** `.kranz/config.json` is **not** in the sandbox write-deny set —
`mission_write_denies` covers only `events.jsonl`, `state.json*`, `estimate.json`, `control/`,
`runs/` (`sandbox.rs:791-815`), while `write_allowlist` includes the session cwd. Under
`workerIsolation: "checkout"` (`types.rs:1404-1408`) the session cwd *is* the repo root, so a
prompt-injected worker can plant the file; it is gitignored, so it appears in no diff, no touch set,
and no merge gate, and the next `config::load` picks it up. Default worktree mode does not reach
`repo_root`, which is why this is the secondary rather than the primary path.

**Fix.** Split `MissionConfig` into operator-only keys (`claudeBinary`, `acpCommand`, `packDir`,
`contractEnvPassthrough`, `baseUrl`, `dangerouslyAllowAll`, containment escapes) settable only from
`~/.kranz/config.json`, and repo-settable preferences. Refuse loudly, naming the key and the layer.
Minimum: reject a relative `claudeBinary`, or one resolving inside the repo tree.

**Existing coverage.** `config.rs` has ~30 layering tests; none asserts provenance. `ready.rs:1446`
`backend_lanes_probe_only_backends_some_role_selects` never sets `claudeBinary`.

---

## HIGH — Binary probes spawn with the full ambient environment (the one un-cleared child)

**Severity:** HIGH · **Confidence:** CONFIRMED

**Refs:** `crates/engine/src/backend_probe.rs:18-26`; `crates/engine/src/backend_readiness.rs:461-470`
(`run_bounded`, used by `probe_cli_login`).

```rust
let mut command = Command::new(binary);
command.arg("--version").stdin(Stdio::null())…      // no .env_clear()
```

`agent_env.rs` exists specifically so that "ambient server secrets (Slack tokens, GH_TOKEN, cloud
credentials) [cannot reach] every prompt-injectable child" — and every session spawn honors it
(`backend_claude.rs:898-901`, `backend_codex.rs:585-586`, `backend_kimi.rs:550-551`,
`backend_cursor.rs:1148-1149`, `backend_droid.rs:332-333`, `backend_acp.rs:493-498`, all verified).
The discovery/readiness probes are the exception.

**Attacker + preconditions.** (a) the previous finding's repo-named `claudeBinary`; or (b) any
PATH-precedence shadow of `claude`/`codex`/`droid`/`kimi`/`cursor` (bare names are pushed as
candidates, `backend_claude.rs:188-193`); or (c) on Windows, a repo-root `claude.cmd`/`claude.exe`
if the host's Rust `Command` resolution still consults the working directory — I did not verify the
toolchain's Windows search order, so (c) is PLAUSIBLE only.

**Impact.** A shadowing or repo-named binary receives every operator credential in `envp` on its
first `--version` invocation, before any auth decision.

**Fix.** Probe with `agent_env::sanitized_child_env` over a throwaway home. `--version` needs `PATH`
and nothing else.

**Related, no finding:** binary discovery is **not pinned** after `kranz ready` — `ready.rs` reports
booleans only, and every spawn re-runs `discover_*`. So a probe that passed and the binary that
later runs are two separate resolutions (TOCTOU), though both would have to be attacker-controlled
for it to matter beyond the above.

---

## HIGH — Codex scratch-HOME seed writes the operator's `auth.json` world-readable in shared `/tmp`

**Severity:** HIGH on Linux (N/A on macOS) · **Confidence:** CONFIRMED

**Refs:** `crates/engine/src/backend_codex.rs:50` (`CODEX_SEED_ENTRIES = ["auth.json","config.toml"]`),
`:113-126`; destination from `backend_claude.rs:297-299`.

```rust
let mut source = std::fs::File::open(&src)?;
let mut target = std::fs::OpenOptions::new().write(true).create_new(true).open(&dst)?;
std::io::copy(&mut source, &mut target)?;
```

No `.mode()`, so the destination is created `0666 & ~umask` (typically **0644**), discarding the
source's `0600`. `~/.codex/auth.json` holds the Codex CLI's OAuth tokens / API key. Destination is
`std::env::temp_dir()/kranz-worker-home-<uuid>/home/.codex/auth.json`, with every parent made by
`create_dir_all` (0755). The UUID buys nothing — `/tmp` is listable.

Every other seeding site uses `std::fs::copy`, which *does* carry the source mode over
(`backend_claude.rs:341`, `backend_kimi.rs:142`, `backend_cursor.rs:605`). Codex is the lone
outlier; the code comment explains the `OpenOptions` choice on Windows-ACL grounds and does not
notice the Unix mode consequence.

**Attacker + preconditions.** Any other local account on a host with a shared `/tmp` (Linux, CI
runners), a codex-backed role, and a spec with no relocated `HOME` (`codex_child_env`
`backend_codex.rs:61` — the orchestrator/validator shape). On macOS `temp_dir()` is the per-user
`0700` `/var/folders/.../T`, which contains it.

**Fix.** `.mode(0o600)` on the `OpenOptions` plus a `set_permissions` after (mode only applies at
creation), mirroring `backend_cursor.rs:325-338`. Consider `0700` on the scratch root.

**Existing coverage.** `backend_codex.rs:1071` asserts seeded *contents*; no test anywhere asserts a
mode on any seeded credential.

---

## MEDIUM-HIGH — ACP permission seam fails OPEN when the peer omits the tool-call subject

**Severity:** MEDIUM-HIGH · **Confidence:** CONFIRMED (logic traced)

**Refs:** `crates/engine/src/backend_acp.rs:298-322` (`wildcard_match`), `:325-338`
(`pattern_matches`), `:341-360` (`decide_permission`), `:1048-1056`.

`tool_call_subject` (`:272-296`) falls back to `title`, which ACP v1 leaves optional. With
`subject == ""`, `wildcard_match("git push*", "")` takes `rest.find("git push") → None → false`, so
`pattern_matches` is false, no deny fires, and `decide_permission` returns **`Allow`**.

Every glob-carrying entry of the built-in worker deny list is bypassed this way
(`permissions.rs:42-56`: `Bash(git push*)`, `Bash(git remote add*)`, `Bash(npm publish*)`,
`Bash(cargo publish*)`, `Bash(sudo*)`, `Bash(curl*)`, `Bash(wget*)`). Only bare-name entries
(`WebFetch`, `WebSearch`) survive, since a pattern without `(` gets glob `"*"` and
`wildcard_match("*","")` is true. Same shape for the read-only posture: `kind` defaults to `"other"`
(`:1048-1050`) and `MUTATING_KINDS` (`:151`) does not contain `"other"`.

**Attacker + preconditions.** The ACP peer (the least-trusted backend by construction — and
`BackendKind::Acp::supports_sandbox_enforcement()` is `false`, so no OS sandbox sits behind it), or
repo content steering it. Requires `worker.backend = "acp"`. The peer sends
`session/request_permission` with no `rawInput.command`, no `locations[0].path`, no `title` — which
a *well-behaved* peer can do accidentally.

**Fix.** Fail closed: when a pattern's tool name maps to the call's kind but the subject is empty
(or the kind is absent/`"other"` in a read-only session), deny naming the missing field.

**Existing coverage.** `crates/engine/tests/backend_acp_test.rs:294,355` and the unit tests at
`backend_acp.rs:1375-1411` always supply a non-empty subject. The empty case is untested.

---

## MEDIUM — `kranz config show --global` prints Slack bot/app tokens verbatim

**Severity:** MEDIUM · **Confidence:** CONFIRMED

**Refs:** `crates/cli/src/config_cmd.rs:136-143`, `:240-250` (`render_layer_file`).

```rust
let v: Value = serde_json::from_str(&text)?;
Ok((format!("{}\n", serde_json::to_string_pretty(&v)?), true))   // raw tree, no redaction
```

`~/.kranz/config.json` is the documented home for real Slack credentials (`crates/slack/src/config.rs`
`load_file_config` reads `slack.botToken` / `slack.appToken` from it; `init.rs:641,657` asserts a
`slack.botToken` survives rewrites; `sandbox.rs:858` calls the file out as carrying Slack tokens).
Note the asymmetry: bare `kranz config show` renders through `MissionConfig`, which drops unknown
keys and is safe — only the single-layer `--global`/`--project` path leaks.

**Attacker + preconditions.** Any context capturing stdout: CI logs, screen share, shell transcript,
or a prompt-injected agent that can run `kranz`. kranz's own scrubber would catch `xoxb-` in a
*transcript*, but a terminal or CI log is not scrubbed.

**Fix.** Run the single-layer render through `kranz_engine::scrub`, or redact a fixed key list with
a `--show-secrets` opt-in.

**Existing coverage.** 27 tests in `config_cmd.rs`; none asserts redaction, and `:783` asserts
unknown keys are *preserved*.

---

## MEDIUM — Local backend `baseUrl` is unrestricted: prompt exfiltration + SSRF/port-probe from the engine

**Severity:** MEDIUM · **Confidence:** CONFIRMED

**Refs:** `crates/engine/src/config.rs:610-640` (validation), `crates/engine/src/backend_local.rs:200-214`,
`crates/engine/src/backend_readiness.rs:329-381` (`probe_local_reachability`).

`baseUrl` validation checks only that the string starts `http://`/`https://` and has a non-empty
host (`https://models.internal/v1` is an accepted test case, `config.rs:1558`). The engine then
POSTs `{base}/v1/chat/completions` **from the engine process, outside any sandbox** — the module doc
says so explicitly (`backend_local.rs:12-15`) — carrying the assembled system + user prompt.

Contrast `hookStatus.endpoint`, which **is** loopback-gated with the reasoning spelled out
("pointing it at a remote host would leak signal authority off-machine", `config.rs:~527-545`). The
local backend has no equivalent check.

**Attacker + preconditions.** Repo `.kranz/config.json` sets `worker.backend = "local"` (or
`validatorFunctional`, the only validator role permitted) plus `baseUrl` at an attacker host.
Consequences: (a) mission goal / role prompt / task text POSTed to the attacker; (b) the attacker
returns arbitrary "model output" that becomes the worker's or functional validator's result —
though KRZ-206b's confirm-on-pass and the scrutiny-role rejection (`config.rs:596-607`) bound the
verdict-forging half; (c) `probe_local_reachability` does a `TcpStream::connect_timeout` to any
host:port and records reachable/unreachable plus the OS error into the readiness report — a
config-driven internal-network oracle.

**Positive notes.** `reqwest::Client::new()` — default TLS verification; `danger_accept_invalid_certs`
appears nowhere in the workspace. Response bodies are bounded (`RESPONSE_BODY_CAP` 8 MiB via
`TailWindow`), the round trip has a 600 s timeout, and no auth header is attached.

**Fix.** Require loopback (or an operator-global allowlist) for `baseUrl`, mirroring `hookStatus`;
at minimum record an `orchestrator.decision` naming the endpoint and the config layer it came from.

---

## MEDIUM — OTEL export bypasses the scrubber and has no length cap

**Severity:** MEDIUM · **Confidence:** CONFIRMED for the missing scrub/cap; PLAUSIBLE for the planted-log vector

**Refs:** `crates/cli/src/otel/map.rs:303,405,452`; `crates/cli/src/otel/run.rs:56-59`.

```rust
("kranz.mission.goal".to_string(), AttrValue::String(goal.to_string())),
("kranz.milestone.title".to_string(), AttrValue::String(open.title.clone())),
SpanStatus::Error(reason.clone()),      // MilestoneBlocked reason
```

Nothing in `crates/cli/src/otel/` calls `crate::scrub` (zero hits across all four files), and there
is no `truncate`. Normally harmless, because `EventLog::append_redacting` scrubs at write time
(`event_log.rs:379`). The gap is that the read path does not assume only that writer: `run.rs:58`
enumerates `<repo>/.kranz/missions/` with a bare `read_dir` and no provenance check, so a repo that
commits `.kranz/missions/planted/events.jsonl` gets hand-written goal/title/reason strings into the
operator's observability backend, unscrubbed and unbounded.

**Positive note (this closes the exfil channel that was asked about):** the OTLP endpoint is a
required CLI flag (`cli.rs:579-581`, `commands.rs:534-537`), **not** readable from repo or user
config, so a hostile repo cannot redirect telemetry. The full attribute set is a clean allow-list —
mission/milestone/run ids, statuses, model, cost, tokens — with no env values, paths, usernames,
prompt text, or `prompt_hash`/`transcript_path`.

**Fix.** `scrub::scrub` + `truncate_chars` in `map.rs` before building attributes.

---

## LOW — Scrub rule gaps (recall), and an allowlist that suppresses *findings* as well as redactions

**Severity:** LOW · **Confidence:** CONFIRMED

**Refs:** `crates/engine/src/scrub.rs:123-215` (rules), `:284-323` (`is_allowlisted`), `:357-382`
(`push_finding`).

Covered well: PEM blocks, GCP JSON private keys, `sk-ant-`, `sk-proj-`, `sk-`, `AIza`, Stripe,
`npm_`, `gh[pos]_`, `github_pat_`, `AKIA`, `aws_secret_access_key`, `xox[baprs]-`, JWTs,
`Bearer`/`Basic`, `scheme://user:PASSWORD@host`, plus generic-assignment and entropy passes.

Gaps found:
- GitHub `ghu_` (user-to-server) and `ghr_` (refresh) are not in `gh[pos]_`.
- Slack app-level `xapp-…` and config tokens `xoxe-…` are not in `xox[baprs]-`.
- Slack incoming-webhook URLs (`https://hooks.slack.com/services/T…/B…/…`) are credentials and match
  no rule.
- GitHub's legacy `Authorization: token <hex>` scheme is not covered (only `Bearer`/`Basic`).
- `is_allowlisted` is consulted by `push_finding`, so it suppresses **detections** as well as
  redactions: an `sk-ant-…` or PEM whose matched text contains `example`/`none`/`your`/`todo` as a
  substring produces no `SecretFinding`, which is what `scan_unified_diff` → the merge secret gate
  runs on (`merge.rs:333`). Redaction itself is unaffected (`scrub_impl`'s static rules never
  consult the allowlist), and the probability of a random token containing one of those substrings
  is small — hence LOW, not MEDIUM.

The kranz serve token is `Uuid::new_v4().simple()` (32 hex, no hyphens, `server/lib.rs:53`), so
`is_uuid` does *not* allowlist it and `token: <value>` redacts correctly. Verified.

---

## LOW — Atomic-write temp files hold the token-bearing global config at default permissions

**Severity:** LOW · **Confidence:** CONFIRMED (ordering); PLAUSIBLE (exploitability)

**Refs:** `crates/cli/src/config_cmd.rs:338-351`; `crates/cli/src/init.rs:497-513`.

Both writers `File::create` the temp file (0666 & ~umask → usually 0644), write the **complete**
content — including `slack.botToken`, since unknown keys are preserved — `sync_data`, and only then
copy the target's mode. Additionally `init.rs:508-510` falls back to `0o600` only when the target
does not exist, so an existing world-readable `~/.kranz/config.json` is rewritten world-readable.

**Fix.** `OpenOptions::new().mode(0o600)` before writing; tighten rather than merely preserve.
Existing coverage (`config_cmd.rs:966`) asserts the *final* mode only.

---

## LOW — ACP session accumulates peer text without bound

**Severity:** LOW (availability) · **Confidence:** CONFIRMED

**Refs:** `crates/engine/src/backend_acp.rs:922-931`, `:954-961`, `:980`.

`BoundedLines`/`STDOUT_LINE_CAP` caps each protocol *line* at 8 MiB, but `message_text` is cleared
only when `chunk_id` changes; a peer that streams `agent_message_chunk` with no (or a constant)
`messageId` grows it for the whole turn, and `tool_calls: HashMap` gains an entry per distinct
`toolCallId` with no eviction. Codex (`backend_codex.rs:454-457`) and kimi
(`backend_kimi.rs:456-459`) *replace* rather than append, so this is ACP-specific — the class
`stream_bounds` was written to close, left open on the newest backend.

---

## LOW — Scratch HOMEs live in shared `/tmp` under 0755 directories

**Severity:** LOW · **Confidence:** CONFIRMED

`scratch_home_root` = `std::env::temp_dir()/kranz-worker-home-<session-uuid>`
(`backend_claude.rs:297-299`), created with `create_dir_all` (0755). Seeded credentials are 0600
(claude `.credentials.json` via `fs::copy`, kimi, cursor's explicit `.mode(0o600)`), so this is
directory-listing exposure only — **except** for the codex `auth.json` above, which is what makes
that finding exploitable. Session ids are v4 UUIDs (`runner.rs:1044,1410,1699`), so the classic
`/tmp` symlink pre-creation attack needs a UUID guess and is not practical. On macOS the temp root
is per-user 0700.

Also LOW: the macOS claude path symlinks the operator's real `~/Library/Keychains` into the scratch
HOME (`backend_claude.rs:346-357`) — deliberate, documented, and the same trust class as the
credentials copy, but it means a worker session's HOME reaches the real login keychain.

---

## INFO — Prompt/plan injection posture

- **Interpolation has no delimiting or escaping.** `prompts::render` (`prompts.rs:53-66`) is a plain
  `{key}` substitution (single pass, values never re-scanned — so no recursive expansion), and the
  judgement turns build messages with bare `format!` (`judgement.rs:100-108`, `:171-177`, `:259-266`).
  A worker's report text lands in the orchestrator's judgement prompt. Partial mitigation: the report
  is injected as `serde_json::to_string_pretty` output, so newlines are `\n`-escaped inside JSON
  strings and the attacker cannot emit raw prompt-shaped lines.
- **Verdicts are structured, not regex-on-prose.** `JudgementDecision` / `VerdictsDecision` are serde
  structs parsed via `runner::parse_report`'s strict→lenient ladder (`runner.rs:538-563`), the
  standards checker validates cardinality strictly (missing *or* duplicate id fails the gate,
  `judgement.rs:234-283`), and every unparseable path defaults conservatively (respawn / fail /
  "all rules failed closed"). No free-text "VERDICT: PASS" is honored anywhere I traced.
- **Roles cannot forge each other's reports.** `runner.rs:438-445` parses `WorkerReport` only for
  `Role::Worker` and `ValidatorReport` only for the validator roles. Note that a worker's
  `RunResult` does come from its own self-reported `report.result` (`runner.rs:452-456`) — by design,
  with `judge_worker_run` and the validators as the checks.
- **Repo-native Claude Code config is inherited on purpose.** `backend.rs:8-11` and `docs/design.md:7`
  state that `CLAUDE.md`, `.claude/skills`, `.mcp.json` and hooks are inherited "for free" because
  cwd is the repo root — for orchestrator and validator sessions too. Whether a hostile `.mcp.json`
  actually launches under `claude -p` with a fresh scratch HOME depends on the CLI's own trust
  handling, which I did not probe: **PLAUSIBLE, unverified.** Worth a live probe.

## INFO — `auth_verify.rs` is not what its name suggests

It verifies whether a candidate *env* lets the `claude` CLI authenticate, by driving a trivial
session and classifying the outcome (`auth_verify.rs:82-155`). No token is compared, logged, placed
in a URL, or written. Nothing to report. The server's actual token check *is* constant-time
(`server/lib.rs:~915` `token_matches`, documented against remote timing probes) — though read
requests may carry `?token=` in the query string (`server/lib.rs:895-901`), which lands in access
logs; that is server surface, outside this review.

---

## Areas checked with no finding

- **`agent_env.rs`** — the allowlist is real and enforced, not documentation. `sanitized_child_env`
  builds from empty: `PATH`, scratch `HOME`, `TMPDIR`, locale vars, non-credential toolchain
  locations, and a fresh cache-only `CARGO_HOME` that never carries `credentials.toml`/`config.toml`
  (`:159-160`, `:268-291`). `contractEnvPassthrough` refuses managed keys case-insensitively
  (`:596-612`). Only variable *names* are ever logged (`:554-563`). Test
  `sanitized_child_env_starts_empty_and_never_inherits_secrets` (`:713-778`) poisons `GH_TOKEN`,
  `SLACK_BOT_TOKEN`, `AWS_SECRET_ACCESS_KEY` and asserts an exact key set.
- **Per-backend auth injection** — exactly one ambient var each, all after `env_clear`: claude
  `ANTHROPIC_API_KEY`, codex `OPENAI_API_KEY`, cursor `CURSOR_API_KEY`, kimi `KIMI_API_KEY`, droid
  `FACTORY_API_KEY`, ACP none. No backend inherits the parent env; none adds a second ambient var;
  ACP config cannot name env vars for the child.
- **Secret logging in the backends** — all `tracing::` sites reviewed; none carries an env value,
  token, or auth header. `backend_cursor.rs:453-456,486-489` explicitly replaces the keychain
  passphrase with `<redacted>` before logging, and never puts it in argv.
- **Scrub coverage at disk/event sinks** — `event_log.rs:379` scrubs every appended event;
  `runner.rs:124` scrubs each transcript line; `runner.rs:435` scrubs `final_text` *before* report
  parsing (so report string fields cannot smuggle a secret into `worker.completed`); `pr_handoff.rs`
  scrubs title, body, report, plan, and assertions; `decompose.rs:202,210` scrubs planner output;
  `hook_gates.rs:576-583`, `ticket_notes.rs:57`, `merge.rs:508`, `workspace_*` all scrub. The OTEL
  exporter is the only sink I found that does not.
- **`stream_bounds.rs`** — `TailWindow` is amortized O(1) with a ≤2×cap memory bound and an explicit
  truncation marker; stdout is 8 MiB/line, stderr 64 KiB total. Correct, and used by every CLI
  backend and the local backend's body reader.
- **`routing.rs` / `routing_rules.rs`** — routing rules load from the **base-branch ref**, so a
  mission cannot re-route itself; a mission-branch divergence is surfaced as a decision. Tiers are
  capability classes, never model ids. `pattern_matches` is a total two-pointer glob (no regex, no
  ReDoS).
- **`cost.rs`** — every emitted/serialized type is numeric and `Copy`; `counts_plan` blanks text
  fields; `estimate.json` carries numbers only. No prompt or response text, no credentials.
- **`prompts.rs`** — static `include_str!` templates, SHA-256-hashed and recorded per session; the
  renderer does not recurse into substituted values.
- **`backend_probe.rs` bounds** — tail-bounded drains (64 KiB), a 3 s deadline, process-group /
  Job-object kill. Only the environment (above) is wrong.
- **`kranz init` writes** — `.kranz/merge-gates.json`, `.kranz/tickets/.gitkeep`, `.gitignore`, and
  with `--register` the global config. No secret to a tracked path; the gitignore block covers
  `serve.token` and `serve.read.token`; validation runs before any mutation.
- **`kranz config set/unset`** — `check_schema_path` rejects any dotted path absent from the
  `MissionConfig` schema, so `slack.botToken` cannot be written through the CLI, and `MissionConfig`
  has no credential-shaped field.
- **`kranz ready` output** — prints no env values or tokens; `program_available` stats candidates
  rather than exec'ing them; nothing is written to disk. Its gitignore-hygiene dimension checks that
  `.kranz/config.json` is *ignored* — note this does not detect an attacker-shipped tracked one.
- **TLS** — `danger_accept_invalid_certs` appears nowhere in the workspace.
- **Validator isolation** — validators run against a throwaway snapshot with a mandatory sandbox
  wrap and a post-session tamper tripwire (`orchestrator.rs:5920-6000`); a snapshot failure blocks
  the round rather than falling back to the real checkout. No finding, and worth preserving.
