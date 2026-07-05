//! Pure, deterministic event-to-span mapping primitives.
//!
//! No OTLP or network types leak in here — this module is unit-testable in
//! isolation. The actual event→`MissionSpan` reduction (walking a mission's
//! event log) lands in a later feature; this one delivers the neutral span
//! record and deterministic id derivation it depends on.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

/// A single OTLP-agnostic span attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    String(String),
    I64(i64),
    F64(f64),
}

/// Span outcome, independent of any OTLP status code encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanStatus {
    Unset,
    Ok,
    Error(String),
}

/// A neutral, transport-agnostic span record.
///
/// Attribute ordering is caller-determined and preserved (a `Vec`, not a
/// map) so mapping code can emit attributes in a deterministic order.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionSpan {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: Option<[u8; 8]>,
    pub name: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub attributes: Vec<(String, AttrValue)>,
    pub status: SpanStatus,
}

/// Deterministic trace id for a mission: first 16 bytes of sha256(mission_id).
pub fn trace_id(mission_id: &str) -> [u8; 16] {
    let digest = Sha256::digest(mission_id.as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// Deterministic span id for a mission/opening-seq pair: first 8 bytes of
/// sha256("{mission_id}:{open_seq}"), where `open_seq` is the seq of the
/// span's opening event (mission.created / milestone.started /
/// worker.spawned).
pub fn span_id(mission_id: &str, open_seq: u64) -> [u8; 8] {
    let digest = Sha256::digest(format!("{mission_id}:{open_seq}").as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_deterministic_and_idempotent() {
        let t1 = trace_id("m-01");
        let t2 = trace_id("m-01");
        assert_eq!(t1, t2, "trace_id must be idempotent for the same mission_id");
        assert_eq!(t1.len(), 16);

        let s1 = span_id("m-01", 42);
        let s2 = span_id("m-01", 42);
        assert_eq!(s1, s2, "span_id must be idempotent for the same (mission_id, seq)");
        assert_eq!(s1.len(), 8);

        let t_other = trace_id("m-02");
        assert_ne!(t1, t_other, "distinct missions must get distinct trace ids");

        let s_other_seq = span_id("m-01", 43);
        assert_ne!(s1, s_other_seq, "distinct seqs must get distinct span ids");

        let s_other_mission = span_id("m-02", 42);
        assert_ne!(s1, s_other_mission, "distinct missions must get distinct span ids even with the same seq");
    }
}
