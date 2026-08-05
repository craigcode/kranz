//! Mission tickets — `.kranz/tickets/<slug>.md` (design: docs/backlog-and-slack.md).
//!
//! A ticket is a mission-in-waiting authored by a human as markdown: a small
//! `---` frontmatter block (parsed here without a YAML dependency) plus body
//! sections (`## Goal`, `## Context`, `## Scoping answers`, `## Acceptance
//! hints`). The `.md` stays human-authored; mutable pipeline status lives in a
//! sibling `<slug>.status` JSON file so the ticket text is never rewritten by
//! the engine (except the explicit "needs context" append the orchestrator
//! makes, and the committed lifecycle state below).
//!
//! ## Committed lifecycle state (design: ticket-state-frontmatter)
//!
//! The `.status` sidecar is gitignored runtime: on a fresh clone it vanishes,
//! and with it any operator verdict like done/superseded — a closed ticket
//! would silently re-enter the ready path. The durable home for that verdict
//! is the ticket .md itself: an optional additive `state:` frontmatter key
//! ([`TicketLifecycle`]; `open` default, plus terminal `done`, `superseded`,
//! `wontfix`) with an optional free-text `state-note:`. It is the SINGLE
//! SOURCE OF TRUTH: reads resolve with frontmatter precedence (a diverging
//! sidecar cache is logged, never silently followed), and the one lifecycle
//! write path — [`Ticket::write_lifecycle`] — writes BOTH, demoting the
//! sidecar to a write-through cache so existing readers keep working. A
//! ticket with NO `state:` key reads its sidecar exactly as before this
//! schema existed (backcompat).
//!
//! [`Ticket::mission_goal`] folds the whole ticket into one readable markdown
//! blob so the non-interactive draft driver can seed the orchestrator with the
//! entire ticket in a single message.

use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default priority when frontmatter omits it (1 high … 3 low).
const DEFAULT_PRIORITY: u8 = 2;

/// Heading [`Ticket::mission_goal`] appends before a set task class, and
/// [`parse_task_class_from_goal`] looks for on the way back out.
const TASK_CLASS_HEADING: &str = "## Task class\n";

/// Recover the task class [`Ticket::mission_goal`] folded in, from a mission
/// `goal` string. [`crate::orchestrator::MissionEngine::create`] calls this
/// to route the executor tier for a mission seeded from a ticket, since by
/// the time `create` runs it only has the folded goal, not the `Ticket`.
pub fn parse_task_class_from_goal(goal: &str) -> Option<String> {
    let idx = goal.find(TASK_CLASS_HEADING)?;
    let rest = &goal[idx + TASK_CLASS_HEADING.len()..];
    let line = rest.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// How often a ticket re-instantiates. Recurring schedules are re-drafted by
/// the scheduler; `Once` is the default one-shot ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Schedule {
    #[default]
    Once,
    Nightly,
    Weekly,
}

impl Schedule {
    /// Parse case-insensitively; an unknown value maps to [`Schedule::Once`]
    /// with a warning (a typo should not silently drop the whole ticket).
    fn parse(raw: &str) -> Schedule {
        match raw.trim().to_ascii_lowercase().as_str() {
            "once" => Schedule::Once,
            "nightly" => Schedule::Nightly,
            "weekly" => Schedule::Weekly,
            other => {
                tracing::warn!(schedule = %other, "unknown ticket schedule; defaulting to once");
                Schedule::Once
            }
        }
    }
}

/// The committed, operator-declared lifecycle state of a ticket — the
/// optional `state:` frontmatter key (design: ticket-state-frontmatter).
/// Unlike the pipeline [`TicketState`] (which the engine flips as a ticket
/// moves draft → review → queue → run), this is the human's terminal verdict,
/// and it lives IN the committed .md so it survives a fresh clone. Terminal
/// values (`done`/`superseded`/`wontfix`) exclude the ticket from ready/queue
/// evaluation exactly like terminal pipeline states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TicketLifecycle {
    /// Not operator-closed; the sidecar pipeline state governs. This is also
    /// the meaning of an ABSENT `state:` key (backcompat with tickets
    /// authored before the schema existed).
    #[default]
    Open,
    Done,
    Superseded,
    Wontfix,
}

impl TicketLifecycle {
    /// The frontmatter spelling (lower-case, matching the other keys).
    pub fn as_str(self) -> &'static str {
        match self {
            TicketLifecycle::Open => "open",
            TicketLifecycle::Done => "done",
            TicketLifecycle::Superseded => "superseded",
            TicketLifecycle::Wontfix => "wontfix",
        }
    }

    /// Parse a `state:` value. An unknown value is a hard error naming the
    /// ticket (the same rule as `defer-until`, and for the mirror-image
    /// reason): silently defaulting a mistyped terminal state back to open
    /// would re-queue work its author explicitly closed — the very bug this
    /// schema exists to fix.
    fn parse(slug: &str, raw: &str) -> Result<TicketLifecycle> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" => Ok(TicketLifecycle::Open),
            "done" => Ok(TicketLifecycle::Done),
            "superseded" => Ok(TicketLifecycle::Superseded),
            "wontfix" => Ok(TicketLifecycle::Wontfix),
            other => Err(EngineError::Config(format!(
                "ticket {slug}: invalid state '{other}' (expected open, done, \
                 superseded, or wontfix)"
            ))),
        }
    }

    /// The pipeline projection of a terminal lifecycle state, or `None` for
    /// [`TicketLifecycle::Open`]: an open ticket makes no lifecycle claim on
    /// the pipeline, so the sidecar state governs it.
    fn terminal_pipeline_state(self) -> Option<TicketState> {
        match self {
            TicketLifecycle::Open => None,
            TicketLifecycle::Done => Some(TicketState::Done),
            TicketLifecycle::Superseded => Some(TicketState::Superseded),
            TicketLifecycle::Wontfix => Some(TicketState::Wontfix),
        }
    }
}

