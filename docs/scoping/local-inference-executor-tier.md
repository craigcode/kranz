# Kranz Workstream: Local Inference Executor Tier

**Status:** Proposed
**Depends on:** Existing orchestrator/worker/validator topology, event-sourced state
**Suggested ticket series:** KRZ-201+ (keeps clear of the KRZ-101–109 Gas City interop sequence)

---

## 1. Rationale

Kranz currently assumes frontier-model workers. This workstream adds a local
inference tier so that execution-class subtasks route to models running on
owned hardware, with the frontier tier reserved for planning, ambiguity, and
escalation.

Motivations, in priority order:

1. **Volume and parallelism.** Kranz's premise is parallel batch workers
   running continuously — a quota-saturating, latency-tolerant workload.
   Local executors remove the per-token ceiling on how many workers can run.
2. **Trace-to-fine-tune flywheel.** The event store already captures full
   task traces. Adding model identity and validation outcomes to those
   events turns operational logs into a fine-tuning dataset for free.
3. **Sovereignty and pinning.** Local weights are version-pinned by
   definition — no silent model updates under a mission.
4. **Learning the layer.** First-hand calibration of what local models can
   and cannot do, rather than taking conference-panel claims on trust.

### Known counter-evidence (design constraint, not a blocker)

Public benchmarking (AkitaOnRails, Apr 2026) found that a frontier-planner /
cheap-executor split **lost to solo Opus in a mature harness** on interactive
coding tasks — on both quality and cost — because the marginal cost of a
frontier call under a Max subscription is zero. The carve-out: mixed setups
win once subscription quota is saturated.

**Design consequence:** local executors are justified by *throughput beyond
quota* and *the flywheel*, not by replacing Opus where Opus is free at the
margin. Do not route interactive/ambiguous work locally to save money; route
batch, bounded, well-specified work locally to scale past the quota ceiling.

---

## 2. Architecture principles

- **Planner–executor split.** Flight director (planning, decomposition,
  ambiguity resolution) stays on Opus. Execution-class tickets (test
  writing, mechanical refactors, doc generation, bounded diffs) route local.
  Validator runs local for mechanical checks with frontier spot-checks on a
  sampled percentage.
- **Deterministic routing first.** Route on task-class tags, not learned
  classifiers. Learned/LLM routing is a later optimisation and needs the
  trace data anyway.
- **Escalation as the safety valve.** Local executor fails validation twice
  → ticket escalates to frontier tier. The escalation rate is the live
  metric for local-model competence.
- **One backend abstraction.** A trait over "OpenAI-compatible endpoint"
  with per-role config (base URL, model, context budget, temperature).
  Anthropic API and localhost become two impls of the same trait.
- **Traces carry provenance.** Every completion event records model ID,
  quantisation, prompt, output, and validation outcome.

---

## 3. Stack decisions

| Layer | Choice | Rationale |
|---|---|---|
| Inference engine | **mistral.rs** (EricLBuehler/mistral.rs) | Pure Rust on Candle; competitive with llama.cpp; OpenAI-compatible *and* Anthropic-compatible (`/v1/messages`) serving; hardware-aware `mistralrs tune`; embeddable via the `mistralrs` crate so workers can carry inference in-process later. |
| Bootstrap engine (Stage 0–2 only) | LM Studio or Ollama | Fastest path to a first endpoint on Windows; disposable once mistral.rs is proven. |
| Throughput graduation | vLLM under WSL2 | Only when multiple workers hit one endpoint concurrently (continuous batching). Not needed for single-consumer serving. |
| Backend abstraction | Own trait, **cribbing Rig's design** (0xPlaygrounds/rig) | Rig's `CompletionModel` trait hierarchy is the reference pattern; Kranz keeps its own thinner abstraction rather than adopting the framework. |
| Routing | Own, deterministic | Thin task-class → tier mapping in Kranz config. Reference designs: vLLM Semantic Router (where learned routing is heading, incl. multi-step agent workflows), LiteLLM (gateway patterns), llm-use (planner+workers+synthesis in miniature). |
| Executor models (candidates) | Qwen3-Coder-30B-A3B; Devstral Small | MoE 30B-A3B class is the consumer-hardware sweet spot (30B memory, ~3B-active speed). Final pick via Stage 3 eval, not benchmarks. |
| Fine-tuning (Stage 5) | Unsloth (LoRA); distilabel for LLM-as-judge curation | Fits 24GB-class hardware for models this size. |

