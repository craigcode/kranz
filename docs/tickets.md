# Tickets — using the backlog

How to capture work as tickets, turn them into drafted plans, and execute
them on demand. This is the usage companion to the design doc
(docs/backlog-and-slack.md §tickets); commands verified against the CLI.

## Public ticket content

In a public repository, tracked tickets and their discussion sidecars are
public too, including in source downloads and existing Git history. Write
self-contained implementation goals, constraints and acceptance criteria.
Technical source attribution and interoperability requirements belong here;
competitive strategy and private business analysis belong in a separate,
access-controlled notes store outside the checkout. Removing a passage in a
later commit does not remove it from earlier commits or release tags.

## Where tickets live

One ticket = one plain markdown file in the repo at `.kranz/tickets/<slug>.md`:

```markdown
---
title: Persist the serve mutation token to a 0600 file
priority: 2
schedule: once
---

## Goal
One self-sufficient paragraph: the outcome, stated imperatively.

## Context
What a fresh worker needs: file paths, incidents, constraints, prior lessons.

## Scoping answers
Left empty at creation. If a draft comes back NEEDS-CONTEXT, the
orchestrator's questions are appended to the ticket — answer them here
and re-draft.

## Acceptance hints
Concrete testable outcomes. Any named test uses a passed-count guard —
`grep -qE 'result: ok\. [1-9][0-9]* passed'` — never a bare test-name
filter (a zero-match filter exits 0).
```

Tickets are ordinary versioned files: they diff, merge, and review like
code. Pipeline state (NEW → REVIEW → QUEUED → done) lives in a sidecar
state file, not the markdown — editing a ticket never corrupts its state.
Slugs are validated (ascii alphanumerics, `-`, `_`, `.`; no traversal).

A ticket can also declare `blocked-by: [slug, ...]` in its frontmatter to
depend on other tickets — see §Dependencies (`blocked-by`) below. Tickets
drafted by the GitHub webhook trigger (`POST /api/hooks/github`, see
docs/protocol.md §Webhooks) additionally carry `trigger: ci-failure|pr-comment`
provenance; human-authored tickets omit it.

### Reviewing a spec or incident artifact

`spec-review` and `incident-review` are explicit artifact-consumer task
classes. They reuse the normal mission plan, Flight Rules resolver, findings,
waivers, checker results, replay, and evidence bundle. A review ticket names
one tracked UTF-8 source; the source is read-only and the mission must commit
a substantive review output:

```markdown
---
title: Review the API compatibility proposal
task-class: spec-review
review-artifact: docs/proposals/api-v2.md
review-output: reviews/api-v2.md
---

## Goal
Review the proposal against the applicable engineering standards.
```

`review-output` is optional and defaults to `reviews/<ticket-slug>.md`.
Drafting refuses a missing, blank, oversized, untracked, symlink, or non-text
source. Approval requires the plan touch set to include the output and exclude
the input. The final gate refuses a missing/blank/unchanged output and also
refuses any deliverable commit that touched the source—even if a later commit
restored its bytes. Rules scoped by `task-classes` and `when-paths` resolve
against the immutable input as read-only context; exceptions and attestations
remain bound to the resulting review diff.

## Importing an OpenSpec change

