//! Bounded readers for backend child-process streams (and the local
//! backend's response body) — the fix for the hostile-workload finding that
//! every CLI backend accumulated unbounded stdout/stderr in memory (a
//! backend emitting a 10GB single line, or endless output with no newline,
//! could hang the engine or exhaust host memory).
//!
//! The house pattern is `command_exec::read_stream_tail`: keep the TAIL,
//! never the head, and keep draining so a full pipe never deadlocks the
//! child. Truncation is marked, and the marker is a SUFFIX so it survives
//! the last-N-chars tailing (`STDERR_TAIL_CHARS` / `BODY_TAIL_CHARS`) that
//! failure messages apply when surfacing the text.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

/// Suffix appended to a retained tail when earlier bytes were dropped.
pub const TRUNCATION_MARKER: &str = "[...truncated; tail kept...]";

/// Max bytes retained from one stdout protocol line. Real stream-json event
/// lines are KiB-scale even with embedded tool output, so an 8 MiB cap only
/// trips on a pathological CLI — where the truncated tail fails JSON parsing
/// and flows through as an `AgentEvent::Other`, leaving session-outcome
/// semantics unchanged.
pub const STDOUT_LINE_CAP: usize = 8 * 1024 * 1024;

/// Max bytes retained from a backend's whole stderr stream. Failure messages
/// surface only the last `STDERR_TAIL_CHARS` (500) of it; 64 KiB leaves
/// generous headroom while bounding a hostile stream.
pub const STDERR_TAIL_CAP: usize = 64 * 1024;

/// Bounded tail-keeping window over a byte stream: memory stays ≤ 2×`cap`
/// no matter how much is pushed, and once anything is dropped the window is
/// marked truncated so the rendered string carries [`TRUNCATION_MARKER`].
/// `cap` must be non-zero (all call sites use the constants above).
///
/// Ring-by-offset, not front-drain: dropping the head advances `start` and
/// the dead prefix is compacted only once it exceeds `cap` — amortized
/// O(1) per pushed byte. The naive `drain(..excess)` per chunk shifted the
/// whole window on every push (~1 TiB of memcpy for a 1 GiB hostile line —
/// 4th-pass review).
pub struct TailWindow {
    // Lazy growth, NOT `Vec::with_capacity(cap)`: a `BoundedLines` window is
    // allocated per line, and pre-allocating STDOUT_LINE_CAP per line would
    // waste 8 MiB on every KiB-scale line.
    buf: Vec<u8>,
    /// The retained tail begins here; the logical content is `buf[start..]`.
    start: usize,
    cap: usize,
    truncated: bool,
}

impl TailWindow {
    pub fn new(cap: usize) -> Self {
        debug_assert!(cap > 0, "a zero-cap tail window retains nothing");
        TailWindow {
            buf: Vec::new(),
            start: 0,
            cap,
            truncated: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.cap {
            self.buf.clear();
            self.buf.extend_from_slice(&bytes[bytes.len() - self.cap..]);
            self.start = 0;
            self.truncated = true;
            return;
        }
        self.buf.extend_from_slice(bytes);
        let excess = (self.buf.len() - self.start).saturating_sub(self.cap);
        if excess > 0 {
            self.start += excess;
            self.truncated = true;
            // Amortized compaction: drop the dead prefix only once it alone
            // exceeds the cap, so steady-state cost is O(1) per byte.
            if self.start > self.cap {
                self.buf.drain(..self.start);
                self.start = 0;
            }
        }
    }

    /// The retained tail bytes (`buf[start..]`).
    fn tail(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    fn is_empty(&self) -> bool {
        self.tail().is_empty() && !self.truncated
    }

    /// The retained tail as text (`from_utf8_lossy` keeps a leading partial
    /// UTF-8 sequence safe), with the marker appended once anything was
    /// dropped. Marker on its own trailing line, so `.trim_end()` +
    /// last-N-chars surfacing keeps it visible.
    pub fn render(&self) -> String {
        let tail = String::from_utf8_lossy(self.tail());
        if self.truncated {
            format!("{tail}\n{TRUNCATION_MARKER}")
        } else {
            tail.into_owned()
        }
    }

    /// [`Self::render`] for one line: the terminator (`\n`, then a trailing `\r`)
    /// is stripped first — the same shape `tokio::io::Lines::next_line`
    /// returns — and the marker stays on the line itself.
    fn render_line(&self) -> String {
        let mut bytes: &[u8] = self.tail();
        if bytes.last() == Some(&b'\n') {
            bytes = &bytes[..bytes.len() - 1];
        }
        if bytes.last() == Some(&b'\r') {
            bytes = &bytes[..bytes.len() - 1];
        }
        let text = String::from_utf8_lossy(bytes);
        if self.truncated {
            format!("{text} {TRUNCATION_MARKER}")
        } else {
            text.into_owned()
        }
    }
}

/// Drain `reader` to EOF, retaining only the bounded tail. Backend stderr
/// capture runs here: the pipe must be drained to the end no matter how much
/// the child writes (a full pipe would deadlock the child), while retained
/// memory stays bounded. The tail (with marker) is returned once, at EOF —
/// the stderr-capture tasks publish it then, and every reader of that buffer
/// runs after the capture task is joined; incremental publication would
/// multiply memcpy on a hostile multi-GB stream.
pub async fn drain_to_tail<R>(mut reader: R, cap: usize) -> String
where
    R: AsyncRead + Unpin,
{
    let mut window = TailWindow::new(cap);
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            // A read error ends capture exactly like the old
            // `while let Ok(Some(line))` loop did: keep what we have.
            Ok(0) | Err(_) => break,
            Ok(n) => window.push(&chunk[..n]),
        }
    }
    window.render()
}

