//! Full-screen interactive planning TUI (`kranz plan` on a real terminal).
//!
//! Supersedes the line-mode REPL for interactive use: a ratatui alternate-
//! screen app with a scrollback transcript, a live activity feed (tailed from
//! `events.jsonl`), a color-coded status line, and an always-editable input
//! line. The problems it exists to fix (all observed live with the REPL):
//! arrow keys printed `^[[A`, `[orch]` activity lines trampled the `you>`
//! prompt, a running turn was indistinguishable from a hang, nothing
//! acknowledged accepted input, and type-ahead silently answered later
//! prompts.
//!
//! ## Interactivity contract
//!
//! The input line is ALWAYS editable, including while an orchestrator turn
//! runs. Submitting while busy echoes the message immediately (tagged
//! `(queued)`) and sends it as the next turn when the current one finishes —
//! never discarded, never leaking into an unrelated prompt. `/plan` renders
//! the plan + cost estimate and enters a single-key approval mode. A
//! successful approval commits the plan, then enters a second single-key
//! prompt — start execution now, or exit — because approving the plan and
//! starting the spend are separate consent steps ([`PlanningOutcome`]).
//!
//! ## Terminal safety
//!
//! [`TerminalGuard`] is an RAII guard restoring cooked mode + the main screen
//! on every exit path, and [`install_panic_hook`] restores the terminal
//! BEFORE the default panic hook prints — a panicked TUI must never leave the
//! shell raw. Restoration is idempotent (guarded by [`TUI_ACTIVE`]).
//!
//! ## Async wiring
//!
//! [`MissionEngine`] is not shareable across tasks, so the engine is moved
//! *into* the in-flight turn future and handed back with the result
//! ([`Phase`]). The loop `select!`s between key events (a dedicated reader
//! thread feeding a channel), a 250ms tick (activity polling via
//! [`EventLog::read_events_after`] + spinner), and the in-flight turn future.
//!
//! Everything that can be tested headless is a pure, engine-free piece:
//! [`InputEditor`], [`PendingQueue`], [`ScrollState`], [`classify_submission`],
//! [`approval_key`], [`post_approval_key`], [`busy_status_line`],
//! [`wrap_text`].

