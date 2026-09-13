//! `kranz decompose` core (ticket: .kranz/tickets/ticket-dag-decomposition.md):
//! one planner turn decomposes a complex goal into a small DAG of ordinary
//! tickets linked by `blocked-by` edges — reusing the existing dependency
//! machinery (deps.rs satisfaction, cycle detection, work-time skip-on-failed-
//! blocker) instead of inventing new orchestration. The drain then executes
//! the DAG in dependency order under the existing claim protocol.
//!
//! [`drive_decompose`] owns the call order — planner turn ([`plan_decomposition`])
//! → deterministic validation ([`validate_nodes`]) → all-or-none write
//! ([`write_dag`], only when the caller passed `--yes`). It never prints; the
//! CLI renders the DAG preview ([`render_preview`]) and the result. Each
//! emitted ticket is an ordinary `.kranz/tickets/<slug>.md`: it flows through
//! `kranz draft` / `kranz ticket queue` exactly like a hand-written ticket —
//! the mission stays the atom, the DAG is the molecule.

use crate::backend::{AgentBackend, AgentEvent, AgentSession, PromptMode, SessionSpec};
use crate::deps;
use crate::error::{EngineError, Result};
use crate::permissions;
use crate::scrub;
use crate::ticket::Ticket;
use crate::types::{MissionConfig, Role};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Hard cap on the tickets one decomposition may propose (the DAG review is a
/// single up-front glance; past this the operator should split by hand).
pub const MAX_NODES: usize = 8;

/// Cap on the silence between two planner stream events before the session is
/// declared dead — mirrors orchestrator.rs's DEFAULT_ORCH_STALL_TIMEOUT (long
/// thinking pauses are expected; ten minutes of nothing is not).
const PLANNER_STALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Default priority when the planner omits it (1 high … 3 low), matching
/// ticket.rs's DEFAULT_PRIORITY.
fn default_priority() -> u8 {
    2
}

/// One node of the planner's proposed decomposition — the JSON shape the
/// planner prompt contracts for: `{"slug","title","priority","goal","context",
/// "acceptanceHints":["..."],"blockedBy":["slug"]}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedNode {
    pub slug: String,
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub acceptance_hints: Vec<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

/// Priority inherited from the planner output, clamped to the ticket scale
/// 1 (high) ..= 3 (low).
fn clamp_priority(priority: u8) -> u8 {
    priority.clamp(1, 3)
}

// ---------------------------------------------------------------------------
// The planner turn — mirrors the draft loop's session shape (orchestrator role
// config + permissions, read-only, budget-capped), single-shot: there is no
// mission to seed a streaming orchestrator for, and no follow-up turn.
// ---------------------------------------------------------------------------

/// System-prompt appendix for the decomposition planner session. Deliberately
/// NOT the orchestrator role prompt: that one steers toward mission plans with
/// validation contracts, while this session's whole job is the ticket-DAG JSON
/// contract the user prompt carries.
const PLANNER_SYSTEM_PROMPT: &str = "You are a decomposition planner for the kranz mission \
     harness: you split one complex goal into a small DAG of mission tickets. You may read \
     the repository (read-only) to ground the split. Your final answer is ONLY the JSON \
     array the user asked for.";

/// Build the planner's user prompt: the goal plus the exact output contract
/// and the validation rules [`validate_nodes`] will enforce deterministically.
pub fn planner_prompt(goal: &str, existing_slugs: &[String]) -> String {
    let existing = if existing_slugs.is_empty() {
        "none".to_string()
    } else {
        existing_slugs.join(", ")
    };
    format!(
        "Decompose this goal into a DAG of mission tickets:\n\
         \n\
         GOAL:\n{goal}\n\
         \n\
         Emit 1 to {MAX_NODES} tickets as a JSON array — output ONLY the JSON, no prose, no \
         code fences:\n\
         [{{\"slug\":\"kebab-slug\",\"title\":\"short title\",\"priority\":2,\"goal\":\"what \
         this ticket delivers\",\"context\":\"grounding notes\",\"acceptanceHints\":[\"checkable \
         hint\"],\"blockedBy\":[\"other-slug\"]}}]\n\
         \n\
         Rules:\n\
         - Each ticket is one mission-sized unit with its own deliverable; together they cover \
         the goal.\n\
         - slug: letters, digits, '-', '_' (no spaces, no separators), unique per ticket. \
         Already taken (never reuse these): {existing}.\n\
         - blockedBy: slugs that must Complete before this ticket can run — from your proposed \
         set or the existing tickets. A ticket with no blockedBy is a root; at least one root \
         is required. The edges must form a DAG: no cycles, no self-edges.\n\
         - priority: 1 (high) to 3 (low).\n\
         - List roots first in the array."
    )
}

/// Slugs of every parseable ticket currently in the backlog — the planner may
/// reference them in `blockedBy` and must not reuse them for new nodes.
fn existing_slugs(repo_root: &Path) -> Vec<String> {
    let mut slugs: Vec<String> = Ticket::list(repo_root)
        .into_iter()
        .map(|t| t.slug)
        .collect();
    slugs.sort();
    slugs
}

/// Run the one-shot planner turn against `backend` and parse the proposed DAG.
/// Session construction mirrors the draft loop's orchestrator turn (role model/
/// effort/budget, read-only orchestrator permissions); errors surface the same
/// way — backend failures as [`EngineError::Backend`], an unparseable reply as
/// a clear [`EngineError::Other`] naming the contract (never a panic).
pub async fn plan_decomposition(
    backend: &dyn AgentBackend,
    repo_root: &Path,
    goal: &str,
    cfg: &MissionConfig,
) -> Result<Vec<PlannedNode>> {
    let role_cfg = cfg.role(Role::Orchestrator).clone();
    let mut spec = SessionSpec {
        cwd: repo_root.to_path_buf(),
        prompt: PromptMode::SingleShot(planner_prompt(goal, &existing_slugs(repo_root))),
        append_system_prompt: Some(PLANNER_SYSTEM_PROMPT.to_string()),
        model: role_cfg.model.clone(),
        effort: role_cfg.reasoning_effort.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        tools: role_cfg.tools.clone(),
        writable: false,
        settings_json: None,
        json_schema: None,
        max_budget_usd: role_cfg.max_budget_usd,
        max_turns: role_cfg.max_turns,
        env: std::collections::HashMap::new(),
        sandbox: None,
        hook_status: None,
    };
    permissions::apply(
        permissions::for_role(Role::Orchestrator, cfg, &[], &[], &[]),
        &mut spec,
    );

    let mut session = backend.start(spec).await?;
    let text = pump_planner(session.as_mut()).await?;
    parse_planner_output(&text)
}

/// Pump the planner session to its terminal `Result`, mirroring
/// orchestrator.rs's pump_turn text handling: the turn's text is the `Result`
/// text when non-empty, else the concatenated assistant `Text` blocks — and it
/// is credential-scrubbed at this single choke point before anything derives
/// from it.
async fn pump_planner(session: &mut dyn AgentSession) -> Result<String> {
    let mut texts: Vec<String> = Vec::new();
    loop {
        let event = match tokio::time::timeout(PLANNER_STALL_TIMEOUT, session.next_event()).await {
            Err(_elapsed) => {
                return Err(EngineError::Backend(format!(
                "decompose planner stream stalled (> {PLANNER_STALL_TIMEOUT:?} without an event)"
            )))
            }
            Ok(result) => result?,
        };
        match event {
            None => {
                let detail = session
                    .exit_status()
                    .map(|e| format!("{e:?}"))
                    .unwrap_or_else(|| "no exit status".to_string());
                return Err(EngineError::Backend(format!(
                    "decompose planner stream closed without a result ({detail})"
                )));
            }
            Some(AgentEvent::Text { text, .. }) => texts.push(text),
            Some(AgentEvent::Result { text, is_error, .. }) => {
                if is_error {
                    return Err(EngineError::Backend(format!(
                        "decompose planner turn returned an error result: {}",
                        scrub::scrub(&text)
                    )));
                }
                let turn_text = if text.trim().is_empty() {
                    texts.join("\n")
                } else {
                    text
                };
                return Ok(scrub::scrub(&turn_text));
            }
            // Init/ToolUse/ToolResult/Other: transcript-level noise here; the
            // planner's tools are read-only and its contract is the Result text.
            Some(_) => {}
        }
    }
}

/// Parse the planner's reply into nodes: strict whole-text parse, then the
/// first-`[`-to-last-`]` substring (prose-wrapped or fenced output) — the
/// array-shaped twin of runner.rs's `parse_report` leniency. Anything else is
/// a clear error quoting the reply's opening, never a panic.
pub fn parse_planner_output(text: &str) -> Result<Vec<PlannedNode>> {
    let trimmed = text.trim();
    match serde_json::from_str::<Vec<PlannedNode>>(trimmed) {
        Ok(nodes) => Ok(nodes),
        Err(strict_err) => {
            if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']')) {
                if start < end {
                    if let Ok(nodes) =
                        serde_json::from_str::<Vec<PlannedNode>>(&trimmed[start..=end])
                    {
                        return Ok(nodes);
                    }
                }
            }
            Err(EngineError::Other(format!(
                "decompose planner did not emit a valid JSON array of tickets \
                 ({strict_err}); reply began: {}",
                opening_excerpt(trimmed)
            )))
        }
    }
}

