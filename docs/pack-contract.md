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
that simply registers nothing; schema 3 adds the declaration sections.

## pack.toml schema

```toml
[pack]
name = "acme-standards"     # required, non-empty
schema = 3                  # required: 2 (base manifest) or 3 (contract)

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
```

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
targets and sources, declaration-only checklists and artefact stores).

- valid pack ⇒ the registration report, exit 0
- no `pack.toml` ⇒ "no pack at \<dir\> — nothing to lint", exit 0
- invalid pack ⇒ nonzero exit, the error naming the offending field

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
