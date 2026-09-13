//! Integration tests for the `kranz ticket note|notes` surface (D-BW-3):
//! clap parsing, author resolution (`KRANZ_NOTE_AUTHOR` > `operator`), the
//! unknown-ticket refusal, and chronological rendering. Storage semantics are
//! covered by the engine's `ticket_notes_test.rs`.

use clap::Parser;
use kranz_cli::cli::{Cli, Command, TicketCommand};
use kranz_cli::ticket_notes::{self, render_ticket_notes};
use kranz_engine::ticket::Ticket;
use std::path::Path;

fn scaffold(root: &Path, slug: &str) {
    Ticket::scaffold(root, slug, "Fixture", None, None).unwrap();
}

#[test]
fn ticket_note_clap_parsing() {
    let cli =
        Cli::try_parse_from(["kranz", "ticket", "note", "my-slug", "hello", "world"]).unwrap();
    match cli.command {
        Command::Ticket {
            command: TicketCommand::Note { slug, text },
        } => {
            assert_eq!(slug, "my-slug");
            assert_eq!(text, vec!["hello".to_string(), "world".to_string()]);
        }
        other => panic!("expected ticket note, got {other:?}"),
    }

    // Text is required.
    assert!(Cli::try_parse_from(["kranz", "ticket", "note", "my-slug"]).is_err());

    let cli = Cli::try_parse_from(["kranz", "ticket", "notes", "my-slug"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Ticket {
            command: TicketCommand::Notes { ref slug }
        } if slug == "my-slug"
    ));
}

#[test]
fn ticket_note_author_env_precedence() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, "demo");

    // Unset (and blank) falls back to "operator".
    std::env::remove_var("KRANZ_NOTE_AUTHOR");
    ticket_notes::cmd_ticket_note(root, "demo", "default author").unwrap();
    std::env::set_var("KRANZ_NOTE_AUTHOR", "   ");
    ticket_notes::cmd_ticket_note(root, "demo", "blank author").unwrap();
    // Set wins.
    std::env::set_var("KRANZ_NOTE_AUTHOR", "alice");
    ticket_notes::cmd_ticket_note(root, "demo", "env author").unwrap();
    std::env::remove_var("KRANZ_NOTE_AUTHOR");

    let notes = kranz_engine::ticket_notes::read_notes(root, "demo").unwrap();
    let authors: Vec<&str> = notes.iter().map(|n| n.author.as_str()).collect();
    assert_eq!(authors, vec!["operator", "operator", "alice"]);
}

#[test]
fn ticket_note_cmd_round_trip_and_chronological_render() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, "demo");

    // Unknown ticket: refused, no orphan sidecar.
    let err = ticket_notes::cmd_ticket_note(root, "ghost", "hello").unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    assert!(!Ticket::tickets_dir(root).join("ghost.notes.jsonl").exists());

    // Empty listing renders honestly.
    assert_eq!(
        ticket_notes::cmd_ticket_notes(root, "demo").unwrap(),
        "no notes on 'demo'\n"
    );

    ticket_notes::cmd_ticket_note(root, "demo", "first").unwrap();
    ticket_notes::cmd_ticket_note(root, "demo", "second note").unwrap();
    let out = ticket_notes::cmd_ticket_notes(root, "demo").unwrap();
    assert!(out.contains("notes on 'demo' (2):"), "{out}");
    let first_at = out.find("first").unwrap();
    let second_at = out.find("second note").unwrap();
    assert!(first_at < second_at, "chronological order:\n{out}");
}

#[test]
fn ticket_note_render_shape() {
    let notes = vec![kranz_engine::ticket_notes::TicketNote {
        ts: chrono::DateTime::parse_from_rfc3339("2026-07-29T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        author: "operator".to_string(),
        text: "shipped it".to_string(),
    }];
    let out = render_ticket_notes("demo", &notes);
    assert_eq!(
        out,
        "notes on 'demo' (1):\n2026-07-29T10:00:00+00:00  operator: shipped it\n"
    );
}
