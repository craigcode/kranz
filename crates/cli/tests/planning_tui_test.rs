//! Headless tests for the planning TUI's pure pieces: the input editor state
//! machine, submit/queue routing, transcript scroll bookkeeping, approval-key
//! filtering, status-line formatting, and word wrapping. The full-screen app
//! itself needs a real terminal and is exercised by manual smoke runs.

use crossterm::event::KeyCode;
use kranz_cli::planning_tui::{
    approval_key, busy_status_line, classify_submission, post_approval_key, wrap_text,
    ApprovalKey, BusyKind, InputEditor, PendingQueue, PlanningOutcome, ScrollState,
    SubmitDisposition, APPROVAL_BAR, IDLE_STATUS, PLAN_NOT_READY_NOTICE, POST_APPROVAL_BAR,
    SPINNER_FRAMES,
};

// ---------------------------------------------------------------------------
// InputEditor — insert / backspace / delete / cursor movement
// ---------------------------------------------------------------------------

fn type_str(editor: &mut InputEditor, s: &str) {
    for c in s.chars() {
        editor.insert(c);
    }
}

#[test]
fn editor_insert_and_cursor_movement() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "hello");
    assert_eq!(ed.text(), "hello");
    assert_eq!(ed.cursor(), 5);

    ed.left();
    ed.left();
    assert_eq!(ed.cursor(), 3);
    ed.insert('X');
    assert_eq!(ed.text(), "helXlo");
    assert_eq!(ed.cursor(), 4);

    ed.right();
    ed.right();
    ed.right(); // clamped at end
    assert_eq!(ed.cursor(), 6);

    ed.home();
    assert_eq!(ed.cursor(), 0);
    ed.left(); // clamped at start
    assert_eq!(ed.cursor(), 0);
    ed.end();
    assert_eq!(ed.cursor(), 6);
}

#[test]
fn editor_backspace_and_delete() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "abc");
    ed.backspace();
    assert_eq!(ed.text(), "ab");
    assert_eq!(ed.cursor(), 2);

    ed.home();
    ed.backspace(); // no-op at start
    assert_eq!(ed.text(), "ab");
    ed.delete();
    assert_eq!(ed.text(), "b");
    assert_eq!(ed.cursor(), 0);
    ed.delete();
    ed.delete(); // no-op on empty
    assert_eq!(ed.text(), "");
}

#[test]
fn editor_word_movement() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "one two  three");
    assert_eq!(ed.cursor(), 14);
    ed.word_left();
    assert_eq!(ed.cursor(), 9); // start of "three"
    ed.word_left();
    assert_eq!(ed.cursor(), 4); // start of "two"
    ed.word_left();
    assert_eq!(ed.cursor(), 0);
    ed.word_left(); // clamped
    assert_eq!(ed.cursor(), 0);

    ed.word_right();
    assert_eq!(ed.cursor(), 3); // end of "one"
    ed.word_right();
    assert_eq!(ed.cursor(), 7); // end of "two"
    ed.word_right();
    assert_eq!(ed.cursor(), 14);
    ed.word_right(); // clamped
    assert_eq!(ed.cursor(), 14);
}

#[test]
fn editor_clear_line() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "scrap this");
    ed.clear_line();
    assert_eq!(ed.text(), "");
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn editor_submit_trims_and_resets() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "  hi there  ");
    assert_eq!(ed.submit().as_deref(), Some("hi there"));
    assert_eq!(ed.text(), "");
    assert_eq!(ed.cursor(), 0);

    type_str(&mut ed, "   ");
    assert_eq!(ed.submit(), None); // blank line
}

// ---------------------------------------------------------------------------
// InputEditor — history navigation incl. the edited-buffer guard
// ---------------------------------------------------------------------------