---

## 4. Stages and proposed tickets

### KRZ-201 — Ground zero: first local endpoint
Install LM Studio or Ollama; run a Qwen3 model sized to available VRAM;
enable the local OpenAI-compatible server.
**Exit:** a Rust program obtains a completion from local hardware; one task
class the model handles well and one it handles badly are written down.

### KRZ-202 — Hardware audit and knob calibration
Document GPU/VRAM; verify predicted fit of model+quant+context combinations
(GGUF Q4_K_M baseline; KV-cache cost at agentic context lengths; measured
tok/s). Decide whether existing hardware suffices or escalation (used 3090
24GB / 4090 / 5090 / Spark tier) is warranted.
**Note:** AWS WorkSpaces is excluded (no nested virtualisation) — home-box
project.
**Exit:** hardware decision recorded; fit predictions verified against
reality.

### KRZ-203 — Stand up mistral.rs
Replace the bootstrap engine with `mistralrs serve`; run `mistralrs tune`;
confirm both OpenAI- and Anthropic-compatible endpoints respond.
**Exit:** mistral.rs serving the Stage-0 model with equal or better tok/s.

### KRZ-204 — Executor model eval (own tasks, not benchmarks)
Pull 3–4 real execution-class subtasks from the backlog (tests, mechanical
refactor, doc generation, diff review). Run each through Qwen3-Coder-30B-A3B,
Devstral Small, and one larger candidate if VRAM allows. Score manually.
**Exit:** shortlist of 1–2 models with known task-class competence,
documented.

### KRZ-205 — Backend trait + per-role config
Implement the completion-backend trait; two impls (Anthropic API, local
mistral.rs); per-worker-role config (base URL, model, context budget,
temperature).
**Exit:** any worker role is retargetable between tiers by config alone.

### KRZ-206 — Deterministic router + escalation
Task-class tags on backlog items map to tiers. Two failed validations →
escalate to frontier. Escalation rate surfaced on the dashboard.
**Exit:** routing and escalation observable per mission.

### KRZ-207 — Trace provenance
Extend completion events with model ID, quant, and validation outcome; add
an export path (validated traces → instruction-pair format).
**Exit:** a query over the event store yields a fine-tuning-ready dataset of
accepted traces.

### KRZ-208 — First end-to-end local mission
One real backlog ticket completed entirely by a local executor and passed by
the validator, with escalation armed.
**Exit:** the ticket ships; escalation rate and tok/s recorded as baseline.

### KRZ-209 — (Stretch, months out) Close the flywheel
At a few hundred validated traces: curate with LLM-as-judge (distilabel),
LoRA-tune the executor (Unsloth), re-run the KRZ-204 eval against the tuned
checkpoint.
**Exit:** measured delta (positive or null — both are publishable findings).
**Do not start early.**

---

## 5. Open questions

1. **GPU spec** — unresolved; gates KRZ-202 model choices and the
   buy/no-buy decision.
2. **In-process vs sidecar inference** — the `mistralrs` crate allows
   embedding; start sidecar (simpler ops, matches the trait abstraction),
   revisit if per-worker isolation or startup latency becomes a concern.
3. **Frontier spot-check sampling rate for the validator** — start at
   10–20% and tune against observed local-validator miss rate.
4. **Gas City interop implications** — whether the local tier should be
   exposed through the KRZ-101–109 adapter surface or kept internal to
   Kranz. Defer until KRZ-206 lands.