If you author work with [OpenSpec](https://github.com/Fission-AI/OpenSpec),
`kranz ticket import-openspec` turns a change folder into a ticket:

```bash
kranz ticket import-openspec openspec/changes/dark-mode
```

It reads `proposal.md` (required) into `## Goal`, and `design.md` plus every
`specs/**/*.md` into `## Context` as authored intent. The slug defaults to the
change directory's name; `--slug` overrides it. Re-importing refuses rather
than overwriting a ticket you have since edited.

Two things it deliberately does not do, both of which matter more than what it
does.

It never imports `tasks.md`. That checklist is the authoring assistant's own
decomposition, self-reported and ungraded. Importing it would slip an
unvalidated plan past the orchestrator, which is the step that should be doing
that thinking.

It never turns spec scenarios into acceptance hints. "The app SHALL default to
the system preference" is prose, and prose cannot fail, so copying it into
`## Acceptance hints` would ship a vacuous criterion with every imported
mission. The generated section says so and asks for commands that can fail,
guarded by a passed count. Fill it in before drafting.

The import runs one way. OpenSpec explains why the work exists; once you
approve a plan, that plan is what the validator judges. Nothing writes back to
`openspec/changes/`, so the two cannot drift into disagreeing about what was
actually built.

The lighter-weight alternative needs no import at all: point a review ticket
at the proposal with `task-class: spec-review` and `review-artifact`, and
kranz will critique the spec before anyone builds against it.

## Importing a Magic Atlas program bundle

A program bundle is a frozen work package that Magic Atlas (the `mgatlas`
command) writes from a documented legacy application: the programs reachable
from one root, grouped by static call dependency in callee-first order, with a
context packet per program, a facts file per group, and a `bundle.json` that
records the size and SHA-256 of every other file. The bundle's own `README.md`
is the format reference. `kranz ticket import-atlas` turns one bundle into one
ticket per group:

```bash
kranz ticket import-atlas bundles/app
kranz ticket import-atlas bundles/app --prefix legacy \
  --target-note "the stack described in docs/target.md, code under src/new"
```

Each ticket is `.kranz/tickets/<prefix>-<group id>.md` (`--prefix` defaults to
`atlas`), written in the bundle's group order. Its `blocked-by` lists the
tickets of the groups it depends on, in the bundle's order, so the blocked-by
gate described below runs the missions callee-first. A dependency the group
reaches by reference only still blocks (see below). Priority is the ticket
default for every group: the order comes from `blocked-by`, never from
priority. The importer reads only the documented bundle files; `mgatlas` does
not need to be installed to import. It must be on the `PATH` wherever missions
run their gates, because the generated acceptance commands call it.

The bundle must be a directory inside the repository, reached without passing
through a symlink, because workers run in isolated worktrees of this
repository and read the bundle there. Each component of its path must be
letters, digits, `-`, `_` or `.`, since the path goes into a generated
command, and must be spelled as the directory is named on disk: a filesystem
that ignores letter case would open another spelling, but a worker's worktree
may not. The importer does not ask git whether the bundle is tracked (neither
does `import-openspec`): import first, then commit the bundle and the tickets
together. A bundle that is not committed is invisible to a worker.

Before writing anything it checks the whole bundle and refuses on the first
problem: a `bundle_schema` other than 1, 2 or 3, a bundle not marked `complete`,
a missing field, a manifest path that leaves the bundle, a duplicate group
(two ids that differ only by letter case count as duplicates, since their
tickets would be one file on a filesystem that ignores case), a dependency on
an unknown group, a dependency cycle, a `depends_on_by_reference_only` list
that a schema 3 group lacks, a schema 1 or 2 group has, or that names a group
outside the group's `depends_on` or names one twice, a group id that cannot
be part of a slug, a program its group's list repeats or leaves out, two
groups sharing one facts file, and any difference between the directory and
`bundle.json`. That last check re-hashes every listed file and also refuses
a file or empty directory the manifest does not list, as
`mgatlas bundle --verify` does. A bundle that fails it would fail the
generated acceptance command too. It also counts the non-empty lines of each
facts file and refuses a group whose `fact_count` says otherwise, because the
acceptance text is built on that count. A bundle with more than 2000 groups,
or a group that depends on more than 200 others, is refused.

What a ticket holds:

- `## Goal` asks for the group's programs to be reimplemented on the target
  stack and for every fact to be accounted for in the coverage map. It names
  no language or framework. The target stack is yours to state: pass
  `--target-note TEXT` and it is inserted verbatim, or fill in the line marked
  `OPERATOR:` before drafting.
- `## Context` gives the bundle path and the SHA-256 of `bundle.json` at
  import, the group id and layer, each program with its entity id, name and
  packet index, the facts and blocks files with their counts, the group's
  loose ends and data sources from `groups.json`, and the instruction to read
  a packet's index before relying on the packet. Lists stop at 20 programs,
  10 loose ends and 10 data sources, each with an "N more in `<file>`" line.
- From a schema 3 bundle, `## Context` also holds what that schema adds:
  - The dependencies the group reaches by reference only
    (`depends_on_by_reference_only`), named by ticket. A program of the group
    names a program of that ticket in an expression or in a variable no
    control is bound to, and the bundle records no call between them. It is a
    mention, not a call. The ticket stays in `blocked-by`, because the
    mentioned program is still worth having first. The mention is a program
    literal in an expression or the selection program of such a variable, so
    the text sends the worker to the group's `references_program` facts to
    see how the program is named, unless the pairs say how themselves (see
    below).
  - The group's reference links (`reference_links` in `groups.json`), external
    invocations and uninterpreted operations, among the loose ends, counted
    in the loose-ends line and within the same limit of 10. A reference link
    reads "`<caller id>` <name> names `<callee id>` <name> of ticket
    `<slug>`" with its number of occurrences. Reference links are listed
    ahead of the other loose ends, so the pairs behind the by-reference line
    are in the ticket unless a group has more than 10 of them; the
    by-reference line then says that only the first 10 are listed and that
    `groups.json` holds them all.
  - How a reference link names its program, when the bundle says so. A
    current Magic Atlas writes `reference_kinds` and `facts` on every
    reference link; a schema 3 bundle written before that has neither, and its
    tickets read exactly as they did. With them the bullet goes on after the
    occurrences, one part for each way the program is named, each with the
    ids of its facts in facts-file order: "(3 occurrences): in an expression
    (facts `<fact id>`, `<fact id>`); as the selection program of a variable
    no control is bound to (fact `<fact id>`)". The parts follow the order in
    which the ways first appear in the facts. The ids are those of the
    `references_program` facts in the group's facts file.
    - The kind `unclassified`, or a fact with no kind, reads "in a way the
      bundle does not classify". Any other kind reads "as a reference of a
      kind this importer does not know", with the kind next to each fact id
      as in "`<fact id>` of kind `<kind>`"; it is never presented as one of
      the known kinds. A known kind is matched after the whitespace around it
      is dropped, and in that spelling only.
    - A link names at most 10 facts; the rest are counted, as in "and 2 more
      in `groups.json`". The 10 are counted across the whole link, so a later
      part can read "(2 facts in `groups.json`)" with no id.
    - A fact id or a kind is shown only when it is a plain token of letters,
      digits and `-`, `_`, `.`, `/`, `:`, `%`, no longer than the bound for an
      id or a name. Anything else is counted like a fact past the 10 and not
      shown, because escaped text in the middle of the bullet could still
      read as the importer's own words. A fact with no id is counted the same
      way. The fact ids Magic Atlas writes are plain tokens.
    - The ways are taken from the facts. `reference_kinds` is read only when
      the link lists no fact; the bullet then gives the ways without fact
      ids, naming an unknown kind as in "(kind `<kind>`)". The two fields are
      not checked against each other.
    - When every reference link of the group says something on how, the
      by-reference line stops listing the ways a program can be named and
      reads "Each pair below says what the bundle records about how"; the
      pairs can differ from one to the next, and a pair may say only that the
      bundle does not classify the reference. When some links do and others
      do not, the line says so and sends the worker to the facts file for the
      others. When none does, the line is the one an earlier bundle gets.
  - What a reference link contradicts itself on. The two fields are read as
    leniently as the rest of a loose end, and the import never fails on
    them: a value of another shape reads as absent, an entry of
    `reference_kinds` that is not a string is left out, and an entry of
    `facts` that is not a record is left out and the bullet says how many
    were.
    - A fact that names another `program` than the link's caller, or another
      target than its callee, is left out and the bullet says how many were.
      A fact that names neither is kept.
    - When the link gives its `occurrences` and the facts kept do not add up
      to it, the bullet says "`groups.json` is inconsistent here" with both
      numbers. Leaving a fact out usually brings this about as well. When a
      kept fact has no positive whole number as its `occurrences`, the bullet
      says how many facts give no usable count and gives no sum.
    - The link's own count is the one shown. A link with no count, or 0,
      shows none and is not checked.
  - A loose end that is a fact carries its fact id, as in
    "external invocation in `<program id>` (fact `<fact id>`): COM
    `<stored text>`". Two invocations with the same text stay two bullets,
    and the worker can say in the coverage map how each was handled. Entries
    that carry no fact id and read the same become one bullet ending in
    "(N identical entries)"; the "N more" line counts entries, not bullets.
  - The kind of an external invocation in words: OS command, UDP, COM, .NET,
    web service. A kind the importer does not know is shown with spaces for
    underscores.
  - Text the application stored (the target of an external invocation, the
    expression of a dynamic call target) in a code span, without backslash
    escaping: a Windows path keeps its single backslashes, and spaces are
    kept as stored. The span holds at most the first 80 characters; a longer
    text is followed, outside the span, by an ellipsis and a note that gives
    its full length and names `groups.json` as the place of the rest. A tab,
    line break or invisible character is shown as one space, and the bullet
    says so. The span is fenced with one backtick more than the longest run
    inside the text.
  - A `Links:` line when the bundle's `link_details` is anything but
    `available`. Such a bundle was cut from an export that does not record
    links from expressions, properties and external invocations, and only
    Call Program operations were followed, so the dependencies and
    `blocked-by` may be incomplete. Export the application again with a
    current Magic Atlas, write the bundle again and re-import. The command's
    output carries the same warning. A value the importer does not know, or a
    missing one, is read as not available instead of refused: the import
    stays usable and the ticket says what may be missing.

  Tickets imported from schema 1 and 2 bundles read exactly as before.
- `## Acceptance hints` carries a command that can fail:

  ```bash
  mgatlas coverage --bundle bundles/app --map atlas-coverage/<group id>.jsonl --group <group id>
  ```

  It exits 0 only when every fact of the group is mapped or waived in the map
  file, which the worker writes as JSON Lines as the bundle README describes.
  An empty or missing map fails. `--map-dir` moves the maps (default
  `atlas-coverage/` at the repository root, tracked like any other
  deliverable); it cannot point inside the bundle, under any letter case,
  where a new file would break the manifest. A group with no recorded facts
  would pass with an empty map, so its ticket says the command is not a gate
  there.

  The coverage command refuses a bundle whose files differ from `bundle.json`,
  but no hash covers `bundle.json` itself: rewritten to record an emptied
  facts file, it would let the command pass with an empty map. So a second
  hint pins the manifest to the SHA-256 recorded at import:

  ```bash
  mgatlas bundle --verify bundles/app | grep -Eq '"manifest_sha256": *"<sha256 at import>"'
  ```

  Adopt both commands together; the first is only as good as the second.

Text taken from the bundle is treated as untrusted. Names, ids and paths are
kept on one line, cut to a fixed length and Markdown-escaped, so a crafted
program name cannot add a frontmatter key, a section heading or an instruction
block. Stored text in a schema 3 loose end is kept on one line and cut to a
fixed length too, then put in a code span that no backtick inside it can
close.
Characters that render as nothing (zero-width and bidirectional controls,
fillers, variation selectors, tag characters) are replaced by a space, so a
name cannot carry text the reviewer does not see.

Re-importing refuses the whole import, and writes nothing, if any of its
tickets already exists; `--force` overwrites them all, including tickets you
have edited since. Tickets left over from an earlier import of a different
bundle are not removed. Ticket state sidecars are never touched. If a write
fails after the checks passed (a full disk, a ticket another process created
in between), the tickets already written are removed, or with `--force`
restored to what they held, and the error names any that could not be.

The command prints one line per ticket and a closing note with the number of
tickets and layers and the size of the largest group. A group with more than
8 programs gets a warning that one mission may be too large. A group is a set
of programs that call each other, so the importer cannot split it; read the
drafted plan's estimate before approving.

Three things it deliberately does not do.

It does not treat the coverage command as proof that the work is right. The
command shows that every recorded fact was accounted for. A map target is free
text nobody opens, and a fact can be waived with any reason, so the new code
can be wrong while the command exits 0. The ticket says so and keeps a line
marked `OPERATOR:` for commands that can fail for behaviour, guarded by a
passed count. Fill it in before drafting.

It does not choose the target language, framework or code location, and it
does not copy packet or fact content into the ticket. The ticket points at the
bundle; the worker reads the bundle.

It does not write back. Nothing under the bundle directory is changed, and it
could not be without making the bundle differ from its manifest.

## Listing the backlog

```sh
kranz ticket list          # slug, priority, state, title
kranz ticket show <slug>   # one ticket in full, incl. needs-context questions
                           # or a wrong-plan escalation reason
kranz queue                # the execution queue (approved, awaiting a run)
kranz queue --remove <id>  # retire one runnable entry; keep its mission audit
kranz queue --timing       # how long each mission waited and ran; --json for data
kranz work --once --expect <id> # run only if this mission is still the front
```

Slack: `/kranz work` reports the queue read-only; the App Home tab shows
the overview. The backlog is also browsable, draftable, and approvable
from the dashboard's Pipeline Backlog lens (`#/backlog`, linked from the mission
picker) — it lists the same slug/priority/state/title/blocked-by columns,
drills into a ticket for its goal/context/needs-context, and drives
draft/approve with the same blocked-by-aware gate described below.

