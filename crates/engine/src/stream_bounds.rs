//! Shared bounded stream readers; semantics and limits live in kranz-acp.
#[cfg(test)]
pub(crate) use kranz_acp::io::TRUNCATION_MARKER;
pub(crate) use kranz_acp::io::{
    drain_to_tail, BoundedLines, TailWindow, STDERR_TAIL_CAP, STDOUT_LINE_CAP,
};