use crate::commands::augment_limit_hint;
use crate::output::{self, one_line};
use crate::tail::EventRenderer;
use anyhow::{Context, Result};
use crossterm::event::{
    self as ct_event, DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use kranz_engine::cost;
use kranz_engine::error::EngineError;
use kranz_engine::event_log::EventLog;
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::types::Plan;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use std::collections::VecDeque;
use std::future::Future;
use std::io::Stdout;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Tick driving the spinner and the activity poll.
const TICK: Duration = Duration::from_millis(250);

/// Input reader poll slice; also bounds shutdown latency of the reader thread.
const INPUT_POLL: Duration = Duration::from_millis(100);

/// Mouse wheel scroll step (visual lines).
const WHEEL_LINES: usize = 3;

// ===========================================================================
// Pure, unit-testable pieces
// ===========================================================================

// ---------------------------------------------------------------------------
// InputEditor — the single-line editor state machine
// ---------------------------------------------------------------------------

/// Editable input buffer with cursor movement and session history.
///
/// History navigation (up/down) is only available while the buffer is
/// *untouched* — empty, or holding an unedited history recall. The first
/// edit locks navigation until the buffer is submitted or emptied again, so
/// half-typed messages are never clobbered by an arrow key.
#[derive(Debug, Default)]
pub struct InputEditor {
    buf: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    nav: Option<usize>,
    edited: bool,
}

impl InputEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current buffer contents.
    pub fn text(&self) -> String {
        self.buf.iter().collect()
    }

    /// Cursor position in chars (0 ..= len).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// True while history navigation is locked out by an edit.
    pub fn is_edited(&self) -> bool {
        self.edited
    }

    pub fn insert(&mut self, c: char) {
        self.buf.insert(self.cursor, c);
        self.cursor += 1;
        self.touch();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buf.remove(self.cursor);
            self.touch();
        }
    }

    /// Delete the char under the cursor (forward delete).
    pub fn delete(&mut self) {
        if self.cursor < self.buf.len() {
            self.buf.remove(self.cursor);
            self.touch();
        }
    }

    /// Ctrl-U: clear the whole line.
    pub fn clear_line(&mut self) {
        self.buf.clear();
        self.cursor = 0;
        self.touch();
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.buf.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buf.len();
    }

    /// Alt+Left: to the start of the previous word.
    pub fn word_left(&mut self) {
        while self.cursor > 0 && self.buf[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !self.buf[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
    }

    /// Alt+Right: past the end of the next word.
    pub fn word_right(&mut self) {
        let n = self.buf.len();
        while self.cursor < n && self.buf[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < n && !self.buf[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }

    /// Up arrow: recall the previous history entry (untouched buffers only).
    pub fn history_up(&mut self) {
        if self.edited || self.history.is_empty() {
            return;
        }
        let next = match self.nav {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.recall(next);
    }

    /// Down arrow: towards newer entries; past the newest clears the buffer.
    pub fn history_down(&mut self) {
        if self.edited {
            return;
        }
        match self.nav {
            None => {}
            Some(i) if i + 1 < self.history.len() => self.recall(i + 1),
            Some(_) => {
                self.nav = None;
                self.buf.clear();
                self.cursor = 0;
            }
        }
    }

    /// Enter: take the trimmed buffer (recorded in history when non-empty)
    /// and reset the editor. `None` for a blank line.
    pub fn submit(&mut self) -> Option<String> {
        let text = self.text().trim().to_string();
        self.buf.clear();
        self.cursor = 0;
        self.nav = None;
        self.edited = false;
        if text.is_empty() {
            return None;
        }
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        Some(text)
    }

    fn recall(&mut self, index: usize) {
        self.nav = Some(index);
        self.buf = self.history[index].chars().collect();
        self.cursor = self.buf.len();
    }

    /// Any text mutation locks history navigation — except that an emptied
    /// buffer counts as untouched again (nothing left to clobber).
    fn touch(&mut self) {
        self.nav = None;
        self.edited = !self.buf.is_empty();
    }
}

// ---------------------------------------------------------------------------
// PendingQueue — submit-while-busy semantics
// ---------------------------------------------------------------------------

/// FIFO of messages submitted while a turn was in flight. Each is echoed in
/// the transcript at submit time (tagged `(queued)`) and dispatched, oldest
/// first, whenever the engine returns to idle.
#[derive(Debug, Default)]
pub struct PendingQueue {
    items: VecDeque<String>,
}

impl PendingQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, line: String) {
        self.items.push_back(line);
    }

    pub fn pop(&mut self) -> Option<String> {
        self.items.pop_front()
    }

    pub fn depth(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Submission routing
// ---------------------------------------------------------------------------

/// Where a submitted line goes given the current engine business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitDisposition {
    /// Exit planning — acts immediately, even while a turn runs.
    Quit,
    /// Run `request_plan` now (engine idle).
    RequestPlan,
    /// Run a conversational turn now (engine idle).
    Turn(String),
    /// Engine busy: echo as `(queued)` and dispatch when idle.
    Queued(String),
    /// Unknown slash command — notice only, never queued.
    Unknown(String),
}

/// Route one submitted line. `/quit` always acts immediately; unknown
/// commands are rejected immediately; `/plan` and plain text queue while
/// `busy` and run otherwise.
pub fn classify_submission(line: &str, busy: bool) -> SubmitDisposition {
    let line = line.trim();
    if line == "/quit" {
        return SubmitDisposition::Quit;
    }
    if line.starts_with('/') && line != "/plan" {
        return SubmitDisposition::Unknown(line.to_string());
    }
    if busy {
        return SubmitDisposition::Queued(line.to_string());
    }
    if line == "/plan" {
        SubmitDisposition::RequestPlan
    } else {
        SubmitDisposition::Turn(line.to_string())
    }
}

// ---------------------------------------------------------------------------
// ScrollState — transcript auto-follow vs detached scrollback
// ---------------------------------------------------------------------------

/// Transcript scroll bookkeeping over *visual* (wrapped) lines.
///
/// Follows the bottom by default (new output stays in view). Scrolling up
/// detaches at a fixed top line; scrolling back to the bottom (or End)
/// re-attaches.
#[derive(Debug)]
pub struct ScrollState {
    follow: bool,
    top: usize,
}

impl Default for ScrollState {
    fn default() -> Self {
        ScrollState {
            follow: true,
            top: 0,
        }
    }
}

impl ScrollState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_detached(&self) -> bool {
        !self.follow
    }

    /// Index of the first visible visual line for a viewport of `height`
    /// over `total` lines.
    pub fn top(&self, total: usize, height: usize) -> usize {
        let max_top = total.saturating_sub(height);
        if self.follow {
            max_top
        } else {
            self.top.min(max_top)
        }
    }

    pub fn scroll_up(&mut self, n: usize, total: usize, height: usize) {
        if total <= height {
            self.follow = true; // nothing to scroll: stay attached
            return;
        }
        self.top = self.top(total, height).saturating_sub(n);
        self.follow = false;
    }

    pub fn scroll_down(&mut self, n: usize, total: usize, height: usize) {
        let max_top = total.saturating_sub(height);
        let new_top = self.top(total, height).saturating_add(n).min(max_top);
        self.top = new_top;
        self.follow = new_top >= max_top;
    }

    /// End: jump to the bottom and re-attach.
    pub fn to_follow(&mut self) {
        self.follow = true;
    }
}

// ---------------------------------------------------------------------------
// Approval-mode key filtering
// ---------------------------------------------------------------------------

/// Outcome of one keypress in approval mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalKey {
    Approve,
    Reject,
    Ignore,
}

/// Single-key approval filter: `y`/`Y` approves, `n`/`N` goes back to the
/// conversation, everything else is ignored.
pub fn approval_key(code: KeyCode) -> ApprovalKey {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => ApprovalKey::Approve,
        KeyCode::Char('n') | KeyCode::Char('N') => ApprovalKey::Reject,
        _ => ApprovalKey::Ignore,
    }
}

// ---------------------------------------------------------------------------
// Post-approval mode (plan committed — start execution now?)
// ---------------------------------------------------------------------------

