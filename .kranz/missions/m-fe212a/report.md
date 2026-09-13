# Mission report — m-fe212a

**Goal:** State explicitly in the worker and orchestrator role prompts that the validation contract lives only in the approved plan.json (folded from plan.approved), that assertions are frozen and un-editable by any agent, and that a suspected contract-authoring bug must be escalated to the operator rather than fixed by editing plan text.

Branch `kranz/mission-m-fe212a` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 9h 24m 54s
**Tokens:** 18564 in / 35508 out / 2629944 cache read / 303066 cache write
**Cost:** $10.70 actual vs $2.61–$13.06 estimated (expected $6.09)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-fe212a-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a1] runs `cargo test` without anti-vacuity (`ok. [1-9]`); a zero-test filter would pass vacuously

## What shipped

### Milestone 1 — Prompts state the contract's single source of truth ✅

- ✅ **Document the frozen-contract rule in both role prompts and assert it with a prompt-content test** — 1 run
  - `d095d01` [f-1-1] document frozen validation contract in role prompts

## Validation history

### ms-1 round 1 — Prompts state the contract's single source of truth

No findings.

### Final gate

- [critical] a4 *(final gate)* — verdict turn unparseable; assertion could not be verified

Disposition: waived.
- a4: Not a real defect — the finding stems solely from one transient unparseable verdict turn, not any deficiency in the work. a4 was re-verified pass on the successful retry: both role prompts faithfully convey all three sub-rules (contract = approved plan.json / plan.md doesn't move the gate; assertions frozen and un-editable by any agent; suspected bug escalated to operator, never fixed by editing plan text). The prose is already correct, so a fresh worker session has nothing to fix.

## Contract outcomes

- ✅ **[a1]** The compiled worker and orchestrator role prompts both state that the validation contract lives in the approved plan.json, that editing plan.md does not change the gate, and that a suspected assertion bug is escalated rather than edited — asserted by a prompt-content test. *(command: `cargo test -p kranz-engine --test config_cost_prompts_test prompts_state_contract_lives_in_approved_plan_json -- --exact`)*
- ✅ **[a2]** The entire workspace test suite passes, including the existing prompt hash/format tests (prompt_hashes_are_12_hex_chars_and_distinct) and prompts.rs's all_prompts_mention_base_sha, proving the prompt edits broke nothing. *(command: `cargo test --workspace`)*
- ✅ **[a3]** All Rust sources, including the edited test file, satisfy the repo's formatting gate. *(command: `cargo fmt --all --check`)*
- ✅ **[a4]** The added prompt paragraphs faithfully convey all three sub-rules in clear, non-contradictory prose: (1) the contract is the approved plan.json and editing plan.md does not move the gate; (2) assertions are frozen and cannot be edited by any agent; (3) a suspected contract-authoring bug is escalated to the operator (blocked + message), never 'fixed' by editing plan text. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
