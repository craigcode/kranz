# The pack contract — packs register gates, prompts, checklists, artefact stores

Ticket: `.kranz/tickets/pack-contract-gates-prompts.md` (KRZ-313 series).
Implementation: `crates/engine/src/pack.rs` (+ `pack/toml.rs`).

A **pack** is a directory with a `pack.toml` that declares deterministic
gates, role prompts, checklists, and artefact-store adapters. kranz
validates the declaration at load — fully locally, no Gas City
infrastructure — and wires it into the mission surfaces. This is the IP
boundary made mechanical: **kranz core stays domain-free; domain knowledge
(house standards, review lenses, evidence stores) ships in private packs.**

The contract **extends** the existing pack concept
(`packaging/gascity/pack.toml`, schema 2, `docs/gascity-citizenship.md`) —
it is not a second mechanism. The schema-2 base manifest is a valid pack
that simply registers nothing; schema 3 adds the declaration sections;
schema 4 adds the optional `[standards]` root (the Flight Rules corpus,
KRZ-341 — see below).

## pack.toml schema

```toml
[pack]
name = "acme-standards"     # required, non-empty
schema = 3                  # required: 2 (base manifest), 3 (contract),
                            # or 4 (contract + [standards])

# Zero or more DETERMINISTIC gates. They run at the mission's final gate,
# AFTER the engine floor gates, through the same GatePipeline — advisory,
# like the floor: verdicts are recorded via the decision path and never
# block. A pack can add to the floor, never lower, reorder, or replace it.
[[gate]]
name = "lint-rules"         # required, unique among gates; engine floor
                            # names (vacuous-filter, wrong-polarity,
                            # passes-on-base, env-sensitive,
                            # merge-gate-suite) are reserved
command = "./lint.sh"       # required, non-empty, single line
# kind = "deterministic"    # optional; the ONLY accepted value. Packs may
                            # not declare model-judged gates in this slice —
                            # kind = "model-judged" is a load error.
# whenPaths = ["src/"]      # optional; merge-gate idiom: the gate runs when
                            # at least one changed path equals or sits below
                            # a prefix. Omit to run unconditionally.

# Zero or more prompts, appended to the target role's rendered prompt
# (worker.md / validator-scrutiny.md / validator-functional.md — the same
# append_system_prompt plumbing the embedded templates use).
[[prompt]]
name = "house-style"        # required, unique among prompts
role = "worker"             # required: worker | validator-scrutiny |
                            # validator-functional
text = "Prefer small, reviewed commits."
# textFile = "prompts/style.md"   # exactly one of text / textFile;
                                  # textFile is pack-relative (no `..`),
                                  # read once at load

# Zero or more checklists. DECLARATION-ONLY in this slice: validated at
# load, reported by lint and the run-start decision, never executed.
[[checklist]]
name = "release-readiness"
items = ["changelog updated", "version bumped"]

# Zero or more artefact-store adapters. DECLARATION-ONLY in this slice:
# validated at load, never invoked.
[[artefact_store]]
name = "evidence"
kind = "local-dir"

# Schema 4 only (KRZ-341): the optional Flight Rules standards root. A
# [standards] section at schema 2/3 is a load error naming the field;
# unknown keys inside it fail closed.
# [standards]
# root = "standards"        # pack-relative directory holding the RFC corpus
```

## The standards corpus (schema 4, KRZ-341)

Ticket: `.kranz/tickets/flight-rules-pack-contract.md`; design:
`docs/scoping/flight-rules-engineering-standards.md` (D-A through D-C, D-F,
D-J). Implementation: `crates/engine/src/pack/standards.rs`.

A schema-4 pack may carry a governed standards corpus — human-readable RFCs
containing stable machine-readable rules:

```text
<root>/RFC-014-slug/rfc.md            # one directory per RFC
<root>/RFC-014-slug/rules/RULE-ID.md  # flat rule files, one rule each
```

`rfc.md` frontmatter: `id`, `title`, `owner`, `status` (`draft` /
`approved` / `enforced` / `retired`), optional RFC3339 `effective-at`,
optional `supersedes` list. Rule frontmatter: `id`, positive `revision`,
`rfc` (parent id), `level` (`must` / `should`), `status` (`active` /
`retired`), one-line `statement`, `domains` and `stages` lists
(`planning` / `implementation` / `validation` / `merge`), optional
`when-paths` and `task-classes`, optional typed `checker`
(`gate:<declared-pack-gate>` / `agent-judgement` / `manual-attestation`),
and `waivable` (default `false` — fail-closed).

The load is strict and bounded (D-J):

- **Frontmatter is a hand-written subset** (no YAML dependency, the same
  call as the TOML subset): `key: value` scalars, `key: [a, b]` inline
  lists, double-quoted strings, `#` comments. Anchors, aliases, tags,
  block scalars, single-quoted strings, nested/block values, tabs, and
  unknown or duplicate fields are all load errors naming file and field.
- **Traversal is capability-relative and no-follow**: regular UTF-8 files
  only; symlinked parents/leaves, FIFOs/devices, and hidden files are
  refused, never followed. Caps fail promptly: 256 KiB per file, 512
  files, 256 rules, 1 MiB total normalized bytes
  (`MAX_STANDARDS_*` constants).
