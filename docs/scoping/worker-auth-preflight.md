# Worker auth preflight: obtaining the a9 live-auth proof

Contract assertion a9 requires proof that a worker can actually authenticate
to Claude under the HOME/`CLAUDE_CONFIG_DIR` hygiene relocation (see
[`claude-cli-min-env.md`](claude-cli-min-env.md) and
[`worker-sandboxing.md`](worker-sandboxing.md)) and go on to do real work. This
proof cannot be produced inside a worker session or a unit test — the workers
themselves are what's under test, and the mock backend used in `cargo test`
never talks to a real `claude` CLI or Keychain. It must be produced
out-of-band by an operator with access to a real, logged-in macOS host.

## How to obtain the proof

1. On a macOS host where `claude` is already authenticated via Keychain
   (`claude /login` completed, `claude doctor` reports logged in), run a real
   Kranz mission end-to-end — i.e. `kranz mission run` (or the operator's
   normal mission entry point) against a throwaway repo/branch, with at least
   one feature worker spawned normally (not a dry run, not the mock backend).
2. Let the mission proceed through at least one worker spawn and completion.
3. Inspect the mission's tracing output for the `seed_worker_env` decision
   record (see below) and the worker's `WorkerReport`.

## Success signals vs the m-66aff8 failure signature

| Signal | Success (hygiene actually worked) | m-66aff8 failure signature |
|---|---|---|
| Worker commits | One or more real commits land on the feature branch | None — empty diff |
| Git diff | Non-empty | Empty |
| Reported cost | Non-zero (`$X.XX` in the WorkerReport / billing) | `$0` |
| Tools used | Worker's transcript shows tool calls (Read/Edit/Bash/etc.) | Zero tools invoked |
| CLI behavior | Worker proceeds normally | CLI immediately reports `Not logged in` and exits |
| WorkerReport | Populated `result`, `summary`, `testEvidence`, `commits` | Missing or a stub report with no real content |

If you observe the m-66aff8 signature (`Not logged in`, `$0`, zero tools,
empty diff), the worker was launched unauthenticated — the exact failure this
mission's hygiene work exists to prevent.

## Reading the loud decision record

Every worker spec build logs a `tracing::info!` record from `seed_worker_env`
(`crates/engine/src/runner.rs`) with `session_id`, `decision`, and
`auth_verdict` fields, and never logs secret/credential values:

- `decision = "relocated"` — the auth preflight confirmed the worker
  authenticates under the scratch `HOME`/`CLAUDE_CONFIG_DIR`, and the worker
  was launched relocated into that verified, confinement-checked scratch env
  (see the confinement test in `crates/engine/tests/backend_claude_test.rs`,
  which asserts only the `claude_min_config_entries()` allowlist is copied in
  and no arbitrary operator dotfiles leak into the scratch dir).
- `decision = "inherited"` — relocation was **not** engaged; the worker
  inherited the real operator `HOME` instead, with an accompanying `reason`
  field (`"auth preflight did not confirm authentication in the scratch env"`
  or `"scratch HOME seeding failed after a successful auth preflight"`).

Grep the mission's tracing output for `decision = "relocated"` or
`decision = "inherited"` per `session_id` to confirm, for each worker spawn,
whether hygiene actually engaged or the fail-safe silently (but loudly, in
the log) fell back to the real HOME. A mission that only ever shows
`decision = "inherited"` still ran workers safely (no unauthenticated
launch), but did not exercise the scratch-env relocation path and does not by
itself satisfy a9 — you need at least one `decision = "relocated"` entry
together with the success signals above to close out the live-auth proof.
