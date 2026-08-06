# Tracked routing rules

`.kranz/routing-rules.json` is the tracked, base-branch-owned way to declare
how a ticket's `task-class` frontmatter routes the worker executor to a
capability tier (ticket `routing-rules-config`, the config surface of the
KRZ-331 routing abstraction). Ownership is the merge-gates idiom: the bytes
are read from the live base branch at mission creation, so a mission can
never edit the rules that route it — a mission-branch edit is ignored, and
`surfaced` on the mission's decision log at run time.

```json
{
  "taskClassRules": [
    { "taskClass": "execution-class", "tier": "local" }
  ],
  "patternRules": [
    { "pattern": "docs-*", "tier": "frontier" },
    { "pattern": "*", "tier": "frontier" }
  ]
}
```

Two rule forms, both deterministic:

- `taskClassRules` — complexity-tier rules: an exact task class (compared
  trimmed and case-insensitively) routed to a tier. First match wins.
- `patternRules` — ordered pattern rules, consulted only when no exact rule
  matched: `*` matches any run of characters, every other character is
  literal, and the match is anchored at both ends. First match wins.

No match anywhere falls through to `frontier`. Tiers are capability classes
(`local` | `frontier`), never model ids — a model-id-shaped `tier` is a
schema error. A `local` route with no configured local endpoint
(`worker.baseUrl` + `worker.contextBudget`) fails safe to `frontier` and the
fallback is recorded.

- The file is validated at draft (mission creation) and again at approve; an
  invalid file fails closed naming the file, the rule index, and the field
  (owner: repo-setup).
- When the file exists it IS the routing table: it supersedes any
  layered-config `routing` key wholesale, and the supersession is recorded on
  the mission's decision log.
- The effective route and the deciding rule (`taskClassRules[i]` /
  `patternRules[i]`, or no rule on a fall-through) are recorded on each
  `worker.spawned` as the additive `executorRoute` field — routing is
  provenance, not a hidden implementation detail.
- No file ⇒ today's behavior byte-for-byte: the layered `routing` config
  key, or the hardcoded literal floor (`execution-class` → `local`,
  everything else `frontier`) when no table is configured at all.

Which concrete endpoint a `local` route resolves to is ordinary
local-backend role config (`baseUrl` + `model` + `contextBudget`), not
routing-table content.