- **Identity is frontmatter IDs, never paths** (D-C): IDs are pack-wide
  unique across RFCs and rules; renaming a file preserves the digest. The
  sha256 digest covers the normalized metadata of every RFC/rule (sorted
  by id, lists sorted/deduplicated) PLUS the declarations of referenced
  pack gates — changing a referenced checker changes the digest. Markdown
  prose bodies are never hashed (rationale, not a second machine
  authority).
- **Lifecycle is checked at load and across refs** (D-B/D-C/D-F): draft
  rules may omit a checker; an approved/enforced rule must bind one.
  Retirement is a one-way tombstone. `kranz standards lint --against
  <ref>` reads the base from tracked git blobs (never the worktree) and
  refuses absent/draft → enforced transitions, semantic rule changes
  (statement/level/stages/when-paths/task-classes/checker/waivable)
  without a revision increment, disappeared known rule IDs, and tombstone
  reactivation.
- **Trust boundary** (D-A/D-J): a tracked, repo-relative pack may activate
  enforced rules. An external/untracked pack loads approved advisory rules
  but enforced content is a load error naming the remedy (vendor the pack
  into the repo as a tracked, repo-relative `packDir`).


The manifest is parsed by a **deliberate TOML subset**
(`crates/engine/src/pack/toml.rs`): `[table]` / `[[array-of-tables]]`
headers, bare keys, basic/literal strings, integers, booleans, and
(possibly multi-line) arrays. Everything else — dotted keys, inline
tables, floats, datetimes, multi-line strings — is rejected as an
unsupported construct. WHY hand-rolled: the dependency tree has no TOML
crate and AGENTS.md prefers existing utilities over new dependencies; the
subset is exactly what the contract needs, and shapes it cannot fully
account for fail closed rather than misparse.

## Fail-closed validation

Every load path (lint, engine run start, final gate, role-prompt builders)
is the same strict validator. Any violation is a load error **naming the
offending field** — never a silently-skipped section:

- unknown field in any section, or an unknown section (`[[gates]]`)
- missing required key (`[pack].schema`, `[[gate]].command`, …)
- wrong type (`schema = "3"`, `whenPaths = "src"`, …)
- empty value (gate command, names, checklist items)
- duplicate names within a section
- `kind = "model-judged"` (or anything but `deterministic`)
- a gate name reserved for an engine floor gate
- prompt: both/neither of `text`/`textFile`, an unsupported role target
  (orchestrator is engine-only in this slice), a `textFile` that escapes
  the pack or cannot be read
- unsupported schema version, malformed TOML

## `kranz pack lint <dir>`

Fully local lint surface: loads and validates a pack directory and prints
what registered (gates with commands and scoping, prompts with role
targets and sources, declaration-only checklists and artefact stores; for
a schema-4 pack, the standards registration — RFC/rule counts and the
content digest).

- valid pack ⇒ the registration report, exit 0
- no `pack.toml` ⇒ "no pack at \<dir\> — nothing to lint", exit 0
- invalid pack ⇒ nonzero exit, the error naming the offending field

## `kranz standards lint <dir> [--against <ref>]`

The Flight Rules lint surface (KRZ-341): prints the pack's normalized
standards manifest — every RFC and rule with its effective lifecycle
status, checker binding, and scopes, plus the sha256 content digest and
the trust posture the loader applied. With `--against <ref>`, the base
pack is read from tracked blobs at that git ref (`git show` / `git
ls-tree`, never the worktree) and lifecycle transition violations are
refused with exit 1, one line per violation naming the rule or RFC.

- valid corpus, clean transitions ⇒ the manifest report, exit 0
- a pack without `[standards]` ⇒ "…declares no [standards] root", exit 0
- invalid corpus or refused transitions ⇒ nonzero exit naming the
  file/field or the refused transition

## `kranz standards waive --rule <id> --reason <text> --expires <rfc3339>`

The Flight Rules human exception path (KRZ-344, design D-I): records one
`standards.waiver.approved` event against the mission (selected with
`--mission`, as usual), excepting exactly one recorded standards failure.
The command displays the evidence it binds — the finding, the pinned rule,
the affected paths, and the sha256 over the affected-path diff (the whole
mission diff for an unscoped rule) — and records the approver honestly as
`local-operator` plus the `cli` surface. `--revision` and `--finding`
narrow the target; both default to the pinned revision and the latest
citing finding.

- the rule declares `waivable: false`, is absent from the approved pin
  (an expired/retired rule is never pinned), the revision mismatches, no
  finding cites the rule, a live waiver already covers that finding, the
  reason is empty, or the expiry is not in the future ⇒ "waiver refused:
  …", exit 1, nothing appended
- a live engine holds the mission lock ⇒ refused; stop the mission first
- otherwise the event is appended and the binding evidence printed, exit 0

A change to the affected-path diff, the rule revision, the finding
fingerprint, or the pin — or the expiry passing — invalidates the waiver
and restores the block. There is no `--ignore-standards`, auto-waive,
wildcard, or permanent default, and no model-driven approval path: a
model may request a waiver or propose a fix, never approve one.