/// A parsed ticket: frontmatter fields plus body sections.
#[derive(Debug, Clone, PartialEq)]
pub struct Ticket {
    pub slug: String,
    pub title: String,
    pub priority: u8,
    pub repo_refs: Vec<String>,
    pub schedule: Schedule,
    pub max_budget_usd: Option<f64>,
    pub goal: String,
    pub context: String,
    pub scoping_answers: Vec<String>,
    pub acceptance_hints: Vec<String>,
    /// Slugs of tickets that must reach a Complete mission before this one
    /// can be approved (`blocked-by: [a, b]` frontmatter).
    pub blocked_by: Vec<String>,
    /// Backlog task class (`task-class: execution-class` frontmatter), used
    /// to route the executor to a tier via [`crate::config::task_class_to_tier`].
    pub task_class: Option<String>,
    /// External trigger provenance (`trigger: ci-failure|pr-comment`
    /// frontmatter) — set on webhook-drafted tickets (design D-F,
    /// [`crate::hooks`]); `None` on human-authored tickets.
    pub trigger: Option<String>,
    /// Defect→mission link (`traced-from-mission: m-xxxx` frontmatter) — the
    /// ONE data addition of the flight-surgeon console (ticket
    /// `flight-surgeon-dashboard`): a defect ticket traces back to the mission
    /// that shipped it. Seeded by `kranz draft --from-mission` or added by
    /// hand; absent means "not a traced defect" (no false positives).
    pub traced_from_mission: Option<String>,
    /// Deferral (`defer-until: <RFC 3339>` frontmatter, D-BW-3 adopted from
    /// beads): present but NOT ready until the timestamp passes. Evaluated
    /// against the clock at listing/admission time — no scheduler machinery;
    /// `None` means ready now.
    pub defer_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Operator-declared lifecycle (`state:` frontmatter; see the module
    /// docs). `None` = the key is absent, so the `.status` sidecar governs
    /// exactly as before this schema existed (backcompat); `Some(Open)` = an
    /// explicit open, which defers to the sidecar the same way.
    pub lifecycle: Option<TicketLifecycle>,
    /// The free-text `state-note:` frontmatter carried alongside a lifecycle
    /// state (e.g. "superseded by the flight-surgeon console"). Never parsed
    /// for meaning — notes are discussion, not a second state channel.
    pub state_note: Option<String>,
    /// The full markdown body (everything after the frontmatter block).
    pub raw_body: String,
}

/// Pipeline status of a ticket, stored in `<slug>.status` (never time-based —
/// determinism matters for the event-sourced engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TicketState {
    #[default]
    New,
    Drafting,
    NeedsContext,
    /// The planner escalated at draft time: it CAN plan, but believes the
    /// plan is likely wrong (goal misframed, premise broken). Parks like
    /// [`NeedsContext`] — re-draftable, never schedulable/queueable — but is
    /// distinct from it everywhere the state surfaces.
    WrongPlan,
    Review,
    Queued,
    Running,
    Done,
    Failed,
    /// Claimed then removed from the queue because backend readiness failed
    /// (missing binary, unauthenticated, unsupported config). Distinct from
    /// [`Failed`] so operators can re-queue after fixing the environment
    /// without treating the mission run itself as a failure.
    Parked,
    /// Operator-closed without delivery (`state: superseded` frontmatter —
    /// the work moved elsewhere). Reached only through frontmatter
    /// precedence ([`Ticket::read_state`]) or the lifecycle write path;
    /// terminal everywhere [`Done`] is. Additive serde variant: sidecars
    /// written before it existed never spelled it.
    Superseded,
    /// Operator-closed as not-worth-doing (`state: wontfix` frontmatter).
    /// Same reachability and terminality as [`Superseded`].
    Wontfix,
}

/// On-disk shape of `<slug>.status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StatusFile {
    state: TicketState,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// The mission `kranz draft` created for this ticket — the durable
    /// ticket→mission link `kranz ticket approve <slug>` resolves by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mission_id: Option<String>,
}

/// One observed frontmatter/sidecar disagreement: the committed frontmatter
/// `state:` won over the `.status` sidecar cache. Surfaced (and
/// `tracing::warn!`-logged by [`Ticket::read_state`]) rather than silently
/// resolved — design rule 2 is "never a silent divergence".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDivergence {
    /// The winning state, projected from the frontmatter lifecycle.
    pub frontmatter: TicketState,
    /// The discarded sidecar cache state.
    pub sidecar: TicketState,
}

/// The outcome of resolving a ticket's effective state under frontmatter
/// precedence (see [`Ticket::resolve_state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTicketState {
    /// The state every ready/queue/list evaluation must use.
    pub state: TicketState,
    /// `Some` when a present sidecar disagreed with a terminal frontmatter
    /// state (the frontmatter won). `None` when they agree, when the
    /// frontmatter defers, or when the cache is simply cold.
    pub divergence: Option<StateDivergence>,
}

impl Ticket {
    /// Directory holding ticket markdown for a repo: `.kranz/tickets/`.
    pub fn tickets_dir(repo_root: &Path) -> PathBuf {
        repo_root.join(".kranz").join("tickets")
    }

    /// A slug is a bare file stem, never a path: reject separators, `..`,
    /// leading dots, and empties BEFORE any join — a slug like `../x` must
    /// not escape `.kranz/tickets/` (review P3).
    pub fn valid_slug(slug: &str) -> bool {
        !slug.is_empty()
            && slug.len() <= 128
            && !slug.starts_with('.')
            && !slug.contains("..")
            && slug
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    }

    /// [`valid_slug`](Self::valid_slug) as an error for write/scaffold paths.
    pub fn ensure_valid_slug(slug: &str) -> Result<()> {
        if Self::valid_slug(slug) {
            Ok(())
        } else {
            Err(EngineError::Config(format!(
                "invalid ticket slug '{slug}': use letters, digits, '-', '_' \
                 (no path separators, no leading dot, no '..')"
            )))
        }
    }

    /// The scaffolded body of a new ticket. Frontmatter carries the title;
    /// the body is the four sections the orchestrator expects (`## Goal`,
    /// `## Context`, `## Scoping answers`, `## Acceptance hints`), pre-seeded
    /// with the goal/context when supplied. The result parses back cleanly
    /// through [`Ticket::parse`].
    pub fn ticket_template(title: &str, goal: Option<&str>, context: Option<&str>) -> String {
        let title = crate::scrub::scrub(title.trim());
        let goal_body = goal
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(crate::scrub::scrub)
            .unwrap_or_default();
        let context_body = context
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(crate::scrub::scrub)
            .unwrap_or_default();
        format!(
            "---\n\
             title: {title}\n\
             priority: 2\n\
             schedule: once\n\
             ---\n\
             \n\
             ## Goal\n\
             {goal_body}\n\
             \n\
             ## Context\n\
             {context_body}\n\
             \n\
             ## Scoping answers\n\
             \n\
             ## Acceptance hints\n"
        )
    }

