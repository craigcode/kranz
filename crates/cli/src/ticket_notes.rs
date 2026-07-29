//! The `kranz ticket note|notes` surface (D-BW-3, adopted from beads):
//! environment concerns (note author) and terminal rendering over
//! [`kranz_engine::ticket_notes`], which owns storage, the append-only
//! durability idiom, and the draft-context folding — so every surface
//! (CLI today, REST/Slack later) shares one implementation.

use anyhow::{bail, Result};
use kranz_engine::ticket::Ticket;
use kranz_engine::ticket_notes::{self, TicketNote};
use std::path::Path;

/// Environment variable naming the author recorded on a new note.
const AUTHOR_ENV: &str = "KRANZ_NOTE_AUTHOR";

/// The author for a new note: `$KRANZ_NOTE_AUTHOR` when set and non-blank,
/// else `"operator"`.
pub fn note_author() -> String {
    std::env::var(AUTHOR_ENV)
        .ok()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

/// `kranz ticket note <slug> <text...>`: append one note to the ticket's
/// discussion. Refuses on an unknown ticket (a typo'd slug must not orphan a
/// notes sidecar with no ticket behind it).
pub fn cmd_ticket_note(repo: &Path, slug: &str, text: &str) -> Result<String> {
    let path = Ticket::tickets_dir(repo).join(format!("{slug}.md"));
    if !path.is_file() {
        bail!("ticket '{slug}' not found at {}", path.display());
    }
    let note = ticket_notes::append_note(repo, slug, &note_author(), text)?;
    Ok(format!("noted on '{slug}' at {}\n", note.ts.to_rfc3339()))
}

/// `kranz ticket notes <slug>`: print the discussion chronologically.
pub fn cmd_ticket_notes(repo: &Path, slug: &str) -> Result<String> {
    let notes = ticket_notes::read_notes(repo, slug)?;
    Ok(render_ticket_notes(slug, &notes))
}

/// Render the notes listing: one `<ts>  <author>: <text>` line per note, in
/// file order (== chronological order).
pub fn render_ticket_notes(slug: &str, notes: &[TicketNote]) -> String {
    if notes.is_empty() {
        return format!("no notes on '{slug}'\n");
    }
    let mut out = format!("notes on '{slug}' ({}):\n", notes.len());
    for note in notes {
        out.push_str(&format!(
            "{}  {}: {}\n",
            note.ts.to_rfc3339(),
            note.author,
            note.text
        ));
    }
    out
}