#[test]
fn editor_history_up_down_walks_entries() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "first");
    ed.submit();
    type_str(&mut ed, "second");
    ed.submit();

    ed.history_up();
    assert_eq!(ed.text(), "second");
    assert_eq!(ed.cursor(), 6);
    ed.history_up();
    assert_eq!(ed.text(), "first");
    ed.history_up(); // clamped at oldest
    assert_eq!(ed.text(), "first");

    ed.history_down();
    assert_eq!(ed.text(), "second");
    ed.history_down(); // past newest: back to a fresh empty buffer
    assert_eq!(ed.text(), "");
    ed.history_down(); // no-op when not navigating
    assert_eq!(ed.text(), "");
}

#[test]
fn editor_history_blocked_while_buffer_edited() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "sent");
    ed.submit();

    type_str(&mut ed, "draft");
    ed.history_up(); // guarded: an edited buffer is never clobbered
    assert_eq!(ed.text(), "draft");
    ed.history_down();
    assert_eq!(ed.text(), "draft");

    // Deleting everything makes the buffer untouched again.
    for _ in 0.."draft".len() {
        ed.backspace();
    }
    assert!(!ed.is_edited());
    ed.history_up();
    assert_eq!(ed.text(), "sent");
}

#[test]
fn editor_editing_a_recalled_entry_locks_navigation() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "one");
    ed.submit();
    type_str(&mut ed, "two");
    ed.submit();

    ed.history_up();
    assert_eq!(ed.text(), "two"); // recall itself does not count as an edit
    ed.insert('!');
    assert_eq!(ed.text(), "two!");
    ed.history_up(); // now locked
    assert_eq!(ed.text(), "two!");
    ed.history_down();
    assert_eq!(ed.text(), "two!");
}

#[test]
fn editor_history_skips_consecutive_duplicates() {
    let mut ed = InputEditor::new();
    type_str(&mut ed, "same");
    ed.submit();
    type_str(&mut ed, "same");
    ed.submit();

    ed.history_up();
    assert_eq!(ed.text(), "same");
    ed.history_up(); // only one entry recorded
    assert_eq!(ed.text(), "same");
    ed.history_down();
    assert_eq!(ed.text(), "");
}

// ---------------------------------------------------------------------------
// Pending queue semantics: submit-while-busy queues, drains FIFO when idle
// ---------------------------------------------------------------------------

#[test]
fn classify_routes_by_business() {
    // Idle: text runs a turn, /plan requests the plan.
    assert_eq!(
        classify_submission("build it", false),
        SubmitDisposition::Turn("build it".into())
    );
    assert_eq!(classify_submission("/plan", false), SubmitDisposition::RequestPlan);

    // Busy: both queue instead — never discarded, never answering a prompt.
    assert_eq!(
        classify_submission("also do X", true),
        SubmitDisposition::Queued("also do X".into())
    );
    assert_eq!(
        classify_submission("/plan", true),
        SubmitDisposition::Queued("/plan".into())
    );

    // /quit acts immediately in both states.
    assert_eq!(classify_submission("/quit", false), SubmitDisposition::Quit);
    assert_eq!(classify_submission("/quit", true), SubmitDisposition::Quit);

    // Unknown commands are rejected immediately, busy or not.
    assert_eq!(
        classify_submission("/frobnicate", true),
        SubmitDisposition::Unknown("/frobnicate".into())
    );
    // Leading/trailing whitespace is trimmed before routing.
    assert_eq!(classify_submission("  /quit  ", true), SubmitDisposition::Quit);
}

#[test]
fn queue_is_fifo_and_reports_depth() {
    let mut queue = PendingQueue::new();
    assert!(queue.is_empty());
    assert_eq!(queue.depth(), 0);
    assert_eq!(queue.pop(), None);

    // Three messages submitted while a turn runs...
    for text in ["first", "second", "third"] {
        if let SubmitDisposition::Queued(t) = classify_submission(text, true) {
            queue.push(t);
        } else {
            panic!("busy submission must queue");
        }
    }
    assert_eq!(queue.depth(), 3);

    // ...drain oldest-first when the engine is idle again.
    assert_eq!(queue.pop().as_deref(), Some("first"));
    assert_eq!(queue.pop().as_deref(), Some("second"));
    assert_eq!(queue.pop().as_deref(), Some("third"));
    assert_eq!(queue.pop(), None);
    assert!(queue.is_empty());
}

