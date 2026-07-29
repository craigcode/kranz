//! Integration tests for the ticket discussion primitive
//! (`.kranz/tickets/<slug>.notes.jsonl` — D-BW-3, adopted from beads):
//! append-only storage, safe-id validation, chronological reads, and the
//! capped draft-context folding.

use kranz_engine::ticket::Ticket;
use kranz_engine::ticket_notes::{self, MAX_DRAFT_NOTES};
use std::path::Path;

fn notes_file(root: &Path, slug: &str) -> std::path::PathBuf {
    Ticket::tickets_dir(root).join(format!("{slug}.notes.jsonl"))
}

fn scaffold(root: &Path, slug: &str) {
    Ticket::scaffold(root, slug, "Fixture", None, None).unwrap();
}

#[test]
fn ticket_note_append_creates_file_and_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, "demo");

    // No sidecar yet: reads as empty, nothing on disk.
    assert!(ticket_notes::read_notes(root, "demo").unwrap().is_empty());
    assert!(!notes_file(root, "demo").exists());

    let note = ticket_notes::append_note(root, "demo", "operator", "why priority changed").unwrap();
    assert_eq!(note.author, "operator");
    assert_eq!(note.text, "why priority changed");

    // The file appeared on first append, one JSON object on one line,
    // keyed {ts, author, text} in that order (struct declaration order).
    let raw = std::fs::read_to_string(notes_file(root, "demo")).unwrap();
    let line = raw.trim_end();
    assert!(!line.contains('\n'), "one note == one line: {raw}");
    assert!(line.starts_with("{\"ts\":"), "ts leads: {line}");
    assert!(line.contains("\"author\":\"operator\""), "author: {line}");
    assert!(
        line.contains("\"text\":\"why priority changed\""),
        "text: {line}"
    );

    let notes = ticket_notes::read_notes(root, "demo").unwrap();
    assert_eq!(notes, vec![note]);
}

#[test]
fn ticket_note_appends_chronological_and_append_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, "demo");

    ticket_notes::append_note(root, "demo", "alice", "first").unwrap();
    let after_first = std::fs::read_to_string(notes_file(root, "demo")).unwrap();
    ticket_notes::append_note(root, "demo", "bob", "second").unwrap();
    ticket_notes::append_note(root, "demo", "alice", "third").unwrap();

    // File order is chronological order (oldest first), authors preserved.
    let notes = ticket_notes::read_notes(root, "demo").unwrap();
    let texts: Vec<&str> = notes.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(texts, vec!["first", "second", "third"]);
    assert!(notes[0].ts <= notes[1].ts && notes[1].ts <= notes[2].ts);

    // Append-only is literal: every prior byte survives untouched (there is
    // no edit/delete path — the storage layer never rewrites history).
    let after_all = std::fs::read_to_string(notes_file(root, "demo")).unwrap();
    assert!(
        after_all.starts_with(&after_first),
        "later appends must not rewrite earlier lines"
    );
}

#[test]
fn ticket_note_bad_slug_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    for bad in ["../evil", "a/b", "", ".hidden"] {
        assert!(
            ticket_notes::append_note(root, bad, "op", "x").is_err(),
            "append with slug {bad:?} must be refused"
        );
        assert!(
            ticket_notes::read_notes(root, bad).is_err(),
            "read with slug {bad:?} must be refused"
        );
    }
    // Nothing escaped the tickets dir.
    assert!(!root.join(".kranz").join("evil.notes.jsonl").exists());
}

#[test]
fn ticket_note_empty_text_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, "demo");

    assert!(ticket_notes::append_note(root, "demo", "op", "   ").is_err());
    assert!(ticket_notes::append_note(root, "demo", "", "text").is_err());
    // A refused append creates nothing.
    assert!(!notes_file(root, "demo").exists());
}

#[test]
fn ticket_note_corrupt_line_errors_naming_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, "demo");
    std::fs::write(notes_file(root, "demo"), "not json\n").unwrap();

    let err = ticket_notes::read_notes(root, "demo").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("demo.notes.jsonl"), "names the file: {msg}");
    assert!(msg.contains("line 1"), "names the line: {msg}");
}

#[test]
fn ticket_note_draft_context_includes_recent_and_caps_at_50() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, "demo");

    // No notes: no section (the seed stays exactly the folded ticket).
    assert_eq!(ticket_notes::draft_context(root, "demo").unwrap(), None);

    for i in 1..=(MAX_DRAFT_NOTES + 10) {
        ticket_notes::append_note(root, "demo", "op", &format!("note {i}")).unwrap();
    }
    let section = ticket_notes::draft_context(root, "demo")
        .unwrap()
        .expect("notes produce a section");
    assert!(section.contains("## Ticket notes"));
    assert!(
        section.contains(&format!(
            "(most recent {MAX_DRAFT_NOTES} of {} notes)",
            MAX_DRAFT_NOTES + 10
        )),
        "omission marker: {section}"
    );
    // The most recent 50 are notes 11..=60: newest present, oldest capped.
    assert!(section.contains("note 60"));
    assert!(section.contains("note 11"));
    assert!(!section.contains("note 10]"));
    assert!(!section.contains(": note 10\n"));
    let bullets = section.lines().filter(|l| l.starts_with("- [")).count();
    assert_eq!(bullets, MAX_DRAFT_NOTES);
}

#[test]
fn ticket_note_draft_context_small_discussion_lists_all() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, "demo");

    ticket_notes::append_note(root, "demo", "alice", "review found X").unwrap();
    ticket_notes::append_note(root, "demo", "bob", "decided Y").unwrap();
    let section = ticket_notes::draft_context(root, "demo")
        .unwrap()
        .expect("notes produce a section");
    assert!(section.contains("alice: review found X"));
    assert!(section.contains("bob: decided Y"));
    assert!(
        !section.contains("most recent"),
        "no omission marker under the cap: {section}"
    );
}