/// First ~120 chars of a reply, for error messages that quote the planner.
fn opening_excerpt(text: &str) -> String {
    const MAX: usize = 120;
    if text.chars().count() > MAX {
        format!("{}…", text.chars().take(MAX).collect::<String>())
    } else {
        text.to_string()
    }
}

// ---------------------------------------------------------------------------
// Deterministic validation — the hard gate, run identically for the dry-run
// preview and the write. Every refusal is loud and writes NOTHING.
// ---------------------------------------------------------------------------

/// Validate a proposed DAG against the repo's backlog: 1..=[`MAX_NODES`]
/// nodes; every slug slug-valid, unique within the set, and not colliding with
/// an existing ticket; every `blockedBy` edge resolving to a slug in the
/// proposed set or an existing ticket; no cycle within the proposed set; at
/// least one root (a node with no blockers). Cycles that run THROUGH
/// pre-existing ticket edges are caught by the authoritative
/// [`deps::detect_cycle`] gate inside [`write_dag`] (it reads the files, so it
/// needs the staged write first).
pub fn validate_nodes(repo_root: &Path, nodes: &[PlannedNode]) -> Result<()> {
    if nodes.is_empty() {
        return Err(EngineError::InvalidState(
            "decompose planner proposed 0 tickets (need 1..=8)".to_string(),
        ));
    }
    if nodes.len() > MAX_NODES {
        return Err(EngineError::InvalidState(format!(
            "decompose planner proposed {} tickets (max {MAX_NODES}) — narrow the goal \
             or split it by hand",
            nodes.len()
        )));
    }

    let dir = Ticket::tickets_dir(repo_root);
    let mut seen: HashSet<&str> = HashSet::with_capacity(nodes.len());
    for node in nodes {
        Ticket::ensure_valid_slug(&node.slug)?;
        if !seen.insert(node.slug.as_str()) {
            return Err(EngineError::InvalidState(format!(
                "decompose planner proposed duplicate slug '{}'",
                node.slug
            )));
        }
        let path = dir.join(format!("{}.md", node.slug));
        if path.exists() {
            return Err(EngineError::InvalidState(format!(
                "ticket '{}' already exists at {}",
                node.slug,
                path.display()
            )));
        }
    }

    for node in nodes {
        for blocker in &node.blocked_by {
            Ticket::ensure_valid_slug(blocker)?;
            if seen.contains(blocker.as_str()) {
                continue;
            }
            let path = dir.join(format!("{blocker}.md"));
            if !path.is_file() {
                return Err(EngineError::InvalidState(format!(
                    "node '{}' is blocked by unknown slug '{blocker}': not in the proposed \
                     set and no existing ticket at {}",
                    node.slug,
                    path.display()
                )));
            }
        }
    }

    if let Some(cycle) = in_set_cycle(nodes) {
        return Err(EngineError::InvalidState(format!(
            "blocked-by cycle in the proposed decomposition: {}",
            cycle.join(" -> ")
        )));
    }

    if !nodes.iter().any(|n| n.blocked_by.is_empty()) {
        return Err(EngineError::InvalidState(
            "the proposed decomposition has no root: every node is blocked — at least one \
             node must have an empty blockedBy"
                .to_string(),
        ));
    }
    Ok(())
}

