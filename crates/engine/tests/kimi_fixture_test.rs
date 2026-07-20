//! Validates the committed `kimi -p --output-format stream-json` fixture
//! used to ground the future `backend_kimi` parser, and guards the specific
//! gap flagged by finding `f-1-1` / contract `a4`: the fixture's raw wire
//! lines do NOT contain an `Init` line and do NOT end in a terminal
//! `Result` frame (the last line is a `session.resume_hint` meta line), so
//! `docs/scoping/kimi-cli-backend.md` must say in-so-many-words how
//! `backend_kimi` synthesizes both events from lines that are neither.
//! This crate has no kimi parser yet, so these tests check the fixture's
//! raw JSONL shape directly rather than through an `AgentEvent` adapter.

use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn fixture_lines(name: &str) -> Vec<serde_json::Value> {
    let content = std::fs::read_to_string(fixture_path(name)).expect("read fixture");
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSON line"))
        .collect()
}

#[test]
fn fixture_has_no_init_or_system_line() {
    let lines = fixture_lines("kimi_exec_scrutiny.jsonl");
    for line in &lines {
        let role = line["role"].as_str().unwrap_or_default();
        assert_ne!(
            role, "system",
            "fixture must not contain an init/system line"
        );
        assert_ne!(role, "init", "fixture must not contain an init/system line");
    }
}

#[test]
fn fixture_does_not_end_in_a_terminal_result_frame() {
    let lines = fixture_lines("kimi_exec_scrutiny.jsonl");
    let last = lines.last().expect("fixture has at least one line");

    // The last wire line is the `session.resume_hint` meta line, not a
    // `result`-typed frame carrying the terminal Result payload directly.
    assert_eq!(last["role"], "meta");
    assert_eq!(last["type"], "session.resume_hint");
    assert_ne!(last["role"], "result");
    assert_ne!(last["type"], "result");
}

#[test]
fn doc_explicitly_notes_the_init_and_result_synthesis_seam() {
    let doc_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("scoping")
        .join("kimi-cli-backend.md");
    let raw = std::fs::read_to_string(&doc_path).expect("read kimi-cli-backend.md");
    // Normalize markdown line-wrapping so substring checks aren't sensitive
    // to where a paragraph happens to wrap.
    let doc = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(
        doc.contains("stdout line corresponds to it at all")
            || doc.contains("never emits an `init`/`system` line"),
        "doc must state that no wire line carries an Init frame"
    );
    assert!(
        doc.contains("synthesize at stream start"),
        "doc must state backend_kimi synthesizes Init at stream start from its own invocation"
    );
    assert!(
        doc.contains("synthesize from the `session.resume_hint` line itself")
            || doc.contains("there is no separate result frame to parse"),
        "doc must state the resume_hint line is the sole source of the terminal Result"
    );
}
