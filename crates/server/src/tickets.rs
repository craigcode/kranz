//! `GET /api/tickets` and `GET /api/tickets/:slug` — read-only backlog
//! surface over `.kranz/tickets/` (docs/protocol.md). Every ticket is
//! re-parsed from disk per request, matching the rest of this crate's
//! no-cache discipline.

use crate::error::ApiError;
use crate::ServerState;
use axum::extract::{Path as UrlPath, State};
use axum::Json;
use kranz_engine::ticket::Ticket;
use serde_json::{json, Value};
use std::sync::Arc;

/// `GET /api/tickets` — a summary row per parseable ticket under
/// `.kranz/tickets/`: slug, priority, pipeline state, title, and blockedBy.
pub(crate) async fn list_tickets(State(server): State<Arc<ServerState>>) -> Json<Value> {
    let rows: Vec<Value> = Ticket::list(server.host.repo_root())
        .iter()
        .map(|ticket| ticket_summary_json(server.host.repo_root(), ticket))
        .collect();
    Json(Value::Array(rows))
}

/// `GET /api/tickets/:slug` — the full parsed ticket plus `needsContext`
/// (the orchestrator's clarifying questions, if any were appended). 400 for
/// an invalid/traversal slug (checked at the route boundary before any
/// filesystem access), 404 for a slug with no ticket file.
pub(crate) async fn get_ticket(
    State(server): State<Arc<ServerState>>,
    UrlPath(slug): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    Ticket::ensure_valid_slug(&slug)
        .map_err(|e| ApiError::bad_request(format!("invalid ticket slug '{slug}': {e}")))?;

    let path = Ticket::tickets_dir(server.host.repo_root()).join(format!("{slug}.md"));
    if !path.is_file() {
        return Err(ApiError::not_found(format!("unknown ticket '{slug}'")));
    }
    let ticket = Ticket::load(&path)
        .map_err(|e| ApiError::internal(format!("failed to parse ticket '{slug}': {e}")))?;

    Ok(Json(ticket_full_json(server.host.repo_root(), &ticket)))
}

/// Summary projection for the list route: no goal/context/body text, just
/// enough to render a backlog table.
fn ticket_summary_json(repo_root: &std::path::Path, ticket: &Ticket) -> Value {
    let state = Ticket::read_state(repo_root, &ticket.slug);
    json!({
        "slug": ticket.slug,
        "priority": ticket.priority,
        "state": state,
        "title": ticket.title,
        "blockedBy": ticket.blocked_by,
    })
}

/// Full projection for the show route: every parsed field plus the derived
/// `needsContext` question list.
fn ticket_full_json(repo_root: &std::path::Path, ticket: &Ticket) -> Value {
    let state = Ticket::read_state(repo_root, &ticket.slug);
    json!({
        "slug": ticket.slug,
        "title": ticket.title,
        "priority": ticket.priority,
        "schedule": ticket.schedule,
        "blockedBy": ticket.blocked_by,
        "goal": ticket.goal,
        "context": ticket.context,
        "scopingAnswers": ticket.scoping_answers,
        "acceptanceHints": ticket.acceptance_hints,
        "state": state,
        "needsContext": needs_context_questions(&ticket.raw_body),
    })
}

/// Port of `kranz_cli::backlog::needs_context_block`'s heading scan, but
/// returning the bare question strings (bullet items, marker stripped)
/// instead of the raw markdown block — a REST consumer wants data, not
/// text to re-render.
fn needs_context_questions(raw_body: &str) -> Vec<String> {
    let mut collecting = false;
    let mut out = Vec::new();
    for line in raw_body.lines() {
        let is_section = line.trim_start().starts_with("##");
        if collecting && is_section {
            break; // next section ends the block
        }
        if is_section
            && line
                .trim_start_matches('#')
                .trim()
                .to_ascii_lowercase()
                .starts_with("needs context")
        {
            collecting = true;
            continue;
        }
        if collecting {
            if let Some(item) = bullet_item(line) {
                out.push(item);
            }
        }
    }
    out
}

/// The content of a dash bullet (`- item`), trimmed, else None. Mirrors
/// `kranz_engine::ticket`'s private helper of the same name.
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