/// Newline-delimited reader over a child stdout that bounds per-line memory:
/// a line longer than the cap is still drained to its newline (the child
/// never blocks on a full pipe) but only its tail is retained, suffixed with
/// [`TRUNCATION_MARKER`]. Drop-in replacement for `BufReader::lines()` in the
/// CLI backend sessions.
pub struct BoundedLines<R> {
    reader: BufReader<R>,
    cap: usize,
    strict: bool,
    window: TailWindow,
}

impl<R: AsyncRead + Unpin> BoundedLines<R> {
    pub fn new(inner: R) -> Self {
        Self::with_cap(inner, STDOUT_LINE_CAP)
    }

    /// Protocols with authority-bearing JSON reject oversized lines and
    /// invalid UTF-8 instead of accepting a lossy or truncated rendering.
    pub fn new_strict(inner: R) -> Self {
        let mut lines = Self::new(inner);
        lines.strict = true;
        lines
    }

    /// Explicit cap, separated so tests can exercise truncation without
    /// streaming 8 MiB (mirrors `run_shell_command_with_timeout`).
    pub fn with_cap(inner: R, cap: usize) -> Self {
        BoundedLines {
            reader: BufReader::new(inner),
            cap,
            strict: false,
            window: TailWindow::new(cap),
        }
    }

