# ms-2 milestone status: inherited (no-op)

Finding under review: "ms-2 milestone diff (a13 / f-2-2 deliverables)".

## What the finding observed

The ms-2 milestone range (`2325b58..HEAD`, base = the last ms-1 fix-cycle
commit) was empty at the time of validation: `git rev-list --count
2325b58..HEAD` returned `0` and `git diff --name-only 2325b58 HEAD` returned
nothing. Every ms-2 deliverable named in the milestone goal — the finalized
`backend_cursor` route decision and its implementation brief — already
existed in the tree, but was authored by commits at or before the range
base:

- `## Decision (2026-07-09, revised) -> direct-parser` and the refreshed
  acceptance-bar table in `docs/scoping/cursor-cli-backend.md` — authored by
  `0c91545 [f-1-4]` and `c68337a [ms-1-fix-1-1]`.
- The auth/permission pointer in
  `docs/scoping/cursor-probe-evidence/implementation-brief.md` — authored by
  `2325b58 [ms-1-fix-1-3]`.

None of those commits are inside `2325b58..HEAD` (the range is exclusive of
its base), so ms-2's own diff had no work to show for a milestone goal that
was, in substance, already satisfied.

## Resolution

Per the suggested fix's first branch: the ms-1 fix cycle legitimately
completed the route finalization and brief before ms-2 opened. Confirming
intent explicitly, rather than leaving ms-2's "green" to be read as work
performed within this milestone:

- **ms-2 is recorded here as an inherited / no-op milestone.** The
  `backend_cursor` route decision (`direct-parser`, see
  `docs/scoping/cursor-cli-backend.md#decision-2026-07-09-revised`) and its
  implementation brief (`docs/scoping/cursor-probe-evidence/implementation-brief.md`)
  were finalized in full by the ms-1 fix cycle (`0c91545`, `c68337a`,
  `2325b58`) and are not revised, extended, or re-derived by ms-2.
- No route-decision content changes in this milestone. This file, and the
  companion check script (`check-ms-2-fix-1-1.sh`), are the only artifacts
  ms-2 contributes: a record that the milestone's deliverables were
  pre-existing and an assertion that the "empty range" reading of that state
  does not recur silently.
- `probe-result.json.route_decision` remains the single machine-readable
  pointer to the decision (`"decision": "direct-parser"`, `"decided_by":
  "f-1-4 (2026-07-09)"`); it is unchanged by ms-2.

If a future milestone needs to revise the route decision, it should update
`docs/scoping/cursor-cli-backend.md` directly and add a new `## Decision
(<date>, revised)` section, the same pattern `c68337a` used to supersede the
original `defer` call.