Slack also has direct backlog verbs:

- **`/kranz ticket list`** — one line per ticket (slug, priority, state,
  title). Read-only: no allowlist gate, and the reply is ephemeral (visible
  only to the invoker).
- **`/kranz ticket show <slug>`** — one ticket's full detail (goal, state,
  blocked-by, any parked NEEDS-CONTEXT questions). Also read-only and
  ephemeral, no allowlist gate; an unknown or invalid slug replies with a
  graceful ephemeral error rather than failing silently.
- **`/kranz draft <slug>`** — runs a non-interactive draft turn for a
  backlog ticket through the hosted engine (create the mission, seed it,
  demand the plan). A SPEND action: gated on the `slack.allowUsers`
  allowlist exactly like `/kranz new`, same "not authorized" refusal for
  unlisted users. On authorization it acks immediately with an
  `:hourglass_flowing_sand:` ephemeral (the draft turn takes a minute or
  two), then posts the terminal outcome back to the invoker: ready-for-review
  (mission id + branch), approved-and-queued, or — if the ticket was
  underspecified — **NEEDS-CONTEXT** with the orchestrator's questions
  appended, the same as the CLI/REST draft path.
- **`/kranz approve <slug>`** — the slug-resolving twin of `/kranz approve
  <mission-id>`; approve-by-slug. Gated on the same allowlist as `/kranz
  new`/`draft`. It resolves the ticket's drafted mission and runs the exact
  same `kranz_engine::deps::approve_ticket` gate the CLI and REST approve
  paths run, so a blocked-by refusal (or a cycle, or a not-REVIEW ticket) is
  surfaced to the Slack user **verbatim** — never paraphrased. On success it
  replies with the mission id now queued for `kranz work`.