// ---------------------------------------------------------------------------
// Transcript scroll: auto-follow vs detached bookkeeping
// ---------------------------------------------------------------------------

#[test]
fn scroll_follows_bottom_by_default() {
    let scroll = ScrollState::new();
    assert!(!scroll.is_detached());
    // 100 lines in a 10-line viewport: the last 10 are visible.
    assert_eq!(scroll.top(100, 10), 90);
    // Content shorter than the viewport starts at the top.
    assert_eq!(scroll.top(5, 10), 0);
}

#[test]
fn scroll_up_detaches_and_new_content_stays_put() {
    let mut scroll = ScrollState::new();
    scroll.scroll_up(3, 100, 10);
    assert!(scroll.is_detached());
    assert_eq!(scroll.top(100, 10), 87);

    // New output arrives: a detached view must not move.
    assert_eq!(scroll.top(150, 10), 87);
}

#[test]
fn scroll_down_to_bottom_reattaches() {
    let mut scroll = ScrollState::new();
    scroll.scroll_up(5, 100, 10);
    assert!(scroll.is_detached());

    scroll.scroll_down(2, 100, 10);
    assert!(scroll.is_detached()); // still above the bottom
    assert_eq!(scroll.top(100, 10), 87);

    scroll.scroll_down(50, 100, 10); // overshoot clamps to bottom
    assert!(!scroll.is_detached());
    assert_eq!(scroll.top(100, 10), 90);
}

#[test]
fn scroll_end_key_reattaches() {
    let mut scroll = ScrollState::new();
    scroll.scroll_up(30, 100, 10);
    assert!(scroll.is_detached());
    scroll.to_follow();
    assert!(!scroll.is_detached());
    assert_eq!(scroll.top(100, 10), 90);
}

#[test]
fn scroll_up_on_short_content_stays_attached() {
    let mut scroll = ScrollState::new();
    scroll.scroll_up(3, 5, 10); // everything already visible
    assert!(!scroll.is_detached());
    assert_eq!(scroll.top(5, 10), 0);
}

// ---------------------------------------------------------------------------
// Approval-mode key filtering
// ---------------------------------------------------------------------------

#[test]
fn approval_mode_filters_keys() {
    assert_eq!(approval_key(KeyCode::Char('y')), ApprovalKey::Approve);
    assert_eq!(approval_key(KeyCode::Char('Y')), ApprovalKey::Approve);
    assert_eq!(approval_key(KeyCode::Char('n')), ApprovalKey::Reject);
    assert_eq!(approval_key(KeyCode::Char('N')), ApprovalKey::Reject);

    // Everything else is ignored — the old REPL's type-ahead bug (a stray
    // line answering the approval prompt) must be impossible.
    assert_eq!(approval_key(KeyCode::Enter), ApprovalKey::Ignore);
    assert_eq!(approval_key(KeyCode::Esc), ApprovalKey::Ignore);
    assert_eq!(approval_key(KeyCode::Char('q')), ApprovalKey::Ignore);
    assert_eq!(approval_key(KeyCode::Char(' ')), ApprovalKey::Ignore);
    assert_eq!(approval_key(KeyCode::Up), ApprovalKey::Ignore);
}

// ---------------------------------------------------------------------------
// Post-approval mode ("start execution now?") key filtering
// ---------------------------------------------------------------------------