    /// Scaffold `.kranz/tickets/<slug>.md` from the template. Shared by the
    /// CLI (`kranz ticket new`) and the REST `POST /api/tickets` handler so
    /// the template lives in exactly one place. `EngineError::Config` for an
    /// invalid slug, `EngineError::InvalidState` (→ 409 over REST) if a
    /// ticket with that slug already exists. Returns the written path.
    pub fn scaffold(
        repo_root: &Path,
        slug: &str,
        title: &str,
        goal: Option<&str>,
        context: Option<&str>,
    ) -> Result<PathBuf> {
        Self::ensure_valid_slug(slug)?;
        let dir = Self::tickets_dir(repo_root);
        let path = dir.join(format!("{slug}.md"));
        if path.exists() {
            return Err(EngineError::InvalidState(format!(
                "ticket '{slug}' already exists at {}",
                path.display()
            )));
        }
        std::fs::create_dir_all(&dir)?;
        let body = Self::ticket_template(title, goal, context);
        Self::parse(slug, &body).map_err(|e| {
            EngineError::Other(format!(
                "internal error: scaffolded ticket does not parse: {e}"
            ))
        })?;
        std::fs::write(&path, body)?;
        Ok(path)
    }

    /// Parse ticket markdown. `slug` is supplied by the caller (usually the
    /// file stem). Malformed frontmatter is an [`EngineError::Config`].
    pub fn parse(slug: &str, markdown: &str) -> Result<Ticket> {
        let (front, body) = split_frontmatter(slug, markdown)?;

        let mut title: Option<String> = None;
        let mut priority = DEFAULT_PRIORITY;
        let mut repo_refs: Vec<String> = Vec::new();
        let mut schedule = Schedule::Once;
        let mut max_budget_usd: Option<f64> = None;
        let mut blocked_by: Vec<String> = Vec::new();
        let mut task_class: Option<String> = None;
        let mut trigger: Option<String> = None;
        let mut traced_from_mission: Option<String> = None;
        let mut defer_until: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut lifecycle: Option<TicketLifecycle> = None;
        let mut state_note: Option<String> = None;

        for (key, value) in front {
            match key.as_str() {
                "title" => title = Some(value.scalar()),
                "priority" => {
                    if let Ok(p) = value.scalar().parse::<u8>() {
                        priority = p;
                    } else {
                        tracing::warn!(slug, value = %value.scalar(), "invalid ticket priority; keeping default");
                    }
                }
                // Both spellings — frontmatter is kebab-case per the design doc,
                // but tolerate the camelCase a hand-editor might type.
                "repo-refs" | "reporefs" => repo_refs = value.list(),
                "blocked-by" | "blockedby" => blocked_by = value.list(),
                "task-class" | "taskclass" => {
                    let v = value.scalar().trim().to_string();
                    task_class = if v.is_empty() { None } else { Some(v) };
                }
                // External trigger provenance (design D-F); additive — older
                // readers ignore it via the unknown-key arm below.
                "trigger" => {
                    let v = value.scalar().trim().to_string();
                    trigger = if v.is_empty() { None } else { Some(v) };
                }
                // Defect→mission link (flight-surgeon console); additive —
                // older readers ignore it via the unknown-key arm below.
                "traced-from-mission" | "tracedfrommission" => {
                    let v = value.scalar().trim().to_string();
                    traced_from_mission = if v.is_empty() { None } else { Some(v) };
                }
                // Deferral (D-BW-3); additive. Unlike the warn-and-default
                // scalar fields, a malformed timestamp is a hard parse error
                // naming the ticket: silently dropping a deferral would queue
                // work its author explicitly parked.
                "defer-until" | "deferuntil" => {
                    let v = value.scalar().trim().to_string();
                    if !v.is_empty() {
                        defer_until = Some(parse_defer_until(slug, &v)?);
                    }
                }
                "schedule" => schedule = Schedule::parse(&value.scalar()),
                // The committed lifecycle state (design
                // ticket-state-frontmatter); additive — older readers ignore
                // it via the unknown-key arm below. An empty value is absent;
                // an unknown one is a hard parse error (see
                // [`TicketLifecycle::parse`]).
                "state" => {
                    let v = value.scalar().trim().to_string();
                    if !v.is_empty() {
                        lifecycle = Some(TicketLifecycle::parse(slug, &v)?);
                    }
                }
                // Free-text companion to `state:`; additive. Never parsed for
                // meaning — notes are discussion, not a state channel.
                "state-note" | "statenote" => {
                    let v = value.scalar().trim().to_string();
                    state_note = if v.is_empty() { None } else { Some(v) };
                }
                "maxbudgetusd" | "max-budget-usd" => {
                    if let Ok(b) = value.scalar().parse::<f64>() {
                        max_budget_usd = Some(b);
                    } else {
                        tracing::warn!(slug, value = %value.scalar(), "invalid ticket maxBudgetUsd; ignoring");
                    }
                }
                // Unknown keys are ignored (forward compatibility).
                _ => {}
            }
        }

        let sections = parse_sections(&body);

        let goal = sections.goal.unwrap_or_default();
        let context = sections.context.unwrap_or_default();

        // Title fallback chain: frontmatter → first heading → slug.
        let title = title
            .filter(|t| !t.trim().is_empty())
            .or(sections.first_heading)
            .unwrap_or_else(|| slug.to_string());

        Ok(Ticket {
            slug: slug.to_string(),
            title,
            priority,
            repo_refs,
            schedule,
            max_budget_usd,
            goal,
            context,
            scoping_answers: sections.scoping_answers,
            acceptance_hints: sections.acceptance_hints,
            blocked_by,
            task_class,
            trigger,
            traced_from_mission,
            defer_until,
            lifecycle,
            state_note,
            raw_body: body,
        })
    }

