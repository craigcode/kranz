# Kranz product introduction — working script

**Format:** roughly 2 minutes 30 seconds; voiceover over a real screen recording.
**Audience:** developers and engineering leads seeing Kranz for the first time.
**Working title:** *Kranz: from an agent task to a reviewed change.*
**Status:** first draft, reconciled against public `f550630` on 2026-09-14;
not yet rehearsed or recorded. The demo below is a proposed scenario, not a
claim that a mission has been run.

The video should answer one question: **How do I decide whether to trust an
agent's work?** Follow one small change through planning, approval, validation,
and delivery. Keep the same mission visible throughout. The agent writes the
code; Kranz governs the work and keeps the evidence.

## Demo story

Use a small, disposable Git repository with an existing human-readable report
command. The task is to add JSON output for automation while preserving the
existing output. This gives viewers a result they can recognize immediately
and a compatibility requirement worth checking.

Paste this into the **Goal** field in **New mission**:

> Add a --json option to the existing report command so scripts can consume
> its output. Return one valid JSON object containing the same information as
> the normal report. Preserve the existing default output. Cover populated
> and empty inputs with automated tests, and document the new option. Keep
> the change limited to reporting, its tests, and its usage documentation.

Before recording, replace “report command” with the demo project's actual
invocation and add its relevant file paths during planning. Agree on the JSON
field names there. Do not use Kranz's own command surface as an invented demo
fixture.

## Timed script

Read only the blockquotes. Screen directions and captions are production notes.
Timings include room for clicks and short holds; trim after the first read-through.

### 0:00–0:18 — The problem and the product

**Screen:** Open on the completed demo mission's report and visible
**UNMERGED** badge. Hold on the report, then briefly show the mission overview.
This is a preview of the result the rest of the video will explain.

**Caption:** `Kranz · Mission control for coding agents`

> When an AI coding agent says it's done, you still need to know what changed,
> what was checked, and whether to accept it. Kranz is local mission control
> for that work, built around your Git repository and coding agents.

### 0:18–0:38 — Give it an outcome

**Screen:** Cut back to the start. Open **New mission**, paste the prepared
goal, and click **Create mission**. Briefly show the planning conversation,
then **Request plan**. Cut out model waiting time.

**Caption:** `Describe the outcome`

> Here's a small task: add JSON output to a report command, while keeping the
> existing output intact. I describe the outcome. Kranz asks its planning
> agent to turn that into a mission, with a scope and checks for success.

### 0:38–1:02 — Review before execution

**Screen:** In **Plan review**, zoom into **Validation contract**. Highlight
the JSON behavior and default-output compatibility checks. Show milestones
and the cost estimate briefly. Click **Approve & commit plan**, hold on the
branch confirmation, then click **Start execution**.

**Caption:** `Review the plan. Approve the work.`

> Before implementation starts, I review the plan: what will change, how we'll
> check it, and the estimated cost. Here, valid JSON and unchanged default
> output both matter. I approve the plan, which records our agreement against
> a fixed Git starting point, then start execution.

### 1:02–1:22 — Watch the work

**Screen:** Show the worker session, the **Features** checklist, and the
**Usage** counter. Keep one feature readable rather than scrolling through
the transcript. Briefly point to the pause control and message composer.

**Caption:** `Isolated work · Visible progress`

> The coding agent implements the change in a dedicated Git worktree. Kranz
> tracks progress and usage, and I can pause or send guidance. The work stays
> separate from my main checkout while the mission runs.

### 1:22–1:47 — Show the evidence

**Screen:** Cut ahead to validation. Open an actual validator session and
hold on a concrete finding or verdict. Show the corresponding command result
from the recorded run. Pick readable evidence for one contract requirement.

**Caption:** `Checks and separate review`

> Kranz runs the agreed checks and brings in separate validator sessions.
> They review the results and the change without seeing the worker's
> reasoning. Findings can lead to repairs, and repair attempts are bounded.
> Here, I can inspect the evidence for the behavior we agreed to deliver.