#[test]
fn post_approval_mode_filters_keys() {
    // y starts the run, n exits with the plan committed.
    assert_eq!(post_approval_key(KeyCode::Char('y')), Some(PlanningOutcome::ApprovedRun));
    assert_eq!(post_approval_key(KeyCode::Char('Y')), Some(PlanningOutcome::ApprovedRun));
    assert_eq!(post_approval_key(KeyCode::Char('n')), Some(PlanningOutcome::ApprovedExit));
    assert_eq!(post_approval_key(KeyCode::Char('N')), Some(PlanningOutcome::ApprovedExit));

    // Everything else is ignored — starting execution spend must be an
    // explicit keypress; Enter/type-ahead must never trigger it.
    assert_eq!(post_approval_key(KeyCode::Enter), None);
    assert_eq!(post_approval_key(KeyCode::Esc), None);
    assert_eq!(post_approval_key(KeyCode::Char('q')), None);
    assert_eq!(post_approval_key(KeyCode::Char(' ')), None);
    assert_eq!(post_approval_key(KeyCode::Char('r')), None);
    assert_eq!(post_approval_key(KeyCode::Up), None);
}

// ---------------------------------------------------------------------------
// Status line formatting
// ---------------------------------------------------------------------------

#[test]
fn busy_status_shows_spinner_elapsed_and_queue_depth() {
    let line = busy_status_line(BusyKind::Turn, 12, 0, 0);
    assert!(line.starts_with(SPINNER_FRAMES[0]), "{line}");
    assert!(line.contains("orchestrator working…"), "{line}");
    assert!(line.contains("12s"), "{line}");
    assert!(line.contains("typing is safe"), "{line}");
    assert!(line.contains("Enter queues your message"), "{line}");
    assert!(!line.contains("queued,"), "{line}");

    let line = busy_status_line(BusyKind::PlanRequest, 3, 1, 2);
    assert!(line.starts_with(SPINNER_FRAMES[1]), "{line}");
    assert!(line.contains("requesting plan…"), "{line}");
    assert!(line.contains("3s"), "{line}");
    assert!(line.contains("2 messages queued, send in order when this turn finishes"), "{line}");

    let line = busy_status_line(BusyKind::Turn, 5, 0, 1);
    assert!(line.contains("1 message queued, sends when this turn finishes"), "{line}");
}

#[test]
fn spinner_frames_cycle() {
    let n = SPINNER_FRAMES.len();
    let a = busy_status_line(BusyKind::Turn, 0, 0, 0);
    let b = busy_status_line(BusyKind::Turn, 0, n, 0); // wraps to frame 0
    assert_eq!(a, b);
    let c = busy_status_line(BusyKind::Turn, 0, 1, 0);
    assert_ne!(a, c);
}

#[test]
fn fixed_status_texts_mention_their_keys() {
    assert!(IDLE_STATUS.contains("/plan"));
    assert!(IDLE_STATUS.contains("/quit"));
    assert!(APPROVAL_BAR.contains("[y]"));
    assert!(APPROVAL_BAR.contains("[n]"));
    // The post-approval bar states the commit already happened, asks the
    // spend question explicitly, and names both keys.
    assert!(POST_APPROVAL_BAR.contains("plan committed"));
    assert!(POST_APPROVAL_BAR.contains("start execution now?"));
    assert!(POST_APPROVAL_BAR.contains("[y]"));
    assert!(POST_APPROVAL_BAR.contains("[n]"));
    // The plan-not-ready notice points back at the conversation and the
    // retry command — plain guidance, no error language.
    assert!(PLAN_NOT_READY_NOTICE.contains("/plan"));
    assert!(!PLAN_NOT_READY_NOTICE.to_lowercase().contains("error"));
    assert!(!PLAN_NOT_READY_NOTICE.to_lowercase().contains("fail"));
}

// ---------------------------------------------------------------------------
// Word wrapping
// ---------------------------------------------------------------------------

#[test]
fn wrap_prefers_spaces_and_hard_breaks_long_words() {
    assert_eq!(wrap_text("a bb ccc", 100), vec!["a bb ccc"]);
    assert_eq!(wrap_text("one two three", 8), vec!["one two", "three"]);
    // A word longer than the width is hard-broken, not lost.
    assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    // Explicit newlines (plan renders) are preserved.
    assert_eq!(wrap_text("a\n\nb", 10), vec!["a", "", "b"]);
    // Width zero degrades to unwrapped lines instead of looping.
    assert_eq!(wrap_text("x y", 0), vec!["x y"]);
}
