# Mission report — m-45624a

**Goal:** Enable a 'live QA mode' for the functional validator: when a mission config lists browser/computer-use tooling for validatorFunctional, that validator both receives (--tools) and is permitted (--allowedTools) those tools, is prompted to drive the built app against acceptance criteria, and the pattern is documented in a docs/validating-uis.md cookbook.

Branch `kranz/mission-m-45624a` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 22m 00s
**Tokens:** 22510 in / 77920 out / 5055612 cache read / 262879 cache write
**Cost:** $25.69 actual vs $5.10–$25.50 estimated (expected $10.20)

## What shipped

### Milestone 1 — Functional validator drives the app in live QA mode ✅

- ✅ **Grant functional validator its configured tools (allow-list) + mock-seam integration test** — 1 run
  - `b1204ba` [f-1-1] fold functional validator's extra configured tools into allowed_tools
  - `378d3ba` [f-1-1] checkpoint (engine commit)
- ✅ **Add always-on Live QA section to the functional validator prompt** — 1 run
  - `de4a962` [f-1-2] add always-on Live QA section to functional validator prompt
- ✅ **Write docs/validating-uis.md cookbook with config block and dashboard worked example** — 1 run
  - `768615a` [f-1-3] add docs/validating-uis.md live QA cookbook

## Validation history

### ms-1 round 1 — Functional validator drives the app in live QA mode

- [critical] a8 — no files changed outside the allowed set (measured against pinned base) — Running the exact a8 command against KRANZ_BASE_SHA=4d431d1 yields non-empty output, so `test -z ...` fails: .kranz/missions/index.md, .kranz/missions/m-45624a/plan.json, .kranz/missions/m-45624a/plan… [truncated]
- [minor] a8 — Command: test -z "$(git diff --name-only $KRANZ_BASE_SHA | grep -vE '^(crates/engine/prompts/validator-functional\.md|docs/validating-uis\.md|crates/engine/src/permissions\.rs|crates/engine/src/runner… [truncated]

Disposition: waived.
- a8 — no files changed outside the allowed set (critical): Not scope creep: the failure is entirely Kranz's own mission scaffolding — .kranz/missions/{index.md,plan.json,plan.md} are written by the mandatory [kranz] plan-approval commit (640f7d4) that sits between the pinned base and milestone, and .kranz/slack-threads.json is engine bridge state. No source/prompt/doc surface outside the allow-list changed; a8's diff-against-base scoping is the flaw, and no worker fix can remove the required approval commit.
- a8 (minor duplicate): Same root cause as the critical finding — the four flagged paths are all .kranz/ bookkeeping artifacts inherent to every mission's diff against $KRANZ_BASE_SHA; the actual milestone diff matches the allow-list exactly. Assertion-scoping gap, not a deliverable defect.

## Contract outcomes

- ✅ **[a1]** The functional validator's SessionSpec both carries its configured tools in `tools` and auto-approves the extra (non-inspect) tools in `allowed_tools`, without broadening Bash and without dropping the Write/Edit deny — proven by a named mock-seam integration test. *(command: `cargo test -p kranz-engine functional_validator_tools_are_carried_and_allowed 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** The full kranz-engine test suite passes (no regressions introduced by the prompt/engine/test changes). *(command: `cargo test -p kranz-engine`)*
- ✅ **[a3]** kranz-engine is clippy-clean with warnings denied. *(command: `cargo clippy -p kranz-engine --all-targets -- -D warnings`)*
- ✅ **[a4]** docs/validating-uis.md exists and contains a concrete .kranz/config.json block that sets validatorFunctional.tools. *(command: `test -f docs/validating-uis.md && grep -q 'validatorFunctional' docs/validating-uis.md && grep -q '"tools"' docs/validating-uis.md`)*
- ✅ **[a5]** The functional validator prompt contains an always-present Live QA section. *(command: `grep -qi 'live qa' crates/engine/prompts/validator-functional.md`)*
- ✅ **[a6]** The Live QA section of validator-functional.md is always present, self-activates on the session having browser/computer-use tools (not a template flag), instructs starting the app per the repo's run docs, exercising each acceptance criterion live, capturing evidence (URLs visited, observed text/state), staying read-only, does not hardcode any specific tool name, and preserves the existing findings-JSON contract. *(agent judgement)*
- ✅ **[a7]** docs/validating-uis.md is a coherent cookbook: it warns that `tools` is the exclusive --tools set so Bash/Read/Glob/Grep must be listed alongside the browser tool, uses a placeholder (not a real hardcoded) browser tool name, explains that the functional validator's configured extra tools are auto-approved while Write/Edit stay denied, and includes a worked apps/dashboard example with an accurate start command and at least one live-driven acceptance criterion plus the evidence to capture. *(agent judgement)*
- ✅ **[a8]** No files are changed outside the functional-validator prompt, the cookbook doc, the engine permissions/runner source, the engine tests, and the design doc — measured against the pinned base commit. *(command: `test -z "$(git diff --name-only $KRANZ_BASE_SHA | grep -vE '^(crates/engine/prompts/validator-functional\.md|docs/validating-uis\.md|crates/engine/src/permissions\.rs|crates/engine/src/runner\.rs|crates/engine/tests/[A-Za-z0-9_]+\.rs|docs/design\.md)$')"`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