### 1:47–2:13 — Delivery is a review decision

**Screen:** Return to the completed mission. Hold on **UNMERGED**, the diff
summary, and report. Show the small code diff in the editor if the summary
alone is insufficient. Click **Merge** in the disposable demo repository;
cut ahead only after its gates pass, then show the **landed** pipeline row.

**Caption:** `Review the result. Gate the merge.`

> When the mission finishes, I get a branch, a diff, and a report. I review
> them before choosing Merge. Kranz runs the repository's merge checks
> against the exact combined change before advancing the local base branch.
> Publishing stays with me: Kranz never pushes from this local workflow.

### 2:13–2:30 — Close on the result

**Screen:** Show the demo report's default output and its new JSON output
side by side. Return to the mission report for the closing line, then hold
the end card for two seconds.

**Caption:** `Plan → Approve → Validate → Review`

> The feature works, and the mission records how it got here: the plan,
> approvals, checks, and outcome. That's Kranz: coding agents do the work,
> and you have the controls and evidence to decide what to accept.

**End card:** `Kranz — Try one small mission in your own repo.`
Add the confirmed public project URL when preparing the final cut.

## Recording setup

1. Prepare a disposable demo repository with a working report command,
   representative populated and empty inputs, and a passing baseline suite.
   Use the supported Claude backend configuration for this first recording;
   keep worktree isolation and both validator roles enabled.
2. Initialize Kranz in that demo repository with `kranz init`. Inspect the
   generated `.kranz/merge-gates.json` and ensure its commands actually test
   this project. Commit the baseline and tracked Kranz policy before the
   mission so the merge gate can read the policy from the base branch.
3. Run `kranz ready`, resolve any readiness findings, then run
   `kranz serve --open`. Finish authentication before capturing the screen.
   Set a modest mission budget and rehearse the complete flow once.
4. In the rehearsal, confirm that the contract tests exercise the report's
   real entry point and fail for missing or incorrect JSON behavior. A test
   invocation that selects zero tests is insufficient. Preserve a default
   output comparison as well as the JSON checks.
5. Record the complete mission, then edit it down. Capture the plan,
   approval, worker activity, validation evidence, unmerged result, local
   merge, and actual output. Save the completed-result shot for the opening.
   Verify the new behavior before using the closing line “The feature works.”

Use 1080p capture with text large enough to read at normal playback. Keep the
cursor still during explanations. Use a brief “Later in the same mission”
caption when skipping execution or gate time. Retain the recorded usage and
elapsed-time values; the video's length is not the mission's runtime.

The main script uses the interactive **New mission** route because it shows
plan approval and execution as two explicit actions. Ticket drafting and
**Queue for run** are another workflow; save that explanation for a longer
tutorial. Planning itself uses an agent and can incur cost before execution.

If the demo produces a useful repair finding, spend five seconds of the
validation scene on that finding and its later resolution. If it passes on
the first attempt, show the actual passing evidence. A separate, rehearsed
blocked mission can illustrate escalation in a follow-up video; do not splice
it in as though it happened in this mission.

Keep installation, the full backend catalog, Slack, Flight Rules, analytics,
crash recovery, and sandbox configuration out of this first cut. Each is a
candidate for its own follow-up. Claims about review in this script mean
separate sessions; different model families are an additional configuration,
not something this recording should imply automatically.

## Implementation references for rehearsal

- Product framing: [positioning ADR](knowledge/decisions/positioning-governance-evidence-layer.md).
- Setup and backend caveats: [README](../README.md).
- Actual goal and planning controls:
  [NewMission.tsx](../apps/dashboard/src/components/NewMission.tsx),
  [PlanningView.tsx](../apps/dashboard/src/components/PlanningView.tsx), and
  [PlanReview.tsx](../apps/dashboard/src/components/PlanReview.tsx).
- Delivery controls and local-only handoff:
  [DeliveredPanel.tsx](../apps/dashboard/src/components/DeliveredPanel.tsx).
- Meaning of the local merge and its checks: [merge gates](merge-gates.md).