/// How planning ended. Returned by [`run`] after the TUI has torn down, so
/// the caller can chain straight into execution instead of telling the user
/// to quit and type `kranz run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningOutcome {
    /// Plan approved + committed; the user chose to start execution now.
    ApprovedRun,
    /// Plan approved + committed; the user exits (execute via `kranz run`).
    ApprovedExit,
    /// Planning ended without an approved plan.
    NotApproved,
}

/// Single-key filter for the post-approval "start execution now?" prompt:
/// `y`/`Y` starts the run, `n`/`N` exits, everything else is ignored —
/// starting spend must be an explicit keypress, never type-ahead.
pub fn post_approval_key(code: KeyCode) -> Option<PlanningOutcome> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(PlanningOutcome::ApprovedRun),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(PlanningOutcome::ApprovedExit),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Status line formatting
// ---------------------------------------------------------------------------

/// What the engine is busy doing (mirrors the in-flight future's kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyKind {
    Turn,
    PlanRequest,
}

/// Spinner frames for the busy status line.
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Idle status line.
pub const IDLE_STATUS: &str = "● ready — type a message; /plan to request the plan; /quit to exit";

/// Approval-mode bar (replaces the status line).
pub const APPROVAL_BAR: &str = "approve this plan? [y] approve & commit   [n] back to conversation";

/// Post-approval bar: the plan is committed; starting execution (spend) is
/// its own explicit consent step.
pub const POST_APPROVAL_BAR: &str = "plan committed — start execution now? [y] run   [n] exit";

/// Detached-scroll marker shown on the transcript's bottom row.
pub const DETACHED_MARKER: &str = "▼ new output below — End to follow";

/// Notice shown when `/plan` resolves to [`PlanRequest::NotReady`]: the
/// orchestrator wants answers before emitting — a normal conversational
/// state, rendered as a plain notice (no error styling, no approval mode).
pub const PLAN_NOT_READY_NOTICE: &str = "not ready to emit — answer above, then /plan again";

/// Busy status line: spinner frame + activity + elapsed + queue depth.
pub fn busy_status_line(
    kind: BusyKind,
    elapsed_secs: u64,
    spinner_frame: usize,
    queued: usize,
) -> String {
    let frame = SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()];
    let doing = match kind {
        BusyKind::Turn => "orchestrator working…",
        BusyKind::PlanRequest => "requesting plan…",
    };
    let queue = match queued {
        0 => String::new(),
        1 => " — 1 message queued, sends when this turn finishes".to_string(),
        n => format!(" — {n} messages queued, send in order when this turn finishes"),
    };
    format!("{frame} {doing} {elapsed_secs}s — typing is safe, Enter queues your message{queue}")
}

// ---------------------------------------------------------------------------
// Word wrapping (char-based; the transcript is ASCII-dominant)
// ---------------------------------------------------------------------------

/// Greedy word wrap to `width` chars per line, preferring space breaks and
/// hard-breaking longer-than-width words. Preserves explicit newlines.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return text.split('\n').map(str::to_string).collect();
    }
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let chars: Vec<char> = raw.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            let hard_end = (start + width).min(chars.len());
            let end = if hard_end < chars.len() {
                match chars[start..hard_end].iter().rposition(|c| *c == ' ') {
                    Some(p) if p > 0 => start + p,
                    _ => hard_end,
                }
            } else {
                hard_end
            };
            let line: String = chars[start..end].iter().collect();
            out.push(line.trim_end().to_string());
            start = end;
            while start < chars.len() && chars[start] == ' ' {
                start += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

/// One semantic transcript entry (styling and wrapping happen at render).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEntry {
    /// A user message, echoed at submit time; `queued` while a turn ran.
    User { text: String, queued: bool },
    /// A full orchestrator reply (possibly multi-line).
    Orch(String),
    /// One activity line from the event tail (tool use, results, denials).
    Activity(String),
    /// Preformatted block: plan render, cost estimate.
    Block(String),
    /// Informational notice.
    Notice(String),
    /// Error line.
    Error(String),
}

