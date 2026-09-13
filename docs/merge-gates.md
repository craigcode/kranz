# Per-repo merge gates

The human-triggered dashboard/Slack Merge action requires a COMPLETE mission,
holds the repo-wide busy lock, and pins both the live base and mission tip
before doing any work. It reads `.kranz/merge-gates.json` and
`.kranz/secret-allowlist` from that pinned live base. Both are tracked repo
policy: a mission branch may change its copy for a future merge, but cannot
weaken or waive the checks judging its own code.

```json
{
  "gates": [
    { "command": "cargo test --workspace" },
    {
      "command": "npm test",
      "cwd": "apps/web",
      "whenPaths": ["apps/web"]
    }
  ]
}
```

- `command` is required and runs with a 600-second process-tree timeout and a
  sanitized environment that excludes server/API credentials.
- `cwd` defaults to `.` and must be repo-relative without parent components.
- `whenPaths` is an optional list of repo-relative path prefixes. The gate
  runs when the mission diff touches a prefix or anything below it.
- At least one gate must be unconditional. Missing, invalid, empty, or
  conditional-only suites fail closed and leave the base branch untouched.
- Gates run in file order and stop on the first failure. Secret scanning still
  runs before the configured suite.

Kranz creates a detached scratch worktree at the pinned live base, merges the
pinned mission SHA there, and runs every applicable gate against that exact
integration result. Only a green integration commit may advance the base, and
the base advances by fast-forward to that same tested commit. A gate failure,
timeout, conflict, moving base, or late mission-branch commit cannot land
untested code; the primary base remains unchanged on refusal.

The suite should mirror the repository's required CI checks. Kranz itself
tracks its Rust workspace gates unconditionally and adds dashboard gates only
when `apps/dashboard` changed.
