# Per-repo merge gates

The human-triggered dashboard/Slack Merge action reads
`.kranz/merge-gates.json` from the live base branch before it runs any command
or changes the base ref. The file is tracked repo policy: a mission branch may
change its copy for a future merge, but cannot weaken the suite judging the
current mission.

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

- `command` is required and runs through Kranz's bounded gate executor.
- `cwd` defaults to `.` and must be repo-relative without parent components.
- `whenPaths` is an optional list of repo-relative path prefixes. The gate
  runs when the mission diff touches a prefix or anything below it.
- At least one gate must be unconditional. Missing, invalid, empty, or
  conditional-only suites fail closed and leave the base branch untouched.
- Gates run in file order and stop on the first failure. Secret scanning still
  runs before the configured suite.

The suite should mirror the repository's required CI checks. Kranz itself
tracks its Rust workspace gates unconditionally and adds dashboard gates only
when `apps/dashboard` changed.