/// Render one entry into styled visual lines of at most `width` chars.
fn entry_lines(entry: &TranscriptEntry, width: usize, out: &mut Vec<Line<'static>>) {
    let dim = Style::new().fg(Color::DarkGray);
    match entry {
        TranscriptEntry::User { text, queued } => {
            let prefix = "you> ";
            let body = if *queued {
                format!("{text} (queued)")
            } else {
                text.clone()
            };
            let body_width = width.saturating_sub(prefix.len()).max(1);
            for (i, line) in wrap_text(&body, body_width).into_iter().enumerate() {
                let lead = if i == 0 {
                    prefix.to_string()
                } else {
                    " ".repeat(prefix.len())
                };
                out.push(Line::from(vec![
                    Span::styled(
                        lead,
                        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(line, Style::new().add_modifier(Modifier::BOLD)),
                ]));
            }
        }
        TranscriptEntry::Orch(text) => {
            let prefix = "orch> ";
            let body_width = width.saturating_sub(prefix.len()).max(1);
            for (i, line) in wrap_text(text, body_width).into_iter().enumerate() {
                let lead = if i == 0 {
                    prefix.to_string()
                } else {
                    " ".repeat(prefix.len())
                };
                out.push(Line::from(vec![Span::styled(lead, dim), Span::raw(line)]));
            }
        }
        TranscriptEntry::Activity(text) => {
            for (i, line) in wrap_text(text, width.saturating_sub(2).max(1))
                .into_iter()
                .enumerate()
            {
                let lead = if i == 0 { "" } else { "  " };
                out.push(Line::from(Span::styled(format!("{lead}{line}"), dim)));
            }
        }
        TranscriptEntry::Block(text) => {
            for line in wrap_text(text, width.max(1)) {
                out.push(Line::from(Span::raw(line)));
            }
        }
        TranscriptEntry::Notice(text) => {
            for line in wrap_text(text, width.max(1)) {
                out.push(Line::from(Span::styled(line, dim)));
            }
        }
        TranscriptEntry::Error(text) => {
            for line in wrap_text(text, width.max(1)) {
                out.push(Line::from(Span::styled(line, Style::new().fg(Color::Red))));
            }
        }
    }
}

/// Total visual lines of a transcript at `width` (scroll math helper).
fn total_visual_lines(entries: &[TranscriptEntry], width: usize) -> usize {
    let mut lines = Vec::new();
    for entry in entries {
        entry_lines(entry, width, &mut lines);
    }
    lines.len()
}

// ===========================================================================
// App state (owns the pure pieces; no engine, no futures)
// ===========================================================================

struct App {
    transcript: Vec<TranscriptEntry>,
    editor: InputEditor,
    queue: PendingQueue,
    scroll: ScrollState,
    /// Last turn error, shown red on the idle status line until the next key.
    error: Option<String>,
    spinner: usize,
}

impl App {
    fn new() -> Self {
        App {
            transcript: Vec::new(),
            editor: InputEditor::new(),
            queue: PendingQueue::new(),
            scroll: ScrollState::new(),
            error: None,
            spinner: 0,
        }
    }

    fn push(&mut self, entry: TranscriptEntry) {
        self.transcript.push(entry);
    }
}

// ===========================================================================
// Phase — engine ownership through the async loop
// ===========================================================================

/// A turn future owns the engine and hands it back with the result, so the
/// borrow checker never sees the engine borrowed across loop iterations.
type TurnFut = Pin<Box<dyn Future<Output = (Box<MissionEngine>, TurnOutput)>>>;

enum TurnOutput {
    Reply(std::result::Result<String, EngineError>),
    Plan(std::result::Result<PlanRequest, EngineError>),
}

enum Phase {
    /// Engine idle, conversation mode.
    Idle(Box<MissionEngine>),
    /// A turn or plan request in flight (engine inside the future).
    Busy {
        kind: BusyKind,
        started: Instant,
        fut: TurnFut,
    },
    /// Plan rendered; waiting for a single y/n key.
    Approval {
        engine: Box<MissionEngine>,
        plan: Plan,
    },
    /// Plan committed; waiting for the single-key run-now/exit choice. The
    /// engine is never used again but must stay alive until teardown (it
    /// flushes buffered events and releases the mission lock on drop).
    PostApproval { _engine: Box<MissionEngine> },
    /// Transient placeholder during phase swaps; never observed by draw.
    Transitioning,
}

/// Copyable view of the phase for rendering.
enum PhaseView {
    Idle,
    Busy { kind: BusyKind, elapsed_secs: u64 },
    Approval,
    PostApproval,
}

impl PhaseView {
    fn of(phase: &Phase) -> Self {
        match phase {
            Phase::Busy { kind, started, .. } => PhaseView::Busy {
                kind: *kind,
                elapsed_secs: started.elapsed().as_secs(),
            },
            Phase::Approval { .. } => PhaseView::Approval,
            Phase::PostApproval { .. } => PhaseView::PostApproval,
            Phase::Idle(_) | Phase::Transitioning => PhaseView::Idle,
        }
    }
}

fn start_turn(mut engine: Box<MissionEngine>, text: String) -> Phase {
    Phase::Busy {
        kind: BusyKind::Turn,
        started: Instant::now(),
        fut: Box::pin(async move {
            let out = engine.planning_turn(&text).await;
            (engine, TurnOutput::Reply(out))
        }),
    }
}

fn start_plan_request(mut engine: Box<MissionEngine>) -> Phase {
    Phase::Busy {
        kind: BusyKind::PlanRequest,
        started: Instant::now(),
        fut: Box::pin(async move {
            let out = engine.request_plan().await;
            (engine, TurnOutput::Plan(out))
        }),
    }
}

// ===========================================================================
// Terminal safety
// ===========================================================================

/// Set while the alternate screen + raw mode are active, so restoration is
/// idempotent and the panic hook only fires teardown when it matters.
static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Restore cooked mode + main screen (idempotent; safe from any thread).
fn restore_terminal() {
    if TUI_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}

/// RAII: terminal restored when dropped, on every path out of [`run`].
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn enter_terminal() -> Result<TerminalGuard> {
    enable_raw_mode().context("enabling raw mode")?;
    TUI_ACTIVE.store(true, Ordering::SeqCst);
    if let Err(e) = execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture) {
        restore_terminal();
        return Err(anyhow::Error::new(e).context("entering the alternate screen"));
    }
    Ok(TerminalGuard)
}