    /// Load and parse a ticket file; the slug is the file stem.
    pub fn load(path: &Path) -> Result<Ticket> {
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                EngineError::Config(format!("ticket path has no file stem: {}", path.display()))
            })?
            .to_string();
        let text = std::fs::read_to_string(path)?;
        Ticket::parse(&slug, &text)
    }

    /// Parse every `*.md` under `.kranz/tickets/`, skipping (with a warning) any
    /// file that fails to parse. Sorted by `(priority, slug)`.
    pub fn list(repo_root: &Path) -> Vec<Ticket> {
        let dir = Self::tickets_dir(repo_root);
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return out;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match Ticket::load(&path) {
                Ok(t) => out.push(t),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unparseable ticket");
                }
            }
        }
        out.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        out
    }

    /// Whether the ticket is ready at `now`: a `defer-until` timestamp in the
    /// future parks it (D-BW-3); anything else — absent, or past — is ready.
    /// Callers supply the clock so the check is explicit at each listing /
    /// admission site (there is no scheduler flipping state).
    pub fn is_ready_at(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.defer_until.is_none_or(|until| until <= now)
    }

    /// Fold the whole ticket into one readable-markdown message: the goal plus
    /// a compact appendix carrying scoping answers, acceptance hints, context,
    /// and (when set) the task class — enough for a draft driver to seed the
    /// orchestrator in one go, and the one channel that carries the task
    /// class into [`crate::orchestrator::MissionEngine::create`] (which
    /// recovers it via [`parse_task_class_from_goal`]) since every seed path —
    /// CLI, REST, Slack — creates the mission from this folded string, not
    /// the `Ticket` itself.
    pub fn mission_goal(&self) -> String {
        let mut out = String::new();
        if self.goal.trim().is_empty() {
            out.push_str(&self.title);
        } else {
            out.push_str(self.goal.trim());
        }

        if !self.scoping_answers.is_empty() {
            out.push_str("\n\n## Scoping answers\n");
            for item in &self.scoping_answers {
                out.push_str("- ");
                out.push_str(item);
                out.push('\n');
            }
        }

        if !self.acceptance_hints.is_empty() {
            out.push_str("\n## Acceptance hints\n");
            for item in &self.acceptance_hints {
                out.push_str("- ");
                out.push_str(item);
                out.push('\n');
            }
        }

        if !self.context.trim().is_empty() {
            out.push_str("\n## Context\n");
            out.push_str(self.context.trim());
            out.push('\n');
        }

        if let Some(task_class) = self.task_class.as_deref().map(str::trim) {
            if !task_class.is_empty() {
                out.push('\n');
                out.push_str(TASK_CLASS_HEADING);
                out.push_str(task_class);
                out.push('\n');
            }
        }

        out
    }

    // -- status file ------------------------------------------------------

    /// Path of the sibling status file for a slug.
    fn status_path(repo_root: &Path, slug: &str) -> PathBuf {
        Self::tickets_dir(repo_root).join(format!("{slug}.status"))
    }

    /// Path of the ticket markdown for a slug.
    pub(crate) fn md_path(repo_root: &Path, slug: &str) -> PathBuf {
        Self::tickets_dir(repo_root).join(format!("{slug}.md"))
    }

    /// The `state:` frontmatter lifecycle of a ticket, scanned without
    /// parsing body sections. `None` when the .md is missing, has no
    /// frontmatter block, or carries no `state:` key — all cases where the
    /// sidecar governs exactly as before the schema existed.
    fn frontmatter_lifecycle(repo_root: &Path, slug: &str) -> Option<TicketLifecycle> {
        let text = std::fs::read_to_string(Self::md_path(repo_root, slug)).ok()?;
        let (front, _) = split_frontmatter(slug, &text).ok()?;
        for (key, value) in front {
            if key != "state" {
                continue;
            }
            let v = value.scalar().trim().to_string();
            if v.is_empty() {
                return None;
            }
            return match TicketLifecycle::parse(slug, &v) {
                Ok(lifecycle) => Some(lifecycle),
                // [`Ticket::parse`] hard-errors on the same value, so the
                // ticket is already dropped (loudly) from every listing; the
                // read path stays total and lets the sidecar govern.
                Err(e) => {
                    tracing::warn!(slug, error = %e, "invalid frontmatter state; sidecar governs");
                    None
                }
            };
        }
        None
    }

    /// The sidecar's pipeline state, or `None` when no readable `.status`
    /// exists. Distinguishing absent from [`TicketState::New`] matters for
    /// divergence reporting: a cold cache (fresh clone) cannot disagree.
    fn sidecar_state(repo_root: &Path, slug: &str) -> Option<TicketState> {
        Self::read_status_file(repo_root, slug).map(|sf| sf.state)
    }

    /// Resolve the effective ticket state under FRONTMATTER PRECEDENCE
    /// (design ticket-state-frontmatter): a terminal `state:` key in the
    /// committed .md wins over the `.status` sidecar — the sidecar is a
    /// write-through cache, never the truth. A PRESENT sidecar that
    /// disagrees is reported as a [`StateDivergence`] (a missing sidecar is
    /// a cold cache, not a divergence). An absent key or an explicit `open`
    /// defers to the sidecar; no sidecar at all is [`TicketState::New`].
    pub fn resolve_state(repo_root: &Path, slug: &str) -> ResolvedTicketState {
        if !Self::valid_slug(slug) {
            return ResolvedTicketState {
                state: TicketState::New,
                divergence: None,
            };
        }
        let sidecar = Self::sidecar_state(repo_root, slug);
        let defer = |state: TicketState| ResolvedTicketState {
            state,
            divergence: None,
        };
        let Some(lifecycle) = Self::frontmatter_lifecycle(repo_root, slug) else {
            return defer(sidecar.unwrap_or(TicketState::New));
        };
        let Some(terminal) = lifecycle.terminal_pipeline_state() else {
            // Explicit `open`: no lifecycle claim — the pipeline governs.
            return defer(sidecar.unwrap_or(TicketState::New));
        };
        let divergence = match sidecar {
            Some(sidecar) if sidecar != terminal => Some(StateDivergence {
                frontmatter: terminal,
                sidecar,
            }),
            _ => None,
        };
        ResolvedTicketState {
            state: terminal,
            divergence,
        }
    }

    /// Read the effective pipeline state; a missing or unreadable status file
    /// is [`TicketState::New`], and an invalid slug never touches the
    /// filesystem. Frontmatter precedence per [`Self::resolve_state`]: a
    /// terminal `state:` key wins, and a diverging sidecar cache is logged —
    /// never a silent divergence (design rule 2).
    pub fn read_state(repo_root: &Path, slug: &str) -> TicketState {
        let resolved = Self::resolve_state(repo_root, slug);
        if let Some(divergence) = &resolved.divergence {
            tracing::warn!(
                slug,
                frontmatter = ?divergence.frontmatter,
                sidecar = ?divergence.sidecar,
                "ticket frontmatter state overrides diverging .status cache"
            );
        }
        resolved.state
    }

    /// Write the pipeline state (plus an optional note) as JSON. Nothing
    /// time-based is recorded, so the file is a pure function of its inputs.
    pub fn write_state(
        repo_root: &Path,
        slug: &str,
        state: TicketState,
        note: Option<String>,
    ) -> Result<()> {
        Self::ensure_valid_slug(slug)?;
        let dir = Self::tickets_dir(repo_root);
        std::fs::create_dir_all(&dir)?;
        // Preserve an existing mission link: state flips (Review→Queued→Done)
        // must never erase which mission the draft created.
        let mission_id = Self::read_status_file(repo_root, slug).and_then(|sf| sf.mission_id);
        let sf = StatusFile {
            state,
            note,
            mission_id,
        };
        let json = serde_json::to_string_pretty(&sf)?;
        atomic_write(&Self::status_path(repo_root, slug), json.as_bytes())?;
        Ok(())
    }

    fn read_status_file(repo_root: &Path, slug: &str) -> Option<StatusFile> {
        if !Self::valid_slug(slug) {
            return None;
        }
        let path = Self::status_path(repo_root, slug);
        // A missing file is the normal "never drafted/approved" case (no log);
        // a PRESENT but unparseable file is corruption worth surfacing, so the
        // reverse lookup and mission-link preservation don't fail silently.
        let text = std::fs::read_to_string(&path).ok()?;
        match serde_json::from_str(&text) {
            Ok(sf) => Some(sf),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "unreadable ticket status; ignoring");
                None
            }
        }
    }

    /// The raw sidecar record — pipeline state plus note — or `None` when no
    /// readable `.status` exists. Crate-internal: the state fold
    /// ([`crate::migrate_state`]) needs the note to carry it into the
    /// frontmatter `state-note:`, and only the fold should be reading sidecar
    /// notes at all (notes are never parsed for state).
    pub(crate) fn sidecar_record(
        repo_root: &Path,
        slug: &str,
    ) -> Option<(TicketState, Option<String>)> {
        Self::read_status_file(repo_root, slug).map(|sf| (sf.state, sf.note))
    }

    /// The ONE lifecycle write path (design ticket-state-frontmatter, rule 2:
    /// the `.status` sidecar is a write-through cache of the committed
    /// frontmatter `state:` — every lifecycle change writes BOTH, so no
    /// reader of either file can observe them apart, and a fresh clone loses
    /// only the cache, never the verdict).
    ///
    /// Upserts the `state:` (and `state-note:`, replacing or removing it)
    /// lines inside the ticket .md's frontmatter block — every other byte
    /// preserved — then mirrors the terminal pipeline projection into the
    /// sidecar via [`Self::write_state`].
    ///
    /// Terminal states only: `Open` is the ABSENCE of a terminal claim, so
    /// there is nothing to cache — un-close a ticket by removing the `state:`
    /// key and resetting the pipeline by hand. Pipeline transitions
    /// (Drafting/Review/Queued/…) keep using [`Self::write_state`], which
    /// never touches the committed .md.
    pub fn write_lifecycle(
        repo_root: &Path,
        slug: &str,
        state: TicketLifecycle,
        note: Option<String>,
    ) -> Result<()> {
        Self::ensure_valid_slug(slug)?;
        let terminal = state.terminal_pipeline_state().ok_or_else(|| {
            EngineError::Config(format!(
                "write_lifecycle takes a terminal state (done, superseded, wontfix); \
                 'open' is the absence of a `state:` key (ticket {slug})"
            ))
        })?;
        let note = note.map(|n| bound_state_note(&n)).filter(|n| !n.is_empty());
        let md = Self::md_path(repo_root, slug);
        let text = std::fs::read_to_string(&md)?;
        let updated = upsert_frontmatter_state(slug, &text, state, note.as_deref())?;
        atomic_write(&md, updated.as_bytes())?;
        Self::write_state(repo_root, slug, terminal, note)?;
        Ok(())
    }

    /// Durably link the ticket to the mission `kranz draft` created for it.
    /// `kranz ticket approve <slug>` resolves through this link — goal-text
    /// matching breaks the moment `plan.approved` rewrites the mission goal
    /// to the orchestrator's refined phrasing (observed live on the first
    /// drafted batch).
    pub fn record_mission(repo_root: &Path, slug: &str, mission_id: &str) -> Result<()> {
        Self::ensure_valid_slug(slug)?;
        let dir = Self::tickets_dir(repo_root);
        std::fs::create_dir_all(&dir)?;
        let (state, note) = match Self::read_status_file(repo_root, slug) {
            Some(sf) => (sf.state, sf.note),
            None => (TicketState::Drafting, None),
        };
        let sf = StatusFile {
            state,
            note,
            mission_id: Some(mission_id.to_string()),
        };
        let json = serde_json::to_string_pretty(&sf)?;
        atomic_write(&Self::status_path(repo_root, slug), json.as_bytes())?;
        Ok(())
    }

    /// The mission recorded by [`Self::record_mission`], if any.
    pub fn mission_for(repo_root: &Path, slug: &str) -> Option<String> {
        Self::read_status_file(repo_root, slug).and_then(|sf| sf.mission_id)
    }

    /// Reverse of [`Self::mission_for`]: find the ticket whose `.status`
    /// records this mission id. Used when a mission-id approve/queue path
    /// (e.g. Slack `/kranz approve m-…`) must still advance the linked
    /// ticket's pipeline state. Tickets without a mission link are skipped.
    ///
    /// Slugs are scanned in sorted order so the result is deterministic (the
    /// event-sourced engine must not depend on `read_dir` order) if two
    /// tickets ever record the same mission id — an unexpected state, so a
    /// duplicate link is also logged.
    pub fn slug_for_mission(repo_root: &Path, mission_id: &str) -> Option<String> {
        if mission_id.is_empty() {
            return None;
        }
        let dir = Self::tickets_dir(repo_root);
        let rd = std::fs::read_dir(&dir).ok()?;
        let mut slugs: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("status"))
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| Self::valid_slug(s))
                    .map(str::to_string)
            })
            .collect();
        slugs.sort();
        let mut found: Option<String> = None;
        for slug in slugs {
            if Self::mission_for(repo_root, &slug).as_deref() == Some(mission_id) {
                match &found {
                    None => found = Some(slug),
                    Some(first) => {
                        tracing::warn!(
                            mission_id,
                            resolved = %first,
                            duplicate = %slug,
                            "multiple tickets link one mission; using the first by sorted slug"
                        );
                    }
                }
            }
        }
        found
    }

    /// Append the orchestrator's verbatim clarifying questions to the ticket
    /// `.md` under a `## Needs context (from orchestrator)` heading, and set
    /// the state to [`TicketState::NeedsContext`].
    pub fn append_needs_context(repo_root: &Path, slug: &str, questions: &[String]) -> Result<()> {
        Self::ensure_valid_slug(slug)?;
        let md = Self::md_path(repo_root, slug);
        let mut text = std::fs::read_to_string(&md)?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("\n## Needs context (from orchestrator)\n");
        for q in bound_questions(questions) {
            text.push_str("- ");
            text.push_str(&crate::scrub::scrub(&q));
            text.push('\n');
        }
        atomic_write(&md, text.as_bytes())?;
        Self::write_state(repo_root, slug, TicketState::NeedsContext, None)?;
        Ok(())
    }

    /// Append the planner's wrong-plan escalation reason to the ticket `.md`
    /// under a `## Wrong plan (from orchestrator)` heading, and set the state
    /// to [`TicketState::WrongPlan`] with the `.status` note carrying the
    /// reason prefixed `WRONG-PLAN: `. Mirrors [`Self::append_needs_context`]'s
    /// shape (bounded, scrubbed, atomic).
    pub fn append_wrong_plan(repo_root: &Path, slug: &str, reason: &str) -> Result<()> {
        Self::ensure_valid_slug(slug)?;
        let reason = bound_reason(reason);
        let md = Self::md_path(repo_root, slug);
        let mut text = std::fs::read_to_string(&md)?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("\n## Wrong plan (from orchestrator)\n");
        text.push_str(&crate::scrub::scrub(&reason));
        text.push('\n');
        atomic_write(&md, text.as_bytes())?;
        Self::write_state(
            repo_root,
            slug,
            TicketState::WrongPlan,
            Some(format!("WRONG-PLAN: {reason}")),
        )?;
        Ok(())
    }

    /// Seed or update the ticket's `traced-from-mission` frontmatter link
    /// (the flight-surgeon console's defect→mission join). `kranz draft
    /// --from-mission` calls this; hand-edited tickets need nothing here —
    /// their field parses through [`Ticket::parse`] like any other. Only the
    /// frontmatter block is rewritten (an existing link line in place, or a
    /// new line right after the opening fence; a frontmatter-less ticket
    /// gains a two-line block above its body) — body bytes are preserved.
    /// Returns `Ok(true)` when the file changed, `Ok(false)` when the link
    /// already named this mission.
    pub fn seed_traced_from_mission(
        repo_root: &Path,
        slug: &str,
        mission_id: &str,
    ) -> Result<bool> {
        Self::ensure_valid_slug(slug)?;
        if !crate::paths::MissionPaths::is_safe_id(mission_id) {
            return Err(EngineError::Config(format!(
                "invalid mission id '{mission_id}' for traced-from-mission"
            )));
        }
        let md = Self::md_path(repo_root, slug);
        let text = std::fs::read_to_string(&md)?;
        let new_line = format!("traced-from-mission: {mission_id}");

        let (bom, source) = match text.strip_prefix('\u{feff}') {
            Some(rest) => ("\u{feff}", rest),
            None => ("", text.as_str()),
        };
        let lines: Vec<&str> = source.split_inclusive('\n').collect();
        let has_frontmatter = lines
            .first()
            .map(|line| line.trim_end() == "---")
            .unwrap_or(false);

        let mut out = String::with_capacity(text.len() + new_line.len() + 8);
        out.push_str(bom);

        if !has_frontmatter {
            out.push_str("---\n");
            out.push_str(&new_line);
            out.push('\n');
            out.push_str("---\n\n");
            out.push_str(source);
            atomic_write(&md, out.as_bytes())?;
            return Ok(true);
        }

        // Scan the frontmatter block for its closing fence and an existing
        // link line (key spelling-tolerant, like the parser).
        let mut closing: Option<usize> = None;
        let mut existing: Option<(usize, String)> = None;
        for (i, line) in lines.iter().enumerate().skip(1) {
            if line.trim_end() == "---" {
                closing = Some(i);
                break;
            }
            if existing.is_none() && !line.trim_start().starts_with('#') {
                if let Some((key, value)) = line.split_once(':') {
                    let key = normalize_key(key);
                    if key == "traced-from-mission" || key == "tracedfrommission" {
                        existing = Some((i, unquote(value.trim())));
                    }
                }
            }
        }
        if closing.is_none() {
            return Err(EngineError::Config(format!(
                "ticket {slug}: frontmatter opened with `---` but was never closed"
            )));
        }
        if let Some((_, value)) = &existing {
            if value == mission_id {
                return Ok(false);
            }
        }
        let replace_idx = existing.as_ref().map(|(i, _)| *i);
        for (i, line) in lines.iter().enumerate() {
            // No existing link: insert one right after the opening fence.
            if i == 1 && replace_idx.is_none() {
                out.push_str(&new_line);
                out.push('\n');
            }
            if replace_idx == Some(i) {
                out.push_str(&new_line);
                out.push('\n');
            } else {
                out.push_str(line);
            }
        }
        atomic_write(&md, out.as_bytes())?;
        Ok(true)
    }
}