## 6. Risks

- **Local-model competence overestimated.** Mitigated by KRZ-204 eval on own
  tasks and the escalation valve; the escalation rate makes the risk
  measurable rather than assumed.
- **KV-cache blowout on agentic contexts.** Long worker contexts are the
  hidden VRAM cost; enforce per-role context budgets from KRZ-205.
- **Open-weight supply risk.** The whole tier depends on labs continuing to
  release weights (Qwen, GLM, Mistral, Nemotron). Pinned local copies of
  chosen models mitigate rug-pull but not stagnation.
- **Fine-tuning-as-memory is speculative.** KRZ-209 is framed as an
  experiment with a null result explicitly acceptable.

---

## 7. Review addendum (2026-07-11)

Mission-fit review against the current codebase. The spine is endorsed; six
adjustments, captured as the `.kranz/tickets/local-inference-*` set.

1. **KRZ-205 is mostly built, not greenfield.** `AgentBackend`
   (crates/engine/src/backend.rs) already has four impls and a `BackendKind`
   enum with per-role `parse_backend` / `role_default_model` / `model_tier`
   floors; per-role backend selection shipped through dashboard/Slack/REST in
   7c1b130. Local = add `BackendKind::Local` + a `backend_local` impl +
   base-url/context fields on the existing `RoleConfig`, slotting local models
   into the existing `model_tier` catalog (or an explicit floor exemption).

2. **`backend_local` is the first HTTP-in-engine backend.** Every existing
   backend spawns a CLI and streams stream-json; mistral.rs serves HTTP, so
   the engine holds the completion loop. This shape (a) sidesteps the worker
   sandbox — an HTTP call made by the engine isn't the sandboxed subprocess,
   so no `localhost:port` egress-allowlist entry is needed on the `fs+net`
   tier — and (b) revives the parked `api-only-harness-tier` idea, now with a
   mission-justified reason (throughput past quota), sharing the no-CLI,
   event-log-recorded machinery. Prefer HTTP-in-engine over a local-CLI wrap.

3. **Pull the flywheel (KRZ-207) to the front, decoupled from the GPU bet.**
   Trace provenance pays off even if no task ever routes local: it sharpens
   the record pillar and yields a standalone validated-trace dataset built on
   *frontier* traces first. Depends on none of KRZ-201–206. Record a content
   hash of the weight file + quant (not just a model string) so the
   "version-pinned by definition" sovereignty claim is auditable; make the
   export derived-and-regenerable from the event log like report.md.

4. **Split the local validator out and gate it harder.** Routing the executor
   local is low-risk (the validator still catches it; escalation makes
   competence measurable). Routing the *validator* local directly attacks "no
   silent green": a weak local validator that wrongly PASSES bad work is not a
   failure and the escalation valve never fires. Keep local validation to
   deterministic mechanical checks; scrutiny/judgment stays frontier, and a
   local pass on a contract command gets a frontier confirm regardless of the
   10–20% spot-check.

5. **Local-run cost accounting.** cost.rs prices per-token; a local run is ~$0
   marginal and would pollute the small, hard-won calibration corpus if mixed
   with paid missions. Flag local runs distinctly (local: $0 marginal, plus
   optional amortized hardware/watt).

6. **Roadmap placement: flywheel is "Now," the executor tier is trigger-gated.**
   Per the doc's own counter-evidence, a mixed setup loses to solo Opus until
   frontier quota — not model competence — is the binding constraint. Gate
   KRZ-201+ on a stretch of missions where that is true.

Stack claims (mistral.rs OpenAI/Anthropic serving + `mistralrs tune` +
embeddable crate, Rig, vLLM Semantic Router, Unsloth, distilabel, the specific
model names) are the proposal's to verify at KRZ-201/203/204; this review does
not independently vouch for them. The "own tasks, not benchmarks" eval posture
is the right way to settle them.