/// Chain a terminal-restoring panic hook in front of the default one, so a
/// panicking TUI prints its message on a sane screen instead of a raw one.
/// Installed once per process; a no-op while the TUI is not active.
fn install_panic_hook() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
    });
}

// ===========================================================================
// Input reader thread
// ===========================================================================

/// Crossterm event reader on a dedicated thread (crossterm's blocking API;
/// the workspace crossterm has no `event-stream` feature). Bounded shutdown:
/// the thread re-checks `stop` every [`INPUT_POLL`].
struct InputThread {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl InputThread {
    fn spawn() -> (Self, UnboundedReceiver<ct_event::Event>) {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let (tx, rx): (UnboundedSender<ct_event::Event>, _) =
            tokio::sync::mpsc::unbounded_channel();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match ct_event::poll(INPUT_POLL) {
                    Ok(true) => match ct_event::read() {
                        Ok(event) => {
                            if tx.send(event).is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
        });
        (
            InputThread {
                stop,
                handle: Some(handle),
            },
            rx,
        )
    }

    /// Stop the thread and wait for it (≤ [`INPUT_POLL`]) so it can never
    /// swallow keystrokes meant for the restored shell.
    async fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = handle.join();
            })
            .await;
        }
    }
}

// ===========================================================================
// The TUI loop
// ===========================================================================

type Tui = Terminal<CrosstermBackend<Stdout>>;

enum LoopEvent {
    Term(ct_event::Event),
    Tick,
    Done(Box<MissionEngine>, TurnOutput),
    InputClosed,
}

struct TuiRun {
    app: App,
    phase: Phase,
    renderer: EventRenderer,
    events_path: PathBuf,
    last_seq: u64,
    title: String,
    /// (width, height) of the transcript pane as of the last draw.
    dims: (u16, u16),
    quit: bool,
    /// Set on successful approval: the mission branch for the exit message.
    approved_branch: Option<String>,
    /// Set in post-approval mode when the user picks `y`: start execution
    /// immediately after teardown instead of exiting.
    run_now: bool,
}

/// Run the full-screen planning TUI to completion. Owns the whole
/// interaction: conversation turns, live activity, `/plan` + approval, the
/// post-approval "start execution now?" prompt, and the exit hints printed
/// AFTER the terminal is restored. Returns how planning ended — on
/// [`PlanningOutcome::ApprovedRun`] the caller starts execution on a fully
/// restored terminal (engine dropped, mission lock released).
pub async fn run(engine: MissionEngine, intro: String) -> Result<PlanningOutcome> {
    let mission_id = engine.mission_id().to_string();
    let title = format!(
        "KRANZ PLANNING — {} — {}",
        mission_id,
        engine.state().mission.goal
    );
    let events_path = engine.paths().events_file();
    let last_seq = engine.state().last_seq;
    // Color off: activity lines are styled by the TUI, not by ANSI codes.
    let renderer = EventRenderer::planning(engine.state(), false);

    install_panic_hook();
    let guard = enter_terminal()?;
    let terminal_result = Terminal::new(CrosstermBackend::new(std::io::stdout()));
    let mut terminal = match terminal_result {
        Ok(t) => t,
        Err(e) => {
            drop(guard);
            return Err(anyhow::Error::new(e).context("initializing the terminal"));
        }
    };
    let (input_thread, mut keys) = InputThread::spawn();

    let mut app = App::new();
    app.push(TranscriptEntry::Notice(intro));

    let mut state = TuiRun {
        app,
        phase: Phase::Idle(Box::new(engine)),
        renderer,
        events_path,
        last_seq,
        title,
        dims: (80, 20),
        quit: false,
        approved_branch: None,
        run_now: false,
    };

    let loop_result = state.run_loop(&mut terminal, &mut keys).await;

    // Teardown order matters: drop the in-flight/idle engine first (flushes
    // buffered events, kills any live session, releases the lock), then
    // restore the terminal, then reap the input thread — and only then print
    // the exit hints onto the restored main screen. A run-now choice starts
    // execution only after all of this: the mission lock is free again and
    // the live event feed prints onto the main screen, never the TUI's.
    let approved_branch = state.approved_branch.take();
    let run_now = state.run_now;
    let leftover: Vec<String> = std::iter::from_fn(|| state.app.queue.pop()).collect();
    drop(state); // drops Phase (and the engine, wherever it lives)
    drop(terminal);
    drop(guard);
    input_thread.shutdown().await;

    loop_result?;
    let outcome = match (&approved_branch, run_now) {
        (None, _) => PlanningOutcome::NotApproved,
        (Some(_), true) => PlanningOutcome::ApprovedRun,
        (Some(_), false) => PlanningOutcome::ApprovedExit,
    };
    match &approved_branch {
        Some(branch) if run_now => {
            println!("plan approved and committed on {branch}.");
        }
        Some(branch) => {
            println!("plan approved and committed on {branch}. run 'kranz run' to execute.");
        }
        None => {
            println!(
                "leaving planning; mission {mission_id} was not approved. \
                 Resume anytime with `kranz plan`."
            );
        }
    }
    // Queued-but-unsent messages must never vanish silently (the queue's
    // whole contract): planning ended before their turn came, so hand them
    // back to the user.
    if !leftover.is_empty() {
        println!(
            "note: {} queued message(s) were never sent (planning ended first):",
            leftover.len()
        );
        for message in &leftover {
            println!("  - {message}");
        }
    }
    Ok(outcome)
}