    /// The next line without its terminator (`\n`, and a trailing `\r` —
    /// the same shape as `tokio::io::Lines::next_line`); `None` at EOF. A
    /// final unterminated line is still returned. Unlike `Lines`, invalid
    /// UTF-8 is lossy-converted rather than an error (strictly more
    /// tolerant; the unparsed-line path handles it downstream).
    /// Partial bytes survive cancellation (e.g. an ACP permission arriving
    /// while stdout is fragmented). Only a completed line resets the window.
    pub async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.strict {
                    std::str::from_utf8(self.window.tail())
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                }
                // EOF: retained bytes are one last unterminated line; a
                // pristine window is a clean end of stream.
                let line = (!self.window.is_empty()).then(|| self.window.render_line());
                self.window = TailWindow::new(self.cap);
                return Ok(line);
            }
            let (take, found_newline) = match available.iter().position(|b| *b == b'\n') {
                Some(pos) => (pos + 1, true),
                None => (available.len(), false),
            };
            self.window.push(&available[..take]);
            self.reader.consume(take);
            if self.strict && self.window.truncated {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "protocol line exceeded its byte limit",
                ));
            }
            if found_newline {
                if self.strict {
                    std::str::from_utf8(self.window.tail())
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                }
                let line = self.window.render_line();
                self.window = TailWindow::new(self.cap);
                return Ok(Some(line));
            }
        }
    }
    pub fn has_partial_line(&self) -> bool {
        !self.window.is_empty()
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_lines_keeps_fragment_and_cap_after_cancelled_read() {
        use std::future::{poll_fn, Future};
        use std::task::Poll;
        use tokio::io::AsyncWriteExt;
        for strict in [false, true] {
            let (reader, mut writer) = tokio::io::duplex(64);
            let mut lines = BoundedLines::with_cap(reader, 16);
            lines.strict = strict;
            writer.write_all(b"first half").await.unwrap();
            {
                let read = lines.next_line();
                tokio::pin!(read);
                poll_fn(|cx| {
                    assert!(read.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
            }
            assert!(lines.has_partial_line());
            writer.write_all(b" and more bytes\nnext\n").await.unwrap();
            let first = lines.next_line().await;
            if strict {
                assert_eq!(first.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
            } else {
                let first = first.unwrap().unwrap();
                assert!(first.contains("more bytes"));
                assert!(first.contains(TRUNCATION_MARKER));
                assert!(!lines.has_partial_line());
                assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("next"));
            }
        }
    }

    #[tokio::test]
    async fn bounded_lines_keeps_complete_json_after_cancelled_read() {
        use std::future::{poll_fn, Future};
        use std::task::Poll;
        use tokio::io::AsyncWriteExt;
        let (reader, mut writer) = tokio::io::duplex(64);
        let mut lines = BoundedLines::new_strict(reader);
        writer.write_all(b"{\"action\":").await.unwrap();
        {
            let read = lines.next_line();
            tokio::pin!(read);
            poll_fn(|cx| {
                assert!(read.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        writer.write_all(b"\"changed\"}\n").await.unwrap();
        assert_eq!(
            lines.next_line().await.unwrap().as_deref(),
            Some(r#"{"action":"changed"}"#)
        );
        assert!(!lines.has_partial_line());
        drop(writer);
        assert_eq!(lines.next_line().await.unwrap(), None);
    }
    #[test]
    fn tail_window_keeps_everything_under_the_cap() {
        let mut window = TailWindow::new(16);
        window.push(b"hello ");
        window.push(b"world");
        assert_eq!(window.render(), "hello world");
    }

    /// 4th-pass review: many small pushes over a saturated window must not
    /// grow memory unboundedly (≤ 2×cap before amortized compaction) and
    /// must keep the exact tail — the shape that made front-drain
    /// quadratic.
    #[test]
    fn tail_window_stays_bounded_and_exact_over_many_small_pushes() {
        let mut window = TailWindow::new(64);
        // 10k 8-byte pushes = 80 KiB through a 64-byte window.
        for i in 0..10_000u32 {
            window.push(format!("{i:08}").as_bytes());
        }
        assert!(
            window.buf.len() <= 128,
            "compaction must bound the buffer at 2x cap, got {}",
            window.buf.len()
        );
        assert!(window.start <= window.buf.len());
        assert_eq!(window.tail().len(), 64);
        assert!(
            window.tail().ends_with(b"00009999"),
            "the exact last bytes are retained: {:?}",
            window.tail()
        );
        assert!(window.truncated);
    }

    #[test]
    fn tail_window_keeps_the_tail_with_a_marker_once_over_the_cap() {
        let mut window = TailWindow::new(8);
        window.push(b"0123456789");
        assert_eq!(window.render(), "23456789\n[...truncated; tail kept...]");

        // Rollover across several small pushes marks truncation too.
        let mut window = TailWindow::new(8);
        window.push(b"aaaa");
        window.push(b"bbbb");
        window.push(b"cc");
        assert_eq!(window.render(), "aabbbbcc\n[...truncated; tail kept...]");
    }

    #[tokio::test]
    async fn bounded_lines_reads_small_lines_like_tokio_lines() {
        let input: &[u8] = b"one\n\r\nthree\r\nfour-no-newline";
        let mut lines = BoundedLines::with_cap(input, 64);
        assert_eq!(lines.next_line().await.unwrap(), Some("one".to_string()));
        assert_eq!(lines.next_line().await.unwrap(), Some("".to_string()));
        assert_eq!(lines.next_line().await.unwrap(), Some("three".to_string()));
        assert_eq!(
            lines.next_line().await.unwrap(),
            Some("four-no-newline".to_string())
        );
        assert_eq!(lines.next_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn bounded_lines_truncates_an_over_long_line_and_keeps_reading() {
        // 30 x's + '\n' against a 16-byte cap: the returned line is the
        // 15-byte tail plus the marker, and the NEXT line parses cleanly.
        let input = format!("{}\nshort\n", "x".repeat(30));
        let mut lines = BoundedLines::with_cap(input.as_bytes(), 16);

        let long = lines.next_line().await.unwrap().expect("first line");
        assert_eq!(
            long,
            format!("{} [...truncated; tail kept...]", "x".repeat(15))
        );
        assert_eq!(
            lines.next_line().await.unwrap(),
            Some("short".to_string()),
            "the line after a truncated one is unaffected"
        );
        assert_eq!(lines.next_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn bounded_lines_bounds_an_endless_line_with_no_newline() {
        // 100 KiB with no newline (spanning many fill_buf chunks, exercising
        // window rollover): one bounded line at EOF, then None.
        let input = vec![b'y'; 100 * 1024];
        let mut lines = BoundedLines::with_cap(input.as_slice(), 32);

        let line = lines.next_line().await.unwrap().expect("the one line");
        assert_eq!(
            line,
            format!("{} [...truncated; tail kept...]", "y".repeat(32))
        );
        assert_eq!(lines.next_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn drain_to_tail_keeps_small_streams_exact() {
        let tail = drain_to_tail(&b"all of it\n"[..], 1024).await;
        assert_eq!(tail, "all of it\n");
    }

    #[tokio::test]
    async fn drain_to_tail_caps_large_streams_with_a_marker() {
        let mut input = vec![b'z'; 100 * 1024];
        input.extend_from_slice(b"THE-END\n");
        let tail = drain_to_tail(input.as_slice(), 64).await;
        assert!(
            tail.ends_with("THE-END\n\n[...truncated; tail kept...]"),
            "{tail}"
        );
        assert_eq!(tail.len(), 64 + "\n[...truncated; tail kept...]".len());
    }
}
