# Agent backends

Kranz is a governance and evidence harness over existing agent runtimes. It
does not install those runtimes, create vendor accounts, or perform interactive
login. Install and authenticate the runtime separately, then use `kranz ready`
to verify discovery before spending on a mission.

| Backend | Executable or endpoint | Authentication Kranz can carry into a cleared session |
|---|---|---|
| Claude Code (default) | `claude` | Existing Claude login state or `ANTHROPIC_API_KEY` |
| Codex CLI | `codex` | Existing Codex login state or `OPENAI_API_KEY` |
| Factory Droid | `droid` | Existing Droid login state or `FACTORY_API_KEY` |
| Kimi Code | `kimi` | Existing Kimi login state; `KIMI_API_KEY` for an already-configured custom provider |
| Cursor | `agent` | `CURSOR_API_KEY`; existing account configuration may also be required by the CLI |
| ACP | Operator-configured `acpCommand` | The peer owns its authentication; Kranz forwards no ambient credential |
| Local OpenAI-compatible | Operator-configured `baseUrl` | Local endpoint policy; no ambient vendor credential is inferred |

The named API-key variable is the only ambient credential admitted for each
native backend. Kranz clears the child environment and copies the backend's
minimal native configuration into a private session home. Unrelated shell,
GitHub, Slack, cloud, and package-registry credentials do not cross that
boundary.

## First-run check

1. Install the chosen vendor CLI using its current official instructions.
2. Complete its login flow directly in that CLI, or export its sanctioned API
   key variable in the environment that starts Kranz.
3. Run `kranz ready` in the target repository.
4. Fix every missing-binary, authentication, model, sandbox, and merge-gate
   finding before starting a paid mission.

Binary overrides such as `KRANZ_CLAUDE_BIN`, `KRANZ_CODEX_BIN`,
`KRANZ_DROID_BIN`, `KRANZ_KIMI_BIN`, and `KRANZ_CURSOR_BIN` are exclusive: a
bad override fails loudly rather than falling back to another executable.
For Claude, a nonempty `claudeBinary` configuration takes precedence over
`KRANZ_CLAUDE_BIN`; either selection is exclusive. A failed or timed-out version
probe reports that path and its failure. PATH and known installation locations
are searched only when neither override is supplied.
Role-specific backend and model selection lives in `.kranz/config.json`; use
`kranz config show` to inspect the effective merged configuration.

Different CLIs can serve the same model family. To require a different family
for scrutiny or functional review, configure
[`reviewerIndependence`](config-composition.md#reviewer-independence-reviewerindependence)
before creating the mission. Approval pins the requirement; resolved backend
fallback must still satisfy it, and unknown identities block rather than count
as independent review. Validator containment remains a separate requirement.