impl TuiRun {
    async fn run_loop(
        &mut self,
        terminal: &mut Tui,
        keys: &mut UnboundedReceiver<ct_event::Event>,
    ) -> Result<()> {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let view = PhaseView::of(&self.phase);
            terminal
                .draw(|frame| self.dims = draw_ui(frame, &self.app, &view, &self.title))
                .context("drawing the planning TUI")?;

            let event = match &mut self.phase {
                Phase::Busy { fut, .. } => tokio::select! {
                    maybe = keys.recv() => match maybe {
                        Some(e) => LoopEvent::Term(e),
                        None => LoopEvent::InputClosed,
                    },
                    _ = tick.tick() => LoopEvent::Tick,
                    (engine, out) = fut.as_mut() => LoopEvent::Done(engine, out),
                },
                _ => tokio::select! {
                    maybe = keys.recv() => match maybe {
                        Some(e) => LoopEvent::Term(e),
                        None => LoopEvent::InputClosed,
                    },
                    _ = tick.tick() => LoopEvent::Tick,
                },
            };

            match event {
                LoopEvent::InputClosed => self.quit = true,
                LoopEvent::Tick => self.on_tick(),
                LoopEvent::Term(e) => self.on_term_event(e),
                LoopEvent::Done(engine, out) => self.on_turn_done(engine, out),
            }

            if self.quit {
                return Ok(());
            }
        }
    }

    /// 250ms tick: advance the spinner and pull new activity lines from the
    /// event log (read errors are transient — the engine may be mid-write).
    fn on_tick(&mut self) {
        self.app.spinner = self.app.spinner.wrapping_add(1);
        if let Ok(events) = EventLog::read_events_after(&self.events_path, self.last_seq) {
            for event in &events {
                self.last_seq = event.seq;
                let line = self.renderer.render(event);
                if !line.is_empty() {
                    self.app.push(TranscriptEntry::Activity(line));
                }
            }
        }
    }

    fn on_term_event(&mut self, event: ct_event::Event) {
        match event {
            ct_event::Event::Key(key)
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                self.on_key(key.code, key.modifiers);
            }
            ct_event::Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_up(WHEEL_LINES),
                MouseEventKind::ScrollDown => self.scroll_down(WHEEL_LINES),
                _ => {}
            },
            _ => {}
        }
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Ctrl-C: the same clean exit path as /quit, in every mode. In
        // post-approval mode the plan stays committed — Ctrl-C just declines
        // the run-now offer.
        if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if matches!(self.phase, Phase::Approval { .. }) {
            self.on_approval_key(code);
            return;
        }
        if matches!(self.phase, Phase::PostApproval { .. }) {
            if let Some(outcome) = post_approval_key(code) {
                self.run_now = outcome == PlanningOutcome::ApprovedRun;
                self.quit = true;
            }
            return;
        }
        self.app.error = None;
        match (code, mods) {
            (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => {
                if self.app.editor.is_empty() {
                    self.quit = true; // Ctrl-D on an empty line = /quit
                } else {
                    self.app.editor.delete();
                }
            }
            (KeyCode::Char('a'), m) if m.contains(KeyModifiers::CONTROL) => self.app.editor.home(),
            (KeyCode::Char('e'), m) if m.contains(KeyModifiers::CONTROL) => self.app.editor.end(),
            (KeyCode::Char('u'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.app.editor.clear_line()
            }
            (KeyCode::Left, m) if m.contains(KeyModifiers::ALT) => self.app.editor.word_left(),
            (KeyCode::Right, m) if m.contains(KeyModifiers::ALT) => self.app.editor.word_right(),
            (KeyCode::Left, _) => self.app.editor.left(),
            (KeyCode::Right, _) => self.app.editor.right(),
            (KeyCode::Home, _) => self.app.editor.home(),
            (KeyCode::End, _) => {
                // End re-attaches a detached transcript; otherwise it is the
                // editor's end-of-line.
                if self.app.scroll.is_detached() {
                    self.app.scroll.to_follow();
                } else {
                    self.app.editor.end();
                }
            }
            (KeyCode::Up, _) => self.app.editor.history_up(),
            (KeyCode::Down, _) => self.app.editor.history_down(),
            (KeyCode::PageUp, _) => self.scroll_up(self.page()),
            (KeyCode::PageDown, _) => self.scroll_down(self.page()),
            (KeyCode::Backspace, _) => self.app.editor.backspace(),
            (KeyCode::Delete, _) => self.app.editor.delete(),
            (KeyCode::Enter, _) => self.on_submit(),
            (KeyCode::Char(c), m)
                if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
            {
                self.app.editor.insert(c);
            }
            _ => {}
        }
    }

    fn on_submit(&mut self) {
        let Some(line) = self.app.editor.submit() else {
            return;
        };
        let busy = matches!(self.phase, Phase::Busy { .. });
        match classify_submission(&line, busy) {
            SubmitDisposition::Quit => self.quit = true,
            SubmitDisposition::Unknown(cmd) => {
                self.app.push(TranscriptEntry::Notice(format!(
                    "unknown command {cmd}; use /plan or /quit"
                )));
            }
            SubmitDisposition::Queued(text) => {
                self.app.push(TranscriptEntry::User {
                    text: text.clone(),
                    queued: true,
                });
                self.app.queue.push(text);
            }
            SubmitDisposition::RequestPlan => {
                self.app.push(TranscriptEntry::User {
                    text: line,
                    queued: false,
                });
                self.phase = match std::mem::replace(&mut self.phase, Phase::Transitioning) {
                    Phase::Idle(engine) => start_plan_request(engine),
                    other => other,
                };
            }
            SubmitDisposition::Turn(text) => {
                self.app.push(TranscriptEntry::User {
                    text: text.clone(),
                    queued: false,
                });
                self.phase = match std::mem::replace(&mut self.phase, Phase::Transitioning) {
                    Phase::Idle(engine) => start_turn(engine, text),
                    other => other,
                };
            }
        }
    }

    fn on_turn_done(&mut self, mut engine: Box<MissionEngine>, out: TurnOutput) {
        // A seed turn may have run inside this turn (fresh session, resume
        // ack, or re-seed). Its reply — often the orchestrator's scoping
        // questions — happened first in the conversation, so it enters the
        // transcript before the turn's own output.
        if let Some(seed) = engine.take_seed_reply() {
            self.app.push(TranscriptEntry::Orch(seed));
        }
        match out {
            TurnOutput::Reply(Ok(text)) => {
                self.app.push(TranscriptEntry::Orch(text));
                self.phase = Phase::Idle(engine);
            }
            TurnOutput::Reply(Err(e)) => {
                let message = format!("{:#}", augment_limit_hint(e.into()));
                self.app.push(TranscriptEntry::Error(format!(
                    "orchestrator turn failed: {message}"
                )));
                self.app.error = Some(format!("orchestrator turn failed: {message}"));
                self.phase = Phase::Idle(engine);
            }
            TurnOutput::Plan(Ok(PlanRequest::NotReady(text))) => {
                // Conversational, not an error: show what the orchestrator
                // said and return to idle — no approval mode, no red.
                self.app.push(TranscriptEntry::Orch(text));
                self.app
                    .push(TranscriptEntry::Notice(PLAN_NOT_READY_NOTICE.to_string()));
                self.phase = Phase::Idle(engine);
            }
            TurnOutput::Plan(Ok(PlanRequest::Ready(plan))) => {
                self.app.push(TranscriptEntry::Block(
                    output::render_plan(&plan).trim_end().to_string(),
                ));
                // Estimate with params calibrated from this repo's completed
                // missions (built-in defaults when there are none yet).
                let calibration = cost::calibrate(&engine.paths().repo_root);
                let estimate = cost::estimate(&plan, &engine.state().config, &calibration.params);
                self.app
                    .push(TranscriptEntry::Block(output::render_cost_estimate(
                        &estimate,
                        calibration.missions_used,
                    )));
                self.phase = Phase::Approval { engine, plan };
                return; // queued messages wait for the approval decision
            }
            TurnOutput::Plan(Err(e)) => {
                let message = format!("{:#}", augment_limit_hint(e.into()));
                self.app.push(TranscriptEntry::Error(format!(
                    "plan request failed: {message}"
                )));
                self.app.error = Some(format!("plan request failed: {message}"));
                self.phase = Phase::Idle(engine);
            }
        }
        self.dispatch_queued();
    }

    fn on_approval_key(&mut self, code: KeyCode) {
        match approval_key(code) {
            ApprovalKey::Ignore => {}
            ApprovalKey::Approve => {
                self.phase = match std::mem::replace(&mut self.phase, Phase::Transitioning) {
                    Phase::Approval { mut engine, plan } => {
                        match engine.approve_plan(plan.clone()) {
                            Ok(()) => {
                                let branch = engine.state().mission.mission_branch.clone();
                                self.app.push(TranscriptEntry::Notice(format!(
                                    "plan approved and committed on {branch}."
                                )));
                                self.approved_branch = Some(branch);
                                // The commit is done; whether to start the
                                // spend is a separate explicit consent step.
                                Phase::PostApproval { _engine: engine }
                            }
                            Err(e) => {
                                self.app.push(TranscriptEntry::Error(format!(
                                    "plan approval failed: {e}"
                                )));
                                self.app.push(TranscriptEntry::Notice(
                                    "back to the conversation.".into(),
                                ));
                                Phase::Idle(engine)
                            }
                        }
                    }
                    other => other,
                };
                if self.approved_branch.is_none() {
                    self.dispatch_queued();
                }
            }
            ApprovalKey::Reject => {
                self.app.push(TranscriptEntry::Notice(
                    "not approved — back to the conversation.".into(),
                ));
                self.phase = match std::mem::replace(&mut self.phase, Phase::Transitioning) {
                    Phase::Approval { engine, .. } => Phase::Idle(engine),
                    other => other,
                };
                self.dispatch_queued();
            }
        }
    }

    /// Idle again: send the oldest queued message as the next turn (FIFO).
    fn dispatch_queued(&mut self) {
        if !matches!(self.phase, Phase::Idle(_)) {
            return;
        }
        while let Some(line) = self.app.queue.pop() {
            match classify_submission(&line, false) {
                SubmitDisposition::Turn(text) => {
                    self.phase = match std::mem::replace(&mut self.phase, Phase::Transitioning) {
                        Phase::Idle(engine) => start_turn(engine, text),
                        other => other,
                    };
                    return;
                }
                SubmitDisposition::RequestPlan => {
                    self.phase = match std::mem::replace(&mut self.phase, Phase::Transitioning) {
                        Phase::Idle(engine) => start_plan_request(engine),
                        other => other,
                    };
                    return;
                }
                // /quit and unknown commands never enter the queue; skip
                // defensively rather than wedge the drain.
                _ => continue,
            }
        }
    }

    /// One page = the transcript pane height (minus one line of overlap).
    fn page(&self) -> usize {
        (self.dims.1 as usize).saturating_sub(1).max(1)
    }

    fn scroll_up(&mut self, n: usize) {
        let (total, height) = self.scroll_geometry();
        self.app.scroll.scroll_up(n, total, height);
    }

    fn scroll_down(&mut self, n: usize) {
        let (total, height) = self.scroll_geometry();
        self.app.scroll.scroll_down(n, total, height);
    }

    fn scroll_geometry(&self) -> (usize, usize) {
        let width = (self.dims.0 as usize).max(1);
        let total = total_visual_lines(&self.app.transcript, width);
        (total, self.dims.1 as usize)
    }
}

