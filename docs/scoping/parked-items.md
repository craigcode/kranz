# Scoping: the three long-parked items (2026-07-04)

Destination: docs/scoping/ in the kranz repo at the next merge window.
Each section is sized to become a mission brief when wanted; none is
recommended for the current cycle.

## OTEL (observability export)

**What it would be.** An opt-in OTLP exporter translating kranz's event log
into spans: mission = trace, milestone/feature/worker-run = nested spans,
cost/tokens as attributes. The event log is already the single source of
truth with stable seq + timestamps, so this is a read-side adapter (tail
events.jsonl → spans), NOT engine instrumentation — zero risk to the
durability path.

**Shape.** A `kranz otel` sidecar (or serve flag) using
opentelemetry-otlp; map events → span lifecycle; config = endpoint + service
name. ~1 mission (M-sized). The only design decision that matters: spans
from the LOG (replayable, lossless, no engine changes) vs live hooks
(lower latency, invasive). Take the log.

**When it earns its keep.** Multiple kranz instances / cloud deploy (M6
Railway) feeding Grafana-or-similar. Solo-local: the dashboard already
answers every question OTEL would. Verdict: build at first cloud deploy,
not before.

## Computer-use QA (functional validator with a browser/GUI)

**What it would be.** A functional-validator mode that drives the built
app (web UI via browser automation; TUI via pty) to verify acceptance
criteria live, instead of judging from code + tests.

**Shape.** The validator-functional role already exists as a session with
its own prompt; the gap is TOOLING (browser/pty access) + PERMISSIONS
(today's validator allow-lists are command-prefix based). Mission-sized
once the agent backend supports granting a browser tool to one role —
which lands naturally with SessionSpec.tools (mission 4 of this cycle).
Sequencing: after mission 4, this becomes "give validator-functional a
browser profile + prompt guidance," roughly one mission. High value for
dashboard-touching missions; useless for engine work. Verdict: next cycle,
gated on mission 4's plumbing.

## Skill capture (missions that learn)

**What it would be.** Completed missions distill reusable knowledge —
"how this repo's release works," "the flaky test patterns" — into files
the NEXT mission's orchestrator reads (a skills/ or lessons/ dir injected
into seed context).

**Shape.** Two halves: capture (a post-completion orchestrator turn:
"write the one lesson a future mission needs, or nothing") and injection
(seed prompt includes lessons/ index). The risk is noise accretion —
lessons need a cap + pruning policy or they become prompt sludge.
Evidence for value already exists: m-c9c915's orchestrator read
m-660ffc's report and designed away the diff-race — that was skill
capture happening ACCIDENTALLY via report.md. Formalizing = one mission
(capture turn + injection + cap policy + tests).

**When.** After a few more real missions accumulate patterns worth
capturing. The cheap first step (zero code): keep writing sharp mission
reports — they are already read.