## The pipeline: draft → review → approve → drain

```sh
kranz ticket new my-fix --title "..." --goal "..."   # scaffold (refuses overwrite)
kranz draft my-fix        # orchestrator drafts a plan, parks plan.md for review
kranz ticket approve my-fix   # commit approval, enqueue, mark QUEUED
kranz work                # drain the queue: one mission at a time, crash-safe
```

Headless producers that start from an external ticket-shaped brief can use
`kranz exec -f mission.md --enqueue`: it creates, plans, and approves the
mission, then writes its mission id to this same queue without running a
worker. Producers that must reconcile a terminal result back to their own
system pair it with `--enqueue-source <producer> --enqueue-external-ref <id>`;
kranz writes that structured ownership receipt before exposing the queue
entry. Execution still belongs to `kranz work` or the serve drain.

What each step really does:

- **`kranz draft <slug>`** is non-interactive: the orchestrator is seeded
  with the whole ticket, produces a plan, and parks a committed `plan.md`
  for review — no execution, no repo changes. An underspecified ticket
  comes back **NEEDS-CONTEXT** with the orchestrator's actual questions
  appended to the ticket file; answer under `## Scoping answers` and
  re-draft. Spend is bounded by the orchestrator budget cap.
- **`kranz draft <slug> --yes`** collapses draft → approve → queue into
  one command. Only for tickets you trust completely: you are skipping the
  one human gate between a ticket and paid execution, and estimates for
  unusual mission shapes can miss badly (m-d341a7: $163.64 actual vs
  $18.35 expected — a doc-heavy shape the calibration corpus didn't model).
- **`kranz ticket approve <slug>`** requires the REVIEW state (a parked
  drafted plan) and moves it to QUEUED with a queue entry.
- **`kranz work`** drains the per-repo queue serially — missions own the
  working tree, so one at a time. `--once` processes exactly one front
  entry and exits (exit 0 if the repo is busy). Crash recovery is
  automatic: claims are atomic file renames, and dead dispatchers' claims
  are recovered on the next run.

## Dependencies (`blocked-by`)

A ticket can name other tickets it depends on:

```markdown
---
title: Wire the new client into the dashboard
priority: 2
blocked-by: [add-client-lib, add-client-tests]
---
```

A blocker is **satisfied only when its mission reaches `Complete`** —
approved, queued, running, or failed all still count as blocked. There is
no partial credit: satisfaction is authoritative on mission status alone,
never on ticket/queue pipeline state.

- **`kranz ticket approve <slug>`** refuses an approval with an unsatisfied
  blocker, naming it honestly:

  ```
  cannot approve <slug>: blocked by <blocker>, <blocker> (its mission is not Complete)
  ```

- **`kranz ticket approve <slug> --force`** overrides unsatisfied blockers
  (approve anyway, at your own risk) — but never a cycle (below); a cycle
  is refused unconditionally.
- **Cycle detection** runs at approve time regardless of `--force`: the
  `blocked-by` graph is walked from `slug`, and a reachable cycle refuses
  the approval with the path spelled out:

  ```
  blocked-by cycle: a -> b -> a
  ```

- **`kranz work` re-checks blockers at work time**, not just at approve
  time: batch-approving a night's worth of tickets can queue a dependent
  right alongside a blocker that later fails mid-drain. When the
  dispatcher is about to run a queued entry and finds a `blocked-by` ticket
  that reached `Failed`, it skips the entry — retiring its claim, marking
  the ticket `Failed` with a note naming the blocker, and printing:

  ```
  warning: skipping ticket '<slug>' — blocked-by '<blocker>' failed
  ```

  This is a one-shot skip, not a retry loop — re-driving a mission whose
  dependency failed can never succeed on its own.

The CLI and the REST `POST /api/tickets/:slug/approve` route share this
exact gate (`kranz_engine::deps::approve_ticket`), so the two surfaces can
never drift on what "approvable" means.

## The backlog over REST

`kranz serve` exposes the same backlog as a REST surface, as an
alternative to the CLI pipeline above:

```
GET  /api/tickets              # list: slug, priority, state, title, blockedBy
GET  /api/tickets/:slug        # full ticket + needsContext + wrongPlan
POST /api/tickets/:slug/draft  # kick off a draft (202, long-running)
POST /api/tickets/:slug/approve  # {"force": bool} -> approve into the queue
```

`draft` is asynchronous: it returns `202 {"missionId":"m-…"}` immediately
and the draft turns run as a background task. Watch progress over that
mission's `GET /api/missions/:id/ws` feed, and poll `GET /api/tickets/:slug`
for the terminal outcome (Review vs NEEDS-CONTEXT). Both `POST` routes
require the mutation token like every other `POST` (docs/protocol.md
§Authority); `approve` returns the same `409` shapes described above for
an unsatisfied blocker or a cycle. See docs/protocol.md §Tickets for the
full request/response reference.

## Batch pattern

```sh
kranz draft fix-a
kranz draft fix-b
# read both parked plans — check the estimates before committing spend
kranz ticket approve fix-a
kranz ticket approve fix-b
kranz work        # leave it in a spare terminal; exits when the queue is dry
```

Queue order is `(priority, insertion order)` from the frontmatter
`priority` field. Batch-approving a night's worth and letting `kranz work`
grind is the intended workflow; reviewing each drafted plan's estimate
first is the one gate worth keeping human.

## Operational notes

- With `kranz serve` running, a drafted mission stays attached to serve
  (holding the mission lock) until approval releases it — the approve
  paths hand off automatically. Expect `kranz status` to show the lock
  moving between serve and the `kranz work` dispatcher.
- Ticket-approve is spend-adjacent everywhere: in Slack it is gated on the
  user allowlist like `/kranz new`.
- Mission outcomes flow back: exit 0 completes the ticket, a blocked
  mission (exit 2) surfaces for guidance, an underspecified plan (exit 3)
  bounces the ticket back with questions.
