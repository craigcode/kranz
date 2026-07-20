# Kimi Code CLI backend probe (`f-1-1`, M1 gate)

Notes for a future `backend_kimi.rs` (`AgentBackend` impl alongside the Claude
CLI, Codex, and Cursor backends in `crates/engine/src/backend.rs`). Grounded
in one real minimal session captured locally with `kimi-code 0.27.0`
(authenticated) via `kimi -p "<prompt>" -m kimi-code/k3 --output-format
stream-json` in a throwaway temp git repo (`/tmp/kimi-probe`). The redacted
fixture lives at `crates/engine/tests/fixtures/kimi_exec_scrutiny.jsonl`.

Following the probe discipline of `docs/scoping/cursor-cli-backend.md`: free
binary/auth checks first, relocated-`$HOME` auth survival check, and only one
smallest-possible billed capture after headless auth was proven free.

## 1. Binary discovery (free)

- `KRANZ_KIMI_BIN` was unset in this environment.
- `kimi` is not on `PATH` and not present at any of `~/.local/bin`,
  `/opt/homebrew/bin`, `/usr/local/bin`, `~/.npm-global/bin`.
- Found at the Kimi-specific install dir `~/.kimi-code/bin/kimi` (a
  self-contained Mach-O 64-bit arm64 binary, not an npm shim).
- `kimi --version` → `0.27.0`.

A future `backend_kimi` binary-discovery order should therefore check
`KRANZ_KIMI_BIN`, then `kimi` on `PATH`, then the well-known dirs, then
**`~/.kimi-code/bin/kimi`** specifically (the Moonshot/Kimi-specific
install dir the spec called out — this is where the real install lives on a
machine that has never put it on `PATH`).

## 2. Auth classification (free)

- `kimi login` is documented (`kimi --help`) as "Authenticate with Kimi Code
  CLI via the device-code flow" — an interactive device flow, same shape as
  Cursor's `agent login` and codex's `codex login`.
- `kimi provider list` (free, read-only) shows the OAuth-managed provider
  already authenticated in this environment:
  `managed:kimi-code  type=kimi  models=3  source=oauth`, default model
  `kimi-code/k3`.
- On-disk credential location (names only, no values read or recorded):
  `~/.kimi-code/credentials/kimi-code.json`, containing the keys
  `access_token`, `refresh_token`, `expires_at`, `scope`, `token_type`,
  `expires_in`. Sibling state: `~/.kimi-code/device_id`,
  `~/.kimi-code/oauth/`, `~/.kimi-code/config.toml` (model/provider config,
  no secrets), `~/.kimi-code/session_index.jsonl` and
  `~/.kimi-code/sessions/**` (local per-session transcripts, see §5).
- `kimi doctor` (free) validates `config.toml`/`tui.toml` schema only; it
  does not check auth.

### Relocated-`$HOME` survival (per `docs/scoping/claude-cli-min-env.md`)

```
HOME=/tmp/kimi-scratch-home kimi provider list
```

