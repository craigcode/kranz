//! Mission tickets — `.kranz/tickets/<slug>.md` (design: docs/backlog-and-slack.md).
//!
//! A ticket is a mission-in-waiting authored by a human as markdown: a small
//! `---` frontmatter block (parsed here without a YAML dependency) plus body
//! sections (`## Goal`, `## Context`, `## Scoping answers`, `## Acceptance
//! hints`). The `.md` stays human-authored; mutable pipeline status lives in a
//! sibling `<slug>.status` JSON file so the ticket text is never rewritten by
//! the engine (except the explicit "needs context" append the orchestrator
//! makes).
//!
//! [`Ticket::mission_goal`] folds the whole ticket into one readable markdown
//! blob so the non-interactive draft driver can seed the orchestrator with the
//! entire ticket in a single message.

use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default priority when frontmatter omits it (1 high … 3 low).
const DEFAULT_PRIORITY: u8 = 2;

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
    Review,
    Queued,
    Running,
    Done,
    Failed,
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

    /// Parse ticket markdown. `slug` is supplied by the caller (usually the
    /// file stem). Malformed frontmatter is an [`EngineError::Config`].
    pub fn parse(slug: &str, markdown: &str) -> Result<Ticket> {
        let (front, body) = split_frontmatter(slug, markdown)?;

        let mut title: Option<String> = None;
        let mut priority = DEFAULT_PRIORITY;
        let mut repo_refs: Vec<String> = Vec::new();
        let mut schedule = Schedule::Once;
        let mut max_budget_usd: Option<f64> = None;

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
                "schedule" => schedule = Schedule::parse(&value.scalar()),
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

    /// Fold the whole ticket into one readable-markdown message: the goal plus
    /// a compact appendix carrying scoping answers, acceptance hints, and
    /// context — enough for a draft driver to seed the orchestrator in one go.
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

        out
    }

    // -- status file ------------------------------------------------------

    /// Path of the sibling status file for a slug.
    fn status_path(repo_root: &Path, slug: &str) -> PathBuf {
        Self::tickets_dir(repo_root).join(format!("{slug}.status"))
    }

    /// Path of the ticket markdown for a slug.
    fn md_path(repo_root: &Path, slug: &str) -> PathBuf {
        Self::tickets_dir(repo_root).join(format!("{slug}.md"))
    }

    /// Read the pipeline state; a missing or unreadable status file is
    /// [`TicketState::New`]. An invalid slug never touches the filesystem.
    pub fn read_state(repo_root: &Path, slug: &str) -> TicketState {
        if !Self::valid_slug(slug) {
            return TicketState::New;
        }
        let path = Self::status_path(repo_root, slug);
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<StatusFile>(&text) {
                Ok(sf) => sf.state,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "unreadable ticket status; treating as new");
                    TicketState::New
                }
            },
            Err(_) => TicketState::New,
        }
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
        let text = std::fs::read_to_string(Self::status_path(repo_root, slug)).ok()?;
        serde_json::from_str(&text).ok()
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
        for q in questions {
            text.push_str("- ");
            text.push_str(q);
            text.push('\n');
        }
        atomic_write(&md, text.as_bytes())?;
        Self::write_state(repo_root, slug, TicketState::NeedsContext, None)?;
        Ok(())
    }
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