/// DFS the `blockedBy` edges restricted to the proposed set — the in-memory
/// twin of deps.rs's `detect_cycle` (same path/on-path bookkeeping, same
/// `[a, b, a]` cycle shape), so a cyclic proposal is refused with the cycle
/// named BEFORE any file is staged. Edges leaving the set (to existing
/// tickets) are skipped here; [`deps::detect_cycle`] owns them post-write.
fn in_set_cycle(nodes: &[PlannedNode]) -> Option<Vec<String>> {
    fn dfs<'a>(
        nodes: &'a [PlannedNode],
        current: &'a str,
        path: &mut Vec<&'a str>,
        on_path: &mut HashSet<&'a str>,
    ) -> Option<Vec<String>> {
        let node = nodes.iter().find(|n| n.slug == current)?;
        for blocker in &node.blocked_by {
            let b = blocker.as_str();
            if !nodes.iter().any(|n| n.slug == b) {
                continue;
            }
            if on_path.contains(b) {
                let mut cycle: Vec<String> = path.iter().map(|s| (*s).to_string()).collect();
                cycle.push(b.to_string());
                let start = cycle
                    .iter()
                    .position(|s| s == b)
                    .expect("blocker is in on_path, so it is in path");
                return Some(cycle[start..].to_vec());
            }
            path.push(b);
            on_path.insert(b);
            if let Some(found) = dfs(nodes, b, path, on_path) {
                return Some(found);
            }
            path.pop();
            on_path.remove(b);
        }
        None
    }

    for node in nodes {
        let mut path = vec![node.slug.as_str()];
        let mut on_path: HashSet<&str> = HashSet::from([node.slug.as_str()]);
        if let Some(cycle) = dfs(nodes, &node.slug, &mut path, &mut on_path) {
            return Some(cycle);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Rendering — the same frontmatter/body shape as Ticket::ticket_template, plus
// the blocked-by edge and acceptance hints the planner supplied.
// ---------------------------------------------------------------------------

/// Render one node as `.kranz/tickets/<slug>.md` markdown. The result parses
/// back cleanly through [`Ticket::parse`] ([`write_dag`] proves it per node).
pub fn render_ticket(node: &PlannedNode) -> String {
    let title = one_line(&scrub::scrub(node.title.trim()));
    let goal = scrub::scrub(node.goal.trim());
    let context = scrub::scrub(node.context.trim());
    let blocked_by = if node.blocked_by.is_empty() {
        String::new()
    } else {
        format!("blocked-by: [{}]\n", node.blocked_by.join(", "))
    };
    let mut hints = String::new();
    for hint in &node.acceptance_hints {
        let h = one_line(&scrub::scrub(hint.trim()));
        if !h.is_empty() {
            hints.push_str("- ");
            hints.push_str(&h);
            hints.push('\n');
        }
    }
    format!(
        "---\n\
         title: {title}\n\
         priority: {priority}\n\
         schedule: once\n\
         {blocked_by}\
         ---\n\
         \n\
         ## Goal\n\
         {goal}\n\
         \n\
         ## Context\n\
         {context}\n\
         \n\
         ## Scoping answers\n\
         \n\
         ## Acceptance hints\n\
         {hints}",
        priority = clamp_priority(node.priority),
    )
}

/// Flatten to a single line: frontmatter scalars and bullet items must never
/// carry a line break into the rendered ticket — a "\nblocked-by: …" smuggled
/// inside a title would inject frontmatter past validation.
fn one_line(text: &str) -> String {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render the dry-run/pre-write DAG preview: one line per node with its
/// priority, its edges (or `(root)`), and its title.
pub fn render_preview(nodes: &[PlannedNode]) -> String {
    let slug_w = nodes.iter().map(|n| n.slug.len()).max().unwrap_or(4).max(4);
    let edge_cell = |n: &PlannedNode| {
        if n.blocked_by.is_empty() {
            "(root)".to_string()
        } else {
            format!("blocked-by: {}", n.blocked_by.join(", "))
        }
    };
    let edge_w = nodes.iter().map(|n| edge_cell(n).len()).max().unwrap_or(6);
    let mut out = format!("proposed decomposition: {} ticket(s)\n", nodes.len());
    for node in nodes {
        out.push_str(&format!(
            "  {:<slug_w$}  pri={}  {:<edge_w$}  {}\n",
            node.slug,
            clamp_priority(node.priority),
            edge_cell(node),
            one_line(node.title.trim()),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// write_dag — stage all N tickets, validate, then write all or none
// ---------------------------------------------------------------------------

/// Validate `nodes`, render every ticket, write them all, then run the
/// authoritative [`deps::detect_cycle`] gate per new slug. Any refusal — a
/// validation failure, an I/O error mid-write, or a cycle (including one
/// running through pre-existing ticket edges) — rolls back every file this
/// call wrote and returns the error loudly: the operator never ends up with
/// half a DAG. Returns the written paths on success.
pub fn write_dag(repo_root: &Path, nodes: &[PlannedNode]) -> Result<Vec<PathBuf>> {
    validate_nodes(repo_root, nodes)?;

    // Stage: render every ticket and prove it parses back (mirrors
    // Ticket::scaffold's parse-back check) BEFORE any file is written.
    let dir = Ticket::tickets_dir(repo_root);
    let mut staged: Vec<(PathBuf, String)> = Vec::with_capacity(nodes.len());
    for node in nodes {
        let body = render_ticket(node);
        Ticket::parse(&node.slug, &body).map_err(|e| {
            EngineError::Other(format!(
                "internal error: rendered ticket '{}' does not parse: {e}",
                node.slug
            ))
        })?;
        staged.push((dir.join(format!("{}.md", node.slug)), body));
    }

    std::fs::create_dir_all(&dir)?;
    let mut written: Vec<PathBuf> = Vec::with_capacity(staged.len());
    for (path, body) in &staged {
        if let Err(e) = std::fs::write(path, body) {
            rollback(&written);
            return Err(e.into());
        }
        written.push(path.clone());
    }

    // The authoritative cycle gate (deps::detect_cycle reads the ticket files,
    // so it also sees edges through pre-existing tickets — the in-set DFS
    // above cannot). Any cycle rolls the whole write back.
    for node in nodes {
        match deps::detect_cycle(repo_root, &node.slug) {
            Ok(None) => {}
            Ok(Some(cycle)) => {
                rollback(&written);
                return Err(EngineError::InvalidState(format!(
                    "blocked-by cycle: {} — refusing the decomposition (nothing was written)",
                    cycle.join(" -> ")
                )));
            }
            Err(e) => {
                rollback(&written);
                return Err(e);
            }
        }
    }
    Ok(written)
}

/// Best-effort removal of the files a refused [`write_dag`] already wrote.
fn rollback(written: &[PathBuf]) {
    for path in written {
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// drive_decompose — the sequencing core (mirrors draft.rs's drive_draft)
// ---------------------------------------------------------------------------

/// [`drive_decompose`]'s return: the validated nodes plus, when `yes` was
/// passed, the paths [`write_dag`] wrote (`None` = dry-run preview, nothing
/// written). The core never prints; the caller renders [`render_preview`] and
/// the outcome.
#[derive(Debug, Clone)]
pub struct DecomposeDrive {
    pub nodes: Vec<PlannedNode>,
    pub written: Option<Vec<PathBuf>>,
}

/// Drive one decomposition: planner turn → [`validate_nodes`] (the preview and
/// the write are gated identically, so a dry run of an invalid DAG refuses
/// just as loudly) → [`write_dag`] when `yes`, nothing otherwise.
pub async fn drive_decompose(
    backend: &dyn AgentBackend,
    repo_root: &Path,
    goal: &str,
    cfg: &MissionConfig,
    yes: bool,
) -> Result<DecomposeDrive> {
    let nodes = plan_decomposition(backend, repo_root, goal, cfg).await?;
    validate_nodes(repo_root, &nodes)?;
    let written = if yes {
        Some(write_dag(repo_root, &nodes)?)
    } else {
        None
    };
    Ok(DecomposeDrive { nodes, written })
}

// ---------------------------------------------------------------------------
// tests — validation/refusal gates over tempdir fixtures (no git, no backend)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn node(slug: &str, blocked_by: &[&str]) -> PlannedNode {
        PlannedNode {
            slug: slug.to_string(),
            title: format!("title for {slug}"),
            priority: 2,
            goal: format!("goal for {slug}"),
            context: String::new(),
            acceptance_hints: vec![format!("{slug} works")],
            blocked_by: blocked_by.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn md_files(repo: &Path) -> Vec<String> {
        let dir = Ticket::tickets_dir(repo);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut files: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                name.ends_with(".md").then_some(name)
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn valid_chain_validates_and_writes_all_tickets() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let nodes = vec![
            node("setup-db", &[]),
            node("api-layer", &["setup-db"]),
            node("frontend", &["api-layer"]),
        ];

        let written = write_dag(repo, &nodes).unwrap();
        assert_eq!(written.len(), 3);
        assert_eq!(
            md_files(repo),
            vec!["api-layer.md", "frontend.md", "setup-db.md"]
        );

        let frontend = Ticket::load(&Ticket::tickets_dir(repo).join("frontend.md")).unwrap();
        assert_eq!(frontend.blocked_by, vec!["api-layer".to_string()]);
        assert_eq!(frontend.schedule, crate::ticket::Schedule::Once);
        assert_eq!(frontend.goal, "goal for frontend");
        assert_eq!(
            frontend.acceptance_hints,
            vec!["frontend works".to_string()]
        );

        let root = Ticket::load(&Ticket::tickets_dir(repo).join("setup-db.md")).unwrap();
        assert!(root.blocked_by.is_empty());
        assert_eq!(
            deps::detect_cycle(repo, "setup-db").unwrap(),
            None,
            "a written valid DAG must pass the authoritative cycle gate"
        );
    }

    #[test]
    fn planner_priority_is_clamped_to_the_ticket_scale() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let mut high = node("urgent", &[]);
        high.priority = 0;
        let mut low = node("whenever", &[]);
        low.priority = 9;

        write_dag(repo, &[high, low]).unwrap();
        let urgent = Ticket::load(&Ticket::tickets_dir(repo).join("urgent.md")).unwrap();
        let whenever = Ticket::load(&Ticket::tickets_dir(repo).join("whenever.md")).unwrap();
        assert_eq!(urgent.priority, 1);
        assert_eq!(whenever.priority, 3);
    }

    #[test]
    fn ab_cycle_is_refused_loudly_with_zero_files_written() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let nodes = vec![node("a", &["b"]), node("b", &["a"])];

        let err = write_dag(repo, &nodes).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cycle"),
            "expected a cycle refusal, got: {msg}"
        );
        assert!(msg.contains("a") && msg.contains("b"));
        assert!(
            md_files(repo).is_empty(),
            "a refused decomposition must leave zero files behind"
        );
    }

    #[test]
    fn self_edge_is_refused_as_a_cycle_with_zero_files_written() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let nodes = vec![node("solo", &["solo"])];

        let err = write_dag(repo, &nodes).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cycle"),
            "expected a cycle refusal, got: {msg}"
        );
        assert!(md_files(repo).is_empty());
    }

    #[test]
    fn cycle_through_an_existing_ticket_rolls_back_every_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        // Pre-existing ticket whose blocked-by dangles onto the slug the
        // planner is about to propose — writing the node closes old -> new ->
        // old, a cycle only deps::detect_cycle can see (it reads the files).
        let dir = Ticket::tickets_dir(repo);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("old.md"),
            "---\ntitle: old\npriority: 2\nschedule: once\nblocked-by: [new-node]\n---\n\n## Goal\nold\n",
        )
        .unwrap();

        let nodes = vec![node("root-node", &[]), node("new-node", &["old"])];
        let err = write_dag(repo, &nodes).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cycle"),
            "expected a cycle refusal, got: {msg}"
        );
        assert!(
            msg.contains("new-node") && msg.contains("old"),
            "the refusal should name the cycle path: {msg}"
        );
        assert_eq!(
            md_files(repo),
            vec!["old.md"],
            "the rollback must remove exactly the files this call wrote"
        );
    }

    #[test]
    fn unknown_blocker_is_refused_and_names_the_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let nodes = vec![node("root", &[]), node("child", &["ghost"])];

        let err = write_dag(repo, &nodes).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ghost"),
            "the refusal must name the unknown blocker: {msg}"
        );
        assert!(md_files(repo).is_empty());
    }

    #[test]
    fn existing_ticket_satisfies_a_blocker_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        Ticket::scaffold(repo, "existing-base", "base", None, None).unwrap();

        let nodes = vec![node("follow-up", &["existing-base"])];
        // No empty-blockedBy node here — "existing-base" is not in the set, so
        // this also exercises that the root rule looks only at the proposal.
        // (It has no root, so validation must refuse; add a root to write.)
        let err = validate_nodes(repo, &nodes).unwrap_err();
        assert!(err.to_string().contains("no root"));

        let nodes = vec![node("root", &[]), node("follow-up", &["existing-base"])];
        let written = write_dag(repo, &nodes).unwrap();
        assert_eq!(written.len(), 2);
        let follow_up = Ticket::load(&Ticket::tickets_dir(repo).join("follow-up.md")).unwrap();
        assert_eq!(follow_up.blocked_by, vec!["existing-base".to_string()]);
    }

    #[test]
    fn collision_with_an_existing_ticket_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        Ticket::scaffold(repo, "taken", "taken", None, None).unwrap();

        let err = write_dag(repo, &[node("taken", &[])]).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(md_files(repo), vec!["taken.md"]);
    }

    #[test]
    fn duplicate_slugs_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let nodes = vec![node("dup", &[]), node("dup", &[])];

        let err = validate_nodes(repo, &nodes).unwrap_err();
        assert!(err.to_string().contains("duplicate slug 'dup'"));
        assert!(md_files(repo).is_empty());
    }

    #[test]
    fn invalid_slug_chars_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        for bad in ["bad slug", "../evil", ".hidden", ""] {
            let nodes = vec![node(bad, &[])];
            assert!(
                validate_nodes(repo, &nodes).is_err(),
                "slug '{bad}' must be refused"
            );
        }
        assert!(md_files(repo).is_empty());
        // The traversal attempt must not have escaped the tickets dir.
        assert!(!repo.join("evil.md").exists());
    }

    #[test]
    fn too_many_nodes_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let nodes: Vec<PlannedNode> = (0..=MAX_NODES)
            .map(|i| node(&format!("n{i}"), &[]))
            .collect();
        assert_eq!(nodes.len(), MAX_NODES + 1);

        let err = validate_nodes(repo, &nodes).unwrap_err();
        assert!(err.to_string().contains("max"), "got: {err}");
        assert!(md_files(repo).is_empty());
    }

    #[test]
    fn zero_nodes_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let err = validate_nodes(repo, &[]).unwrap_err();
        assert!(err.to_string().contains("0 tickets"), "got: {err}");
    }

    #[test]
    fn malformed_planner_json_is_a_clear_error_not_a_panic() {
        let err = parse_planner_output("total prose, no json at all").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("JSON array"), "got: {msg}");
        assert!(
            msg.contains("total prose"),
            "the reply excerpt helps: {msg}"
        );

        // A JSON object (not an array) and an array missing required keys
        // fail the same clear way.
        assert!(parse_planner_output("{\"slug\":\"x\"}").is_err());
        assert!(parse_planner_output("[{\"title\":\"no slug\"}]").is_err());
    }

    #[test]
    fn prose_wrapped_and_fenced_arrays_parse_leniently() {
        let bare = r#"[{"slug":"a","title":"A","priority":1,"goal":"g","context":"c","acceptanceHints":["h"],"blockedBy":[]}]"#;
        let nodes = parse_planner_output(bare).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].slug, "a");
        assert_eq!(nodes[0].priority, 1);

        let wrapped = format!("Here is the decomposition you asked for:\n{bare}\nHope that helps!");
        let nodes = parse_planner_output(&wrapped).unwrap();
        assert_eq!(nodes.len(), 1);

        let fenced = format!("Sure!\n```json\n{bare}\n```\n");
        let nodes = parse_planner_output(&fenced).unwrap();
        assert_eq!(nodes.len(), 1);

        // Defaults: missing priority/context/hints/blockedBy fill in.
        let minimal = r#"[{"slug":"b","title":"B","goal":"g"}]"#;
        let nodes = parse_planner_output(minimal).unwrap();
        assert_eq!(nodes[0].priority, 2);
        assert!(nodes[0].blocked_by.is_empty());
    }

    #[test]
    fn frontmatter_injection_through_a_title_is_flattened() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let mut evil = node("evil", &[]);
        evil.title = "nice title\nblocked-by: [ghost]".to_string();

        write_dag(repo, &[evil]).unwrap();
        let written = Ticket::load(&Ticket::tickets_dir(repo).join("evil.md")).unwrap();
        assert!(
            written.blocked_by.is_empty(),
            "a newline in the title must not smuggle frontmatter"
        );
        assert_eq!(written.title, "nice title blocked-by: [ghost]");
    }

    #[test]
    fn render_ticket_round_trips_through_the_ticket_parser() {
        let mut n = node("round-trip", &["a", "b"]);
        n.priority = 3;
        n.context = "some context\nover lines".to_string();
        let parsed = Ticket::parse("round-trip", &render_ticket(&n)).unwrap();
        assert_eq!(parsed.title, "title for round-trip");
        assert_eq!(parsed.priority, 3);
        assert_eq!(parsed.blocked_by, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(parsed.goal, "goal for round-trip");
        assert_eq!(parsed.context, "some context\nover lines");
        assert_eq!(
            parsed.acceptance_hints,
            vec!["round-trip works".to_string()]
        );
    }

    #[test]
    fn render_preview_shows_roots_edges_and_priorities() {
        let mut low = node("frontend", &["api"]);
        low.priority = 9;
        let preview = render_preview(&[node("api", &[]), low]);
        assert!(preview.contains("proposed decomposition: 2 ticket(s)"));
        assert!(preview.contains("api"));
        assert!(preview.contains("(root)"));
        assert!(preview.contains("blocked-by: api"));
        assert!(
            preview.contains("pri=3"),
            "preview shows the clamped priority"
        );
    }

    #[test]
    fn planner_prompt_carries_the_goal_and_existing_slugs() {
        let prompt = planner_prompt("build a thing", &["taken-one".to_string()]);
        assert!(prompt.contains("build a thing"));
        assert!(prompt.contains("taken-one"));
        assert!(prompt.contains("blockedBy"));
        assert!(prompt.contains("JSON"));
    }
}
