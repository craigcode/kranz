//! Ticket discussion notes — `.kranz/tickets/<slug>.notes.jsonl` (D-BW-3,
//! adopted from beads' flat `{author, text, created_at}` comment model).
//!
//! One JSON object per line — `{ts, author, text}` — appended only: there is
//! no edit or delete, mirroring the event log's honesty posture. Notes are
//! the ticket-scoped "why" channel the frontmatter (structured) and body
//! (authored once) cannot carry; they are COMMITTED artifacts (same class as
//! the ticket `.md` itself — see AGENTS.md's tracked-vs-runtime list), not
//! gitignored runtime state.
//!
//! Durability follows the [`crate::event_log`] idiom: open O_APPEND (creating
//! on first append), one `write_all` of the full line, flush, fsync. Appends
//! are clock-stamped at write time; file order IS chronological order, so
//! reads never re-sort.

use crate::error::{EngineError, Result};
use crate::ticket::Ticket;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// How many notes [`draft_context`] folds into the drafter's seed — the most
/// recent N, so a long discussion cannot blow up the prompt.
pub const MAX_DRAFT_NOTES: usize = 50;

/// One note on a ticket. Field order is the on-disk key order (`ts`,
/// `author`, `text`); additive-only like every persisted shape here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketNote {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub author: String,
    pub text: String,
}

/// Path of the notes sidecar for a slug. Slug validation happens in
/// [`append_note`]/[`read_notes`] BEFORE this is ever joined, so a
/// traversal-shaped slug cannot escape `.kranz/tickets/` (the same
/// safe-id discipline as ticket reads).
fn notes_path(repo_root: &Path, slug: &str) -> PathBuf {
    Ticket::tickets_dir(repo_root).join(format!("{slug}.notes.jsonl"))
}

/// Append one note to a ticket's discussion, creating the sidecar on first
/// append. `author` is supplied by the caller (the CLI resolves
/// `KRANZ_NOTE_AUTHOR` → `operator`); `text` is trimmed, scrubbed (the file
/// is committed — the same posture as text the engine writes into ticket
/// bodies), and must not be empty. Returns the stored note.
pub fn append_note(repo_root: &Path, slug: &str, author: &str, text: &str) -> Result<TicketNote> {
    Ticket::ensure_valid_slug(slug)?;
    let author = author.trim();
    if author.is_empty() {
        return Err(EngineError::Config(
            "note author must not be empty (set KRANZ_NOTE_AUTHOR or omit it for 'operator')"
                .to_string(),
        ));
    }
    let text = crate::scrub::scrub(text.trim());
    if text.is_empty() {
        return Err(EngineError::Config(format!(
            "refusing to record an empty note on ticket '{slug}'"
        )));
    }
    let note = TicketNote {
        ts: chrono::Utc::now(),
        author: author.to_string(),
        text,
    };
    let mut line = serde_json::to_string(&note)?;
    line.push('\n');

    let dir = Ticket::tickets_dir(repo_root);
    std::fs::create_dir_all(&dir)?;
    // The event-log idiom: O_APPEND + create, a single write of the whole
    // line, then fsync — a concurrent appender can interleave between notes
    // but never within one.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(notes_path(repo_root, slug))?;
    file.write_all(line.as_bytes())?;
    file.flush()?;
    file.sync_data()?;
    Ok(note)
}

/// Read every note in file order (== chronological order, oldest first). A
/// missing sidecar is the normal "no notes yet" case and reads as empty. A
/// malformed line is an [`EngineError::Config`] naming the file and line —
/// notes are a committed, tool-maintained record, so corruption is surfaced
/// for repair rather than silently skipped.
pub fn read_notes(repo_root: &Path, slug: &str) -> Result<Vec<TicketNote>> {
    Ticket::ensure_valid_slug(slug)?;
    let path = notes_path(repo_root, slug);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut notes = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let note = serde_json::from_str::<TicketNote>(line).map_err(|e| {
            EngineError::Config(format!(
                "corrupt ticket notes file {} (line {}): {e}",
                path.display(),
                i + 1
            ))
        })?;
        notes.push(note);
    }
    Ok(notes)
}

/// The markdown section [`crate::draft::drive_draft`] appends to the
/// drafter's seed: the most recent [`MAX_DRAFT_NOTES`] notes as bullets, with
/// an omission marker when older ones were capped. `None` when the ticket
/// has no notes (the seed is then exactly the folded ticket, as before).
pub fn draft_context(repo_root: &Path, slug: &str) -> Result<Option<String>> {
    let notes = read_notes(repo_root, slug)?;
    if notes.is_empty() {
        return Ok(None);
    }
    let start = notes.len().saturating_sub(MAX_DRAFT_NOTES);
    let mut out = String::from("\n\n## Ticket notes\n");
    if start > 0 {
        out.push_str(&format!(
            "(most recent {MAX_DRAFT_NOTES} of {} notes)\n",
            notes.len()
        ));
    }
    for note in &notes[start..] {
        out.push_str(&format!(
            "- [{}] {}: {}\n",
            note.ts.to_rfc3339(),
            note.author,
            note.text
        ));
    }
    Ok(Some(out))
}