// ===========================================================================
// Rendering
// ===========================================================================

/// Draw the four rows: title bar, transcript, status line, input line.
/// Returns the transcript pane's (width, height) for scroll math.
fn draw_ui(frame: &mut Frame, app: &App, view: &PhaseView, title: &str) -> (u16, u16) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    draw_title(frame, chunks[0], title);
    draw_transcript(frame, chunks[1], app);
    draw_status(frame, chunks[2], app, view);
    draw_input(frame, chunks[3], app, view);

    (chunks[1].width, chunks[1].height)
}

fn draw_title(frame: &mut Frame, area: Rect, title: &str) {
    let text = one_line(title, area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(
            text,
            Style::new().add_modifier(Modifier::BOLD),
        ))
        .style(Style::new().bg(Color::DarkGray).fg(Color::White)),
        area,
    );
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &App) {
    let width = (area.width as usize).max(1);
    let height = area.height as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for entry in &app.transcript {
        entry_lines(entry, width, &mut lines);
    }
    let total = lines.len();
    let top = app.scroll.top(total, height);
    let bottom = (top + height).min(total);
    let visible: Vec<Line<'static>> = lines[top..bottom].to_vec();
    frame.render_widget(Paragraph::new(visible), area);

    // Detached scrollback: a dim marker on the bottom row shows the way back.
    if app.scroll.is_detached() && bottom < total && height > 0 {
        let marker = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                DETACHED_MARKER,
                Style::new().fg(Color::DarkGray),
            )),
            marker,
        );
    }
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App, view: &PhaseView) {
    let paragraph = match view {
        PhaseView::Approval => Paragraph::new(Span::styled(
            APPROVAL_BAR,
            Style::new().add_modifier(Modifier::BOLD),
        ))
        .style(Style::new().bg(Color::Yellow).fg(Color::Black)),
        PhaseView::PostApproval => Paragraph::new(Span::styled(
            POST_APPROVAL_BAR,
            Style::new().add_modifier(Modifier::BOLD),
        ))
        .style(Style::new().bg(Color::Yellow).fg(Color::Black)),
        PhaseView::Busy { kind, elapsed_secs } => Paragraph::new(Span::styled(
            busy_status_line(*kind, *elapsed_secs, app.spinner, app.queue.depth()),
            Style::new().fg(Color::Yellow),
        )),
        PhaseView::Idle => match &app.error {
            Some(error) => Paragraph::new(Span::styled(
                format!(
                    "✖ {}",
                    one_line(error, (area.width as usize).saturating_sub(2))
                ),
                Style::new().fg(Color::Red),
            )),
            None => Paragraph::new(Span::styled(IDLE_STATUS, Style::new().fg(Color::Green))),
        },
    };
    frame.render_widget(paragraph, area);
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App, view: &PhaseView) {
    if matches!(view, PhaseView::Approval | PhaseView::PostApproval) {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "(input paused — press y or n)",
                Style::new().fg(Color::DarkGray),
            )),
            area,
        );
        return; // no cursor: the input line is inactive in these modes
    }
    let prompt = "> ";
    let window = (area.width as usize).saturating_sub(prompt.len()).max(1);
    let chars: Vec<char> = app.editor.text().chars().collect();
    let cursor = app.editor.cursor();
    let start = if cursor >= window {
        cursor + 1 - window
    } else {
        0
    };
    let end = (start + window).min(chars.len());
    let visible: String = chars[start.min(chars.len())..end].iter().collect();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prompt, Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(visible),
        ])),
        area,
    );
    let x = area.x + (prompt.len() + (cursor - start)) as u16;
    frame.set_cursor_position(Position::new(x.min(area.right().saturating_sub(1)), area.y));
}
