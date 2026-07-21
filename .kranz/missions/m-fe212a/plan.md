# Mission plan — m-fe212a

**Goal:** State explicitly in the worker and orchestrator role prompts that the validation contract lives only in the approved plan.json (folded from plan.approved), that assertions are frozen and un-editable by any agent, and that a suspected contract-authoring bug must be escalated to the operator rather than fixed by editing plan text.

Branch `kranz/mission-m-fe212a` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$2.61 – $13.06** (expected ~$6.09). Rough estimate — live usage is authoritative; based on 46 completed mission(s).

## Considered alternatives

**Chosen approach:** One milestone, one feature: the change is a cohesive documentation edit to two prompt files plus a single asserting test, comfortably within one worker's turn budget, with no logic changes to sequence.

Rejected shapes:
- **Split into two features (one per prompt file) each with its own test.** — Both features would contend over the same shared test file config_cost_prompts_test.rs, creating file-ownership conflicts for zero benefit on a change this small.
- **Also change engine code to physically block any write to plan.md/plan.json by agents.** — Out of scope and unnecessary — the gate already reads the folded approved plan.json; the only gap is that the prompts never say so. Adding enforcement logic would expand blast radius and risk regressions the mission does not need.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The compiled worker and orchestrator role prompts both state that the validation contract lives in the approved plan.json, that editing plan.md does not change the gate, and that a suspected assertion bug is escalated rather than edited — asserted by a prompt-content test. 
  `cargo test -p kranz-engine --test config_cost_prompts_test prompts_state_contract_lives_in_approved_plan_json -- --exact`
- **[a2]** The entire workspace test suite passes, including the existing prompt hash/format tests (prompt_hashes_are_12_hex_chars_and_distinct) and prompts.rs's all_prompts_mention_base_sha, proving the prompt edits broke nothing. 
  `cargo test --workspace`
- **[a3]** All Rust sources, including the edited test file, satisfy the repo's formatting gate. 
  `cargo fmt --all --check`
- **[a4]** The added prompt paragraphs faithfully convey all three sub-rules in clear, non-contradictory prose: (1) the contract is the approved plan.json and editing plan.md does not move the gate; (2) assertions are frozen and cannot be edited by any agent; (3) a suspected contract-authoring bug is escalated to the operator (blocked + message), never 'fixed' by editing plan text. *(agent judgement)*

## Milestone 1 — Prompts state the contract's single source of truth

### 1.1 Document the frozen-contract rule in both role prompts and assert it with a prompt-content test

Goal: make the worker and orchestrator role prompts state explicitly WHERE the validation contract lives and what to do when an assertion looks buggy, so agents stop trying to 'fix' the contract by editing plan.md. The material fact is already true in code: the merge/validation gate reads the approved plan.json, which is folded from the `plan.approved` event (see crates/engine/src/digest.rs line ~109 rendering 'APPROVED PLAN (plan.json)', and reducer::fold). You are ONLY adding documentation prose plus one test. Do NOT change any engine logic.

Files to edit:
1. crates/engine/prompts/orchestrator.md — add ONE focused paragraph in the execution phase (a natural home is near the 'Converting validator findings' / 'When in doubt' subsections, ~lines 129-139). It must state: (a) the validation contract lives ONLY in the approved plan.json, folded from the `plan.approved` event; editing `plan.md` does NOT change the gate; (b) contract assertions are frozen at approval and cannot be edited by any agent (worker or orchestrator); (c) if you suspect a contract-authoring bug, you must escalate to the operator (block the mission with a message) rather than spawn a feature to edit plan.md/plan.json assertion text.
2. crates/engine/prompts/worker.md — add ONE focused paragraph in the '## Hard rules' section (~lines 14-20). It must state: (a) the validation contract is fixed in the approved plan.json; (b) you must NEVER edit plan.md or plan.json to change an assertion; (c) if a feature spec asks you to edit contract/assertion text, or you believe an assertion is buggy, stop and report `result: "fail"` with the reason in `summary` so the orchestrator can escalate to the operator — do not 'fix' the contract by editing plan text.

Each paragraph MUST contain, case-insensitively, the literal substrings `plan.json`, `plan.md`, and `operator`, and a word beginning `escalat` (e.g. 'escalate'/'escalated'). Keep the prose in the existing voice and Markdown style of each file; do not remove the existing `$KRANZ_BASE_SHA` sentences (the existing prompts.rs test all_prompts_mention_base_sha requires KRANZ_BASE_SHA to remain present).

3. Add a test to crates/engine/tests/config_cost_prompts_test.rs named EXACTLY `prompts_state_contract_lives_in_approved_plan_json`. Follow the existing pattern of `worker_prompt_contains_worker_report_field_names` in that file (uses `prompts::text(Role::Worker)` etc., and `Role`/`prompts` are already imported). For BOTH Role::Worker and Role::Orchestrator, assert the prompt text, lowercased, contains each of: "plan.json", "plan.md", "operator", and "escalat". Give each assertion a clear failure message naming the role and the missing substring.

Protocol: write the test FIRST (it will fail until the paragraphs are added), then add the paragraphs until it passes. Then run `cargo test --workspace` and `cargo fmt --all --check` and fix anything your change broke. Do NOT edit any file other than the two prompt .md files and the one test file. Commit with prefix [<featureId>]. Paste the actual output of the test command and the workspace test run into testEvidence.

Done when:
- A test named exactly prompts_state_contract_lives_in_approved_plan_json exists in crates/engine/tests/config_cost_prompts_test.rs and passes: it asserts that both the Worker and Orchestrator prompt texts, lowercased, each contain the substrings 'plan.json', 'plan.md', 'operator', and 'escalat'.
- cargo test -p kranz-engine --test config_cost_prompts_test prompts_state_contract_lives_in_approved_plan_json -- --exact exits 0.
- cargo test --workspace exits 0 (existing hash and base-sha prompt tests still pass).
- cargo fmt --all --check exits 0.
- Only crates/engine/prompts/orchestrator.md, crates/engine/prompts/worker.md, and crates/engine/tests/config_cost_prompts_test.rs are modified: `git diff --name-only $KRANZ_BASE_SHA` lists exactly those three paths.