## Configuring a mission to run with a pack

Set `packDir` in `.kranz/config.json` (repo layer) or
`~/.kranz/config.json` (global layer) — e.g. via
`kranz config set packDir /path/to/pack`. Relative paths resolve against
the repo root. The key is optional and additive; existing configs are
unaffected.

```json
{ "packDir": "vendor/acme-standards" }
```

## What a configured pack changes

- **Run start**: the pack is loaded and validated BEFORE any side effects
  (the same backstop as workspace-provider resolution). An invalid pack
  fails the run closed, naming the field; a valid one records an
  `orchestrator.decision` with the full registration list.
- **Final gate**: pack gates join the ONE shared `GatePipeline` AFTER the
  engine floor gates — registration order is the evaluation order within
  the deterministic section, so the composition is the guarantee. Their
  commands run against the active tree with the same cleared contract
  environment as contract command assertions (`whenPaths`-scoped gates run
  only when the mission diff matches). Verdicts are advisory and recorded
  via the decision path — pass or fail, so a pack's silent green is as
  visible as its failure. Gate-refusal policy remains a separate operator
  decision, same posture as the contract floor gates.
- **Role prompts**: pack prompts targeting `worker`, `validator-scrutiny`,
  or `validator-functional` are appended to that role's rendered prompt at
  spawn time (marked with pack/prompt provenance). The recorded
  `worker.spawned` prompt hash then names the extended text, not the bare
  template.
- **No pack configured ⇒ byte-identical behavior**: every surface checks
  `packDir` first and takes exactly the pre-pack code path when it is
  absent (regression-tested).

## Flight Rules resolution and approval pinning (KRZ-342)

When a schema-4 pack governs, the ENGINE — never a model — resolves the
applicable rules deterministically (design D-D): a rule applies when the
stage is in its `stages`, its `task-classes` is absent or contains the
mission's task class (routing normalization), and its `when-paths` is
absent or overlaps the selection's paths. Domains are browsing labels and
never select. `retired` rules never apply; `draft` rules apply only on
lint/authoring surfaces.

- **Approval pins the manifest** (D-E): `approve_plan` reloads the trusted
  source — tracked base blobs via `standards::load_at_ref` for a
  repo-relative `packDir` (a mission-branch or worktree edit is invisible
  to it), one capability read for an external pack — and writes the
  applicable set into the plan's additive `standardsManifest` (pack
  identity, digest, selection inputs, and every rule's id, revision,
  effective status, statement, scopes, and checker binding) before any
  branch/commit side effect. A plan carrying a stale or substituted
  manifest is rejected naming both digests; an untracked repo-relative
  corpus or an external pack with effectively enforced rules refuses
  approval. `plan.md` renders the pin for review, and a
  `standards.resolved` event records the selection against the
  `plan.approved` seq.
- **Read-only applicability context**: artifact-review missions pin the
  tracked spec/incident input separately as `contextPaths`. These paths take
  part in `when-paths` selection and checker scoping but never enter the
  writable mission touch set. Waivers and manual attestations for a rule
  selected through context bind the review deliverable diff, not an empty
  source diff.
- **The pin, not a later read, governs the mission**: a mission-branch
  pack edit is ignored and surfaced via an advisory `orchestrator.decision`
  (the routing-rules ownership idiom); an external pack edit after
  approval cannot change a run. A plan revision never re-pins.
- **Final validation re-checks the envelope**: the approval-pinned base
  snapshot is re-resolved against the mission's actual deliverable paths;
  a newly applicable enforced rule (a widened touch-set grant, an
  out-of-contract write) parks the mission for revision/reapproval.
- **Merge refuses policy drift**: the merge re-resolves the LIVE base
  policy against the exact scratch integration diff and, when the
  applicable enforced set differs from the approved pin's, refuses with
  `standards.drifted` (approved/current digests plus the changed rules)
  before the gate suite runs — the mission needs explicit
  revalidation/reapproval, never a silent re-judgement under moved policy.

Stage projections into planner/worker/validator prompts, rule-linked
findings, waivers, and checker execution are the KRZ-343–346 slices.

## Deliberately not in this slice

- **Model-judged pack gates** — refused at load; the engine owns
  model-judged gates in this slice.
- **Orchestrator prompt injection** — refused at load; the session roles
  (worker, both validators) are the supported targets.
- **Checklist execution and artefact-store invocation** — declaration-only:
  validated at load and reported, never executed. Declaring the shapes now
  means the slices that consume them need no schema rework.
- **Approval-time pack gates** — pack gates join the final-gate evaluation
  only; the approval surface keeps the engine floor alone.
- **Standards stage projections, findings, waivers, and enforcement** —
  KRZ-341 lands the schema-4 corpus contract, loader, digest, and
  lifecycle lint; KRZ-342 (above) lands deterministic resolution, the
  approval manifest pin, and drift refusal. Stage projections
  (D-G prompt surfaces), rule-linked findings and waiver decisions, and
  checker execution/blocking are the KRZ-343–346 slices
  (docs/scoping/flight-rules-engineering-standards.md).
