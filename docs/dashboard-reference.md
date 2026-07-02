# Dashboard reference — Factory.ai Mission Control (screenshot, 2026-07-02)

The user supplied a screenshot of the real Factory Mission Control UI. The Kranz
dashboard clones this layout. Every element maps to data the engine already
emits — no new engine capabilities are required.

## Layout (four regions)

```
┌──────────┬──────────────────────────────────────────────┬───────────────────┐
│          │ TOP BAR: mission path + title │ TIME │ PROGRESS │ USAGE          │
│ LEFT     ├──────────────────────────────────────────────┴───────────────────┤
│ SIDEBAR  │ STATUS STRIP: ● PAUSED ▓▓▓▓▓░░░░░ [▶ resume]                     │
│          ├──────────────────────────────────────────────┬───────────────────┤
│ Sessions │ CENTRE: orchestrator conversation            │ RIGHT COLUMN      │
│ ─ Orch.  │  - user msgs + rich markdown replies         │ ┌ Model panel     │
│ ─ Worker │  - scrollback                                │ ├ Features panel  │
│   #16..N │ ┌──────────────────────────────────────────┐ │ └ Progress Log    │
│ (age)    │ │ Type your message…                       │ │                   │
│          │ │ [+] [model pill] [effort] [Mode] [MCP]   │ │                   │
└──────────┴─┴──────────────────────────────────────────┴─┴───────────────────┘
```

## Element → Kranz data mapping

| Screenshot element | Kranz source |
|---|---|
| Left sidebar session list: "Orchestrator", "Worker #16: M1 test foundation", age ("10d") | `worker.spawned` events / `state.runs` (role, feature title, started_at). Click → transcript viewer rendered from `runs/<runId>.jsonl` (assistant text, tool calls collapsed, results). Pass/fail badge from `run.result`; "denied" badge if any denied `worker.message`. |
| Top bar: repo path + mission title | `state.mission` (repo root, goal) |
| TIME (elapsed 117:42:06) | now − `mission.created_at`, minus paused spans (from `mission.paused`/`resumed` events) — or plain wall-clock elapsed v1 |
| PROGRESS 13/44 | completed features / total features from `state` |
| USAGE 18% | Factory shows subscription usage; Kranz shows live token/cost counter (`state.totals`, `total_cost_usd`) and, when a mission budget is configured, % of budget |
| PAUSED strip + progress bar + ▶ | `mission.status`; ▶ enqueues `ControlCommand::Resume`; bar = feature progress |
| Centre conversation | orchestrator transcript: `user.message` events + orchestrator turns (from orch run transcript / `worker.message` events with run "orch-*"); markdown-rendered |
| Message box + Mission Mode / interrupt toggle | enqueues `ControlCommand::Msg { text, interrupt }` |
| Model panel: per-role model pill + effort pips | `state.config` role configs; edits enqueue `ControlCommand::ConfigChange` (→ `config.changed`, applies next spawn — matches plan §4.1) |
| Features panel: "13/44", milestone header, ✓ checklist | `state.mission.milestones[].features[]` (status icons: ✓ complete, ✗ failed, ↻ active pulsing, — skipped); fix-features visually distinct (`origin: "fix"`); validator entries appear when `milestone.validating` |
| Progress Log (right, bottom): "Worker #15 completed…", "Mission paused", "Worker #15 failed: Unrecoverable 402…", ages | human-readable rendering of the event feed (every event type has a one-line renderer); relative timestamps |

## Differences vs Factory (deliberate, v1)

- Validator model shows Claude models only (no GPT-5.3-Codex — multi-provider
  is out of scope; the config schema doesn't preclude it later).
- "USAGE %" is cost-vs-budget (we meter dollars/tokens, not subscription quota).
- Blocked milestones must be impossible to miss (plan §4.5): the status strip
  turns red with the reason and a jump-to-unblock affordance — Factory's
  paused strip generalizes to `paused | blocked | validating | complete | failed`.

## Style notes from the screenshot

Light theme, warm off-white panels, thin separators, small-caps section labels
(TIME/PROGRESS/USAGE), orange/terracotta accent for status dots, effort pips,
and checkmarks; monospace for ids/paths; density comparable to an IDE sidebar.
Keep it clean and data-dense; no decorative chrome.