/// Per-question length cap (in chars) and total-count cap applied before
/// writing clarifying questions into a ticket's needs-context section, so a
/// multi-KB orchestrator reply cannot blow up the ticket file.
const MAX_QUESTION_CHARS: usize = 500;
const MAX_QUESTION_COUNT: usize = 20;

/// Truncate one question (or the wrong-plan reason) to [`MAX_QUESTION_CHARS`]
/// characters, char-boundary safe.
fn truncate_one(q: &str) -> String {
    if q.chars().count() > MAX_QUESTION_CHARS {
        let mut truncated: String = q.chars().take(MAX_QUESTION_CHARS).collect();
        truncated.push_str(" … (truncated)");
        truncated
    } else {
        q.to_string()
    }
}

/// Bound the wrong-plan reason before it lands in the ticket body and the
/// `.status` note: trimmed, single-paragraph, length-capped like a question.
fn bound_reason(reason: &str) -> String {
    truncate_one(reason.trim())
}

/// Bound a `state-note` for ONE frontmatter line: whitespace-flattened (a raw
/// newline would split the frontmatter record across lines), trimmed, and
/// length-capped like a needs-context question. Written unquoted — the
/// frontmatter parser reads a scalar back verbatim (same as `title:`).
fn bound_state_note(note: &str) -> String {
    truncate_one(&note.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Truncate each question to [`MAX_QUESTION_CHARS`] characters (char-boundary
/// safe) and cap the total number of questions to [`MAX_QUESTION_COUNT`],
/// appending a single "N more omitted" marker when truncated.
fn bound_questions(questions: &[String]) -> Vec<String> {
    if questions.len() <= MAX_QUESTION_COUNT {
        return questions.iter().map(|q| truncate_one(q)).collect();
    }

    let mut out: Vec<String> = questions[..MAX_QUESTION_COUNT]
        .iter()
        .map(|q| truncate_one(q))
        .collect();
    let omitted = questions.len() - MAX_QUESTION_COUNT;
    out.push(format!("… ({omitted} more omitted)"));
    out
}

/// Atomic write via a sibling temp file + rename (POSIX rename is atomic; on
/// Windows the target is removed first when present).
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ticket");
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if cfg!(windows) => {
            // Windows rename fails when the destination exists.
            let _ = std::fs::remove_file(path);
            std::fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

// ---------------------------------------------------------------------------
// Frontmatter
// ---------------------------------------------------------------------------

/// A frontmatter value: either a scalar string or a bracketed `[a, b]` list.
enum FrontValue {
    Scalar(String),
    List(Vec<String>),
}

impl FrontValue {
    fn scalar(&self) -> String {
        match self {
            FrontValue::Scalar(s) => s.clone(),
            // A list where a scalar was expected: join for a best-effort string.
            FrontValue::List(items) => items.join(", "),
        }
    }

    fn list(&self) -> Vec<String> {
        match self {
            FrontValue::List(items) => items.clone(),
            // A bare scalar where a list was expected becomes a one-item list.
            FrontValue::Scalar(s) if !s.is_empty() => vec![s.clone()],
            FrontValue::Scalar(_) => Vec::new(),
        }
    }
}

/// Split a leading `---` frontmatter block from the body. Returns the parsed
/// `key: value` pairs and the remaining markdown body. Missing frontmatter is
/// allowed (empty pairs, whole input is the body). An opening `---` with no
/// closing fence is malformed.
fn split_frontmatter(slug: &str, markdown: &str) -> Result<(Vec<(String, FrontValue)>, String)> {
    // Strip a leading BOM only; the body we return is the untouched remainder
    // after the closing fence so its whitespace/newlines are preserved.
    let source = markdown.trim_start_matches('\u{feff}');

    // The first line must be exactly `---` (trailing whitespace tolerated).
    let first_line_end = source.find('\n').map(|i| i + 1).unwrap_or(source.len());
    let first_line = source[..first_line_end].trim_end();
    if first_line != "---" {
        // No frontmatter: the whole (BOM-stripped) input is the body.
        return Ok((Vec::new(), source.to_string()));
    }

    let mut pairs: Vec<(String, FrontValue)> = Vec::new();

    // Walk the remaining lines tracking byte offsets so we can slice the exact
    // body once the closing fence is found.
    let mut offset = first_line_end;
    while offset < source.len() {
        let rest = &source[offset..];
        let line_len = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        let raw = &rest[..line_len];

        if raw.trim_end() == "---" {
            // Body is everything after this closing fence line.
            let body = source[offset + line_len..].to_string();
            return Ok((pairs, body));
        }

        let line = raw.trim();
        if !line.is_empty() && !line.starts_with('#') {
            if let Some((key, value)) = line.split_once(':') {
                let key = normalize_key(key);
                if !key.is_empty() {
                    pairs.push((key, parse_front_value(value.trim())));
                }
            }
            // Lines without a colon inside frontmatter are ignored.
        }

        offset += line_len;
    }

    Err(EngineError::Config(format!(
        "ticket {slug}: frontmatter opened with `---` but was never closed"
    )))
}

/// Lower-case a frontmatter key and strip whitespace; hyphens/underscores are
/// preserved in the lower-cased form so the match arms can normalize spellings.
fn normalize_key(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace('_', "")
}

/// Parse a scalar or a bracketed `[a, b, c]` list from a raw value string.
fn parse_front_value(raw: &str) -> FrontValue {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let items = inner
            .split(',')
            .map(|item| unquote(item.trim()))
            .filter(|item| !item.is_empty())
            .collect();
        FrontValue::List(items)
    } else {
        FrontValue::Scalar(unquote(raw))
    }
}

/// Parse a `defer-until` frontmatter value as RFC 3339. A malformed value is
/// a hard [`EngineError::Config`] naming the ticket (a silently-dropped
/// deferral would queue parked work), unlike the warn-and-default scalars.
fn parse_defer_until(slug: &str, raw: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|ts| ts.with_timezone(&chrono::Utc))
        .map_err(|e| {
            EngineError::Config(format!(
                "ticket {slug}: invalid defer-until '{raw}' (expected an RFC 3339 \
                 timestamp, e.g. 2026-08-01T09:00:00Z): {e}"
            ))
        })
}

/// Strip a single pair of matching surrounding quotes, if present.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Upsert the `state:` / `state-note:` lines inside a ticket's frontmatter
/// block, preserving every other byte (BOM, key order, body). An existing
/// line is replaced in place; a missing one is inserted right after the
/// opening fence (`state:` first, then `state-note:`); a `None` note REMOVES
/// the `state-note:` line so a stale note can never describe a state it no
/// longer belongs to. A frontmatter-less ticket gains a fresh block above its
/// body. An unclosed frontmatter block is a hard error (same as the parser).
/// Same line-level idiom as [`Ticket::seed_traced_from_mission`].
fn upsert_frontmatter_state(
    slug: &str,
    text: &str,
    state: TicketLifecycle,
    note: Option<&str>,
) -> Result<String> {
    let state_line = format!("state: {}", state.as_str());
    let note_line = note.map(|n| format!("state-note: {n}"));

    let (bom, source) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text),
    };
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let has_frontmatter = lines
        .first()
        .map(|line| line.trim_end() == "---")
        .unwrap_or(false);

    let mut out = String::with_capacity(
        text.len() + state_line.len() + note_line.as_deref().map_or(0, str::len) + 8,
    );
    out.push_str(bom);

    if !has_frontmatter {
        out.push_str("---\n");
        out.push_str(&state_line);
        out.push('\n');
        if let Some(line) = &note_line {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("---\n\n");
        out.push_str(source);
        return Ok(out);
    }

    // Scan the frontmatter block for its closing fence and any existing
    // state lines (key spelling-tolerant, like the parser).
    let mut closing: Option<usize> = None;
    let mut state_idx: Option<usize> = None;
    let mut note_idx: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim_end() == "---" {
            closing = Some(i);
            break;
        }
        if !line.trim_start().starts_with('#') {
            if let Some((key, _)) = line.split_once(':') {
                match normalize_key(key).as_str() {
                    "state" if state_idx.is_none() => state_idx = Some(i),
                    "state-note" | "statenote" if note_idx.is_none() => note_idx = Some(i),
                    _ => {}
                }
            }
        }
    }
    if closing.is_none() {
        return Err(EngineError::Config(format!(
            "ticket {slug}: frontmatter opened with `---` but was never closed"
        )));
    }

    for (i, line) in lines.iter().enumerate() {
        // Missing keys insert right after the opening fence, state first.
        if i == 1 {
            if state_idx.is_none() {
                out.push_str(&state_line);
                out.push('\n');
            }
            if note_idx.is_none() {
                if let Some(line) = &note_line {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        if state_idx == Some(i) {
            out.push_str(&state_line);
            out.push('\n');
            continue;
        }
        if note_idx == Some(i) {
            // Replace in place, or drop the line entirely when no note
            // remains — a stale note must not outlive its state.
            if let Some(line) = &note_line {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Body sections
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Sections {
    goal: Option<String>,
    context: Option<String>,
    scoping_answers: Vec<String>,
    acceptance_hints: Vec<String>,
    /// First `#`/`##`… heading seen anywhere in the body (title fallback).
    first_heading: Option<String>,
}

/// Which known section a `## Heading` maps to (case-insensitive).
enum SectionKind {
    Goal,
    Context,
    ScopingAnswers,
    AcceptanceHints,
    Other,
}

fn classify_heading(text: &str) -> SectionKind {
    match text.trim().to_ascii_lowercase().as_str() {
        "goal" => SectionKind::Goal,
        "context" => SectionKind::Context,
        "scoping answers" => SectionKind::ScopingAnswers,
        "acceptance hints" => SectionKind::AcceptanceHints,
        _ => SectionKind::Other,
    }
}

/// Parse the body into known sections. Text before the first `##` section
/// (with no explicit `## Goal`) becomes the goal.
fn parse_sections(body: &str) -> Sections {
    let mut sections = Sections::default();

    // Current accumulation target: None = preamble (implicit goal), Some(kind).
    let mut current: Option<SectionKind> = None;
    let mut preamble: Vec<&str> = Vec::new();
    let mut text_buf: Vec<&str> = Vec::new();
    let mut bullets: Vec<String> = Vec::new();

    // Commit the buffer accumulated for `current` into `sections`.
    fn flush(
        current: &Option<SectionKind>,
        text_buf: &mut Vec<&str>,
        bullets: &mut Vec<String>,
        sections: &mut Sections,
    ) {
        match current {
            Some(SectionKind::Goal) => {
                let joined = text_buf.join("\n").trim().to_string();
                if !joined.is_empty() {
                    sections.goal = Some(joined);
                }
            }
            Some(SectionKind::Context) => {
                let joined = text_buf.join("\n").trim().to_string();
                if !joined.is_empty() {
                    sections.context = Some(joined);
                }
            }
            Some(SectionKind::ScopingAnswers) => {
                sections.scoping_answers.append(bullets);
            }
            Some(SectionKind::AcceptanceHints) => {
                sections.acceptance_hints.append(bullets);
            }
            Some(SectionKind::Other) | None => {}
        }
        text_buf.clear();
        bullets.clear();
    }

    for raw in body.lines() {
        if let Some(heading) = heading_text(raw) {
            if sections.first_heading.is_none() {
                sections.first_heading = Some(heading.to_string());
            }
        }

        // A `##`-level (or deeper) heading starts a body section and closes the
        // previous one. A single `#` document title is not a section: it is
        // dropped here (not folded into the preamble goal), having already
        // served as the title fallback above.
        if let Some(heading) = section_heading_text(raw) {
            flush(&current, &mut text_buf, &mut bullets, &mut sections);
            current = Some(classify_heading(heading));
            continue;
        }
        if heading_text(raw).is_some() {
            // A `#` title line while still in the preamble: skip it.
            continue;
        }

        match current {
            None => preamble.push(raw),
            Some(SectionKind::ScopingAnswers) | Some(SectionKind::AcceptanceHints) => {
                if let Some(item) = bullet_item(raw) {
                    bullets.push(item);
                }
            }
            Some(_) => text_buf.push(raw),
        }
    }
    flush(&current, &mut text_buf, &mut bullets, &mut sections);

    // Preamble (text before the first `##`) becomes the goal only when no
    // explicit `## Goal` section supplied one.
    if sections.goal.is_none() {
        let joined = preamble.join("\n").trim().to_string();
        if !joined.is_empty() {
            sections.goal = Some(joined);
        }
    }

    sections
}

/// Text of any markdown heading line (`#`, `##`, …), else None.
fn heading_text(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with('#') {
        Some(t.trim_start_matches('#').trim())
    } else {
        None
    }
}

/// Text of a section-level heading (`##` or deeper), else None. A single `#`
/// (document title) does not start a body section.
fn section_heading_text(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with("##") {
        Some(t.trim_start_matches('#').trim())
    } else {
        None
    }
}

/// The content of a dash bullet (`- item`), trimmed, else None.
fn bullet_item(line: &str) -> Option<String> {
    let t = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(marker) {
            let item = rest.trim().to_string();
            if !item.is_empty() {
                return Some(item);
            }
        }
    }
    None
}