→ `No providers configured.` (exit 0). **Auth does NOT survive a relocated
`$HOME`** — same failure mode as Cursor
(`docs/scoping/cursor-cli-backend.md` §"Auth state does not survive a
relocated `$HOME`"). The OAuth token cache lives entirely under
`~/.kimi-code`, which is not itself relocatable via a `KIMI_CONFIG_DIR`-style
env var (no such var was found in the binary's embedded strings or
`--help`); a headless `backend_kimi` worker/validator run under a scratch
`$HOME` sandbox must carry the real `~/.kimi-code` directory into whatever
`HOME` the sandboxed process sees (bind-mount or copy), or use the env-var
API-key path below instead.

### Env-var API key path (documented, not live-tested)

The binary's embedded strings confirm a `KIMI_API_KEY` env var exists as a
"documented config-file fallback" for a **custom** provider entry added via
`kimi provider add <url> --api-key <key>` (falls back to
`KIMI_REGISTRY_API_KEY` for the registry fetch itself, and the imported
provider's own auth resolves `provider.env["KIMI_API_KEY"]` at call time).
This is **not** the same as the built-in OAuth-managed `managed:kimi-code`
provider used above — no `MOONSHOT_API_KEY`/`KIMI_API_KEY` env var was set in
this environment, and this probe did not add a custom provider or exercise
this path live (doing so would require a Moonshot console API key, out of
scope for a free/no-new-credential probe). Flagged as an unverified,
inferred-from-strings alternative for whoever unblocks relocated-`$HOME`
headless auth next, not a confirmed working path.

**Conclusion:** headless auth is usable in-place (this environment's real
`$HOME`) and free to verify (`provider list`, `doctor`), so the probe
proceeded to a live capture per step 3 of the spec.

## 3. Captured fixture

Command (throwaway temp git repo `/tmp/kimi-probe`, smallest possible
prompt, no tool use needed):

```
kimi -p "Reply with exactly: OK" -m kimi-code/k3 --output-format stream-json
```

Two free deterministic pre-billing failures were observed first while
mapping the flag surface (kept out of the committed fixture, recorded here
for the event-schema table):

- `kimi -p "hi" -m kimi-code/bogus-model-xyz --output-format stream-json` →
  exit 1, `error: failed to run prompt: config.invalid: Model
  "kimi-code/bogus-model-xyz" is not configured in config.toml. Add a
  [models."kimi-code/bogus-model-xyz"] entry with max_context_size.` —
  deterministic, user-readable, no session created, no billing.
- `kimi -p "hi" -m "k3:low" --output-format stream-json` → same
  `config.invalid` shape. **`-m` does not accept a `model:effort` suffix
  form** — see §4.
- `kimi -p "hi" ... --auto` → exit 1, `error: Cannot combine --prompt with
  --auto.` — `-p`/`--prompt` and `--auto` are mutually exclusive flags;
  free, pre-billing, deterministic.

The committed fixture (`crates/engine/tests/fixtures/kimi_exec_scrutiny.jsonl`)
is the raw, unmodified stdout of the successful run — two lines, no secrets:

```json
{"role":"assistant","content":"OK"}
{"role":"meta","type":"session.resume_hint","session_id":"session_22287a9d-16e8-480f-961c-033655426733","command":"kimi -r session_22287a9d-16e8-480f-961c-033655426733","content":"To resume this session: kimi -r session_22287a9d-16e8-480f-961c-033655426733"}
```

### Event-to-`AgentEvent` mapping

**This is the single most important deviation from every other backend
probed so far**: Kimi's `-p --output-format stream-json` stdout stream is
*much thinner* than Claude/Codex/Cursor's. It is not an envelope of
`system`/`assistant`/`tool_call`/`result` (Cursor/Claude shape) or
`thread.*`/`turn.*`/`item.*` (Codex shape). For a prompt that triggers no
tool use, stdout emits exactly two line shapes and **nothing else** — no
`init`/`system` line, no per-turn/per-step framing, and critically **no
terminal event carrying token usage**:

| kimi stream-json stdout line | shape | maps to `AgentEvent` |
|---|---|---|
| `{"role":"assistant","content":<string>}` | final assistant text; the fixture shows exactly one, but treat as repeatable (narration then final) until a multi-line/tool-using fixture proves otherwise | `Text { text: content, raw }`, and this same text becomes the terminal `Result.text` (see below — there is no separate terminal event to source it from) |
| `{"role":"meta","type":"session.resume_hint","session_id":...,"command":...,"content":...}` | last line of every run; not a result frame — purely a "how to resume this session" hint | `Result { text: <the last assistant "content" seen this run>, is_error: false, usage: None (not on this wire — see below), cost_usd: None (not on this wire — see below), num_turns: Some(1), raw }` |

### Synthesis seam: no stdout `Init`, no stdout terminal `Result`

Unlike Claude/Codex/Cursor, the `-p --output-format stream-json` wire never
emits an `init`/`system` line and never emits a terminal frame that is
itself a `Result`. `backend_kimi` must **synthesize** both `AgentEvent`s
rather than translate them off the wire:

- **`Init`** — synthesize at stream start, before the first line is even
  read. `model` is the configured `-m` value (known up front, from
  `backend_kimi`'s own invocation args). `session_id` is *not* known at
  this point — it only appears later, on the `role:meta
  type:session.resume_hint` line (§3 table, row 2). Either (a) emit `Init`
  with a placeholder/empty `session_id` and backfill/update it in place
  once the resume_hint line arrives, or (b) buffer emission of `Init` until
  the resume_hint line is seen and backfill then — either is acceptable,
  but the session_id cannot be sourced any earlier than the resume_hint
  line.
- **`Result`** — synthesize from the `session.resume_hint` line itself
  (there is no separate result frame to parse): `text` = the last
  assistant `content` seen this run, `is_error: false`, `num_turns:
  Some(1)`, `usage: None` (not on the stdout wire — see §5 for the
  out-of-band usage path).

**End-of-run completion signal:** the session loop's `saw_result` must key
off the `session.resume_hint` line plus process exit — that line, not a
dedicated result frame, *is* the sole end-of-run signal on this wire. If
the process exits without ever emitting a `session.resume_hint` line, no
`Result` can be synthesized from stdout at all; treat that as an error
condition rather than silently completing.

Tool-use/tool-result event shapes (`ToolUse`/`ToolResult` per the spec's
requested table rows) were **not observed** — the smallest read-only prompt
used for this capture triggered no tool call, and per the "smallest
read-only" discipline in the spec, a second billed capture to force a tool
call was deliberately not attempted. `kimi --help`'s active-tools list
(`Read, Write, Edit, Grep, Glob, Bash, ...`, visible in the local session
wire log, see §5) confirms tool-calling capability exists; only the *wire
shape* of a tool-call/tool-result line on the **stdout** stream-json channel
remains unobserved. Route any future encounter with an unrecognized
`"role"` value (e.g. a tool-call role) to `AgentEvent::Other` until a
tool-using fixture is captured.

Model id validation: pass the id straight through as `kimi-code/<alias>`
(e.g. `kimi-code/k3`); an unconfigured model fails free and deterministically
before billing with `config.invalid: Model "<id>" is not configured in
config.toml...` (exit 1, no session created) — surface as a configuration
error, do not retry.

## 4. Model alias (`-m`) and effort level

The spec asked for "the `-m` alias form that selects k3 and each effort
level (low/high/max) plus `kimi-for-coding[-highspeed]`". Live-probing this
found the premise does not hold: **`-m`/`--model` selects the model alias
only; it has no `model:effort` or `model@effort` suffix syntax.**
`kimi -p ... -m "k3:low"` fails with the same `config.invalid: Model
"k3:low" is not configured...` error as an outright bogus model id — `:low`
is parsed as part of the model-alias string, not as an effort selector.

The three configured model aliases (from `~/.kimi-code/config.toml`,
`[models.*]`, no secrets):

| `-m` alias | wire model | `support_efforts` | `default_effort` |
|---|---|---|---|
| `kimi-code/k3` | `k3` | `["low","high","max"]` | `high` (this environment's `config.toml` additionally sets `[thinking] effort = "max"` globally, which overrides the model default) |
| `kimi-code/kimi-for-coding` | `kimi-for-coding` | (not thinking-capable per this config; no `support_efforts` entry) | — |
| `kimi-code/kimi-for-coding-highspeed` | `kimi-for-coding-highspeed` | (same as above) | — |

Effort level is a **separate axis**, controlled by (precedence, per the
binary's embedded resolution logic):

1. env var `KIMI_MODEL_THINKING_EFFORT` (highest precedence; confirmed live —
   setting it to an unsupported value against `k3` produced a `400 Invalid
   request` from the provider, i.e. it *is* forwarded to the API, not
   validated client-side against `support_efforts`),
2. `config.toml` `[thinking] effort = "low"|"high"|"max"`,
3. the model's own `default_effort`.

There is no CLI flag (`--effort` does not exist in `kimi --help`). A future
`backend_kimi` should therefore select effort by setting
`KIMI_MODEL_THINKING_EFFORT` in the child process's environment before
invoking `kimi -p`, not by encoding it into `-m`.

## 5. Usage/cost — metered, but not on the `-p` stdout wire

The fixture's terminal line (`role: meta, type: session.resume_hint`)
carries **no usage or cost field**. This is a real, observed gap versus
Codex/Cursor's `-p`/`exec` output, both of which put a `usage` object
directly on their terminal event.

Reading the **local session log** (free, read-only, no additional billing)
for the same run —
`~/.kimi-code/sessions/wd_kimi-probe_<hash>/session_<id>/agents/main/wire.jsonl`
— shows the run *was* metered: a `usage.record` line carries
`{"inputOther":2086,"output":31,"inputCacheRead":19200,"inputCacheCreation":0}`
against `"model":"kimi-code/k3"`. `~/.kimi-code/session_index.jsonl` maps
each `session_id` (present in the stdout `session.resume_hint` line) to its
`sessionDir` on disk.

**Cost decision: metered, not Meterless.** Per
`crates/engine/src/backend_readiness.rs:1-5,70-82`, `Meterless` is for
providers with no quota/usage API at all (never invent a 0%-quota bar for
those); Kimi is not that case — usage tokens genuinely exist and are
recorded, just not on the `-p` stdout channel. `backend_kimi` must read them
out-of-band: after a run completes, parse the `session_id` from the
`session.resume_hint` line, resolve its `sessionDir` via
`session_index.jsonl`, and read the `usage.record` (or the last
`step.end`'s `usage` field) from that session's `agents/main/wire.jsonl` to
get `{inputOther, output, inputCacheRead, inputCacheCreation}` token counts.
Cost itself is not on the wire in either location — compute it client-side
via a per-model price table keyed on the `kimi-code/<alias>` id (mirroring
`docs/scoping/codex-backend.md`'s `pricing_for_model` approach), mapping
`inputOther → input`, `output → output`, `inputCacheRead → cached`.

## 6. Permission / deny-rule configuration for headless `kimi -p`

Relevant flags (`kimi --help`):

| Flag | Effect | validator (read-only) | worker (read-write) |
|---|---|---|---|
| default (no flag) | interactive approval prompts — **cannot** be used headlessly; blocks waiting for stdin | no (hangs) | no (hangs) |
| `--plan` | plan mode; produces a plan, no writes | yes | no |
| `-y`/`--yolo` | automatically approve all actions | no | yes |
| `--auto` | "auto permission mode" (per the local session wire log's own system-reminder text: "Tool approvals will be handled automatically... Continue normally without pausing for approval prompts") | no (writes proceed unprompted) | yes |

**Confirmed incompatibility:** `--auto` cannot be combined with `-p`/
`--prompt` (`error: Cannot combine --prompt with --auto.`, free, exit 1,
before any session starts). For a headless single-shot `backend_kimi`,
**`--yolo` is therefore the only flag that both (a) avoids interactive
approval prompts and (b) is accepted together with `-p`** — `--auto`'s
"auto permission mode" is for the interactive/session-server surface, not
`-p` prompt mode. A validator role that must guarantee no writes should
instead use `--plan` (no `-y`/`--yolo`), which was not live-tested in
combination with `-p` in this probe but is documented as producing no writes
in `kimi --help`; flagged as inferred/unconfirmed for whoever picks up the
worker/validator role-mapping implementation.

Static deny/allow rules live in `config.toml` under `[permission]`
(confirmed from the binary's embedded TOML (de)serialization code, not
live-tested — no rule was actually configured and probed in this pass):

```toml
[[permission.rules]]
tool = "Bash"
pattern = "rm -rf*"   # or `match = "..."` — both map to the same `tool(pattern)` rule shape
decision = "deny"      # "deny" | "allow" | "ask"
reason = "..."
scope = "..."
```

Top-level `[permission] deny = [...]` / `allow = [...]` / `ask = [...]`
array-of-rule shorthand is also accepted and normalizes into the same
`permission.rules` list at load time. A `backend_kimi` deny-rule
implementation should write (or `--add-dir`/require a pre-provisioned)
`config.toml` `[permission]` section with kranz's no-push/no-publish/
no-main-write invariants encoded as `tool = "Bash"` deny patterns, since
`--sandbox`-style flags were not found in `kimi --help` at all (no sandbox
flag exists on this CLI, unlike Cursor's `--sandbox`) — isolation must come
entirely from `--yolo`/`--plan` role choice plus these config-level deny
rules plus external process/workspace controls (throwaway `--add-dir`
workspaces, scoped credentials), the same posture `cursor-cli-backend.md`
landed on for its own non-boundary `--sandbox` flag.

## Known gaps for whoever picks this up next

- **Three of the five `AgentEvent` kinds are not evidence-backed by
  captured stdout.** Only `Text` (the `role:assistant` line) and the
  terminal `Result` (synthesized from the `role:meta
  type:session.resume_hint` line — see §3 "Synthesis seam") are grounded in
  what this probe actually observed on the wire. `Init`, `ToolUse`, and
  `ToolResult` are **not** evidence-backed: `Init` is a pure synthesis (no
  stdout line corresponds to it at all, per the seam note above), and
  `ToolUse`/`ToolResult` are inferred placeholders — no tool call was
  triggered by this probe's single no-tool-use prompt, so their mapping
  rows in the §3 table remain unconfirmed until a second, tool-using
  read-only capture is taken (planned for the M2 parser feature `f-2-1`).
  Interim rule until that capture exists: route any unrecognized `"role"`
  value straight to `AgentEvent::Other`.
- Tool-call/tool-result wire shape on `-p --output-format stream-json`
  stdout is unobserved (no tool-using capture was made; see §3).
- `--plan` combined with `-p` was not live-tested (inferred read-only from
  `--help` text only).
- The `KIMI_API_KEY` custom-provider env-var auth path (§2) was found in
  the binary's strings but not live-tested — no relocated-`$HOME` capture
  using it was attempted.
- Effort-level enforcement was only probed with an *invalid* value (which
  reached the provider as a live `400` — i.e. `KIMI_MODEL_THINKING_EFFORT`
  is forwarded, not client-validated); the three valid values
  (`low`/`high`/`max`) were not each separately captured against `k3`.
- `-m kimi-code/kimi-for-coding` and `-m kimi-code/kimi-for-coding-highspeed`
  were not live-captured (only present in `config.toml`); the k3 fixture is
  the sole captured model.
