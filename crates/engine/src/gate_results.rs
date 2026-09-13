//! Persisting gate evaluations as first-class `gate.result` events, and
//! resolving the artefact references those events carry (ticket
//! `.kranz/tickets/gate-results-first-class-events`, KRZ-312 — the
//! governance evidence layer's last substrate gap before provenance-replay).
//!
//! Two halves, deliberately small:
//!
//! 1. **Emission shape** — [`gate_result_events`] converts one evaluated
//!    [`crate::gate::GatePipeline`]'s reports into `gate.result` payloads, one event per
//!    gate, in pipeline order, assigning each gate its ladder position
//!    (zero-based index within its kind/section). This is the ONLY place
//!    that mapping lives, so the approval and final-gate surfaces can never
//!    drift apart in how they number the ladder.
//! 2. **Resolution** — [`resolve_artefact`] classifies a stored artefact
//!    reference against the mission dir as resolved/unresolved WITHOUT ever
//!    failing: the ticket's discipline is that a reference whose bytes are
//!    gone (a cleaned `runs/`, a discarded scratch checkout) resolves to
//!    "unresolved", never to an error that blocks replay.
//!
//! WHY the reference shape has a scheme at all: today's gates produce
//! gate-local handles — a description (`contract gate vacuous-filter`), a
//! command line, a tracked suite path — whose evidence is inherently textual
//! and travels in the event payload itself. When the evidence IS a file, the
//! reference must say so unambiguously or a resolver cannot tell
//! "cargo test --workspace" from a path; `file:` marks a mission-relative
//! path (e.g. `file:runs/r-1.jsonl`, the same relative shape
//! [`crate::paths::MissionPaths::transcript_rel`] records). Anything without
//! the scheme is the gate-local handle verbatim — the ticket's "inherently
//! textual" case — and classifies as [`ArtefactResolution::Inline`]: there
//! are no bytes to lose, the event payload IS the evidence.
//!
//! WHY mission-relative, never absolute (ticket text): an absolute host path
//! makes the log unreadable on any other machine and leaks the host layout
//! into the audit record; the mission dir is the anchor every reader already
//! has. References pointing outside it (`..`, absolute) can never resolve
//! honestly, so they classify as unresolved rather than erroring.
//!
//! Scrubbing: nothing here truncates or redacts. The event-log append
//! boundary already scans and redacts every string payload
//! (`EventLog::append_redacting`), so captured output reaches the log
//! scrubbed exactly like `orchestrator.decision` details do.

use crate::events::EventKind;
use crate::gate::{GateKind, GateReport, GateSurface};
use std::path::{Component, Path, PathBuf};

/// The scheme marking an artefact reference as a mission-relative file path
/// (`file:runs/r-1.jsonl`). References without it are gate-local handles —
/// the evidence is textual and lives in the event payload.
pub const FILE_REF_SCHEME: &str = "file:";

/// Build a file-backed artefact reference from a mission-relative path.
/// Callers pass the relative path (the `runs/<id>.jsonl` idiom); the scheme
/// is glued on here so the marker exists in exactly one spelling.
pub fn file_artefact_ref(mission_relative: &str) -> String {
    format!("{FILE_REF_SCHEME}{mission_relative}")
}

/// The resolution of a stored artefact reference against the mission dir.
/// A classification, never an error: every constructor path through
/// [`resolve_artefact`] is total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtefactResolution {
    /// A `file:` reference whose mission-relative bytes are present; the
    /// payload is the absolute path to read them from.
    Resolved { path: PathBuf },
    /// A `file:` reference whose bytes are gone (a cleaned `runs/`, a pruned
    /// mission, a discarded scratch checkout) — or whose path could never
    /// resolve honestly inside a mission dir (absolute, `..`, symlinked).
    /// The payload is the path that was probed, for diagnostics.
    Unresolved { path: PathBuf },
    /// No `file:` scheme: the reference is the gate-local handle itself (a
    /// command line, a description, a tracked suite path). Its evidence is
    /// textual and travels in the event payload — there is nothing on disk
    /// to re-find, so there is nothing that can go missing.
    Inline,
}

/// Classify a stored artefact reference against `mission_dir`. Total by
/// construction: missing bytes, unreadable entries, escape-shaped paths,
/// and io errors all classify as [`ArtefactResolution::Unresolved`] (or
/// [`ArtefactResolution::Inline`] when there is nothing to resolve) — a
/// replay over a pruned mission must degrade to "evidence no longer on
/// disk", never fail.
///
/// A symlinked artefact classifies as unresolved rather than being followed:
/// the mission tree is no-follow territory (P1 mission-path-no-follow), and
/// the resolver only ever classifies — readers re-open through the pinned
/// mission capability when they need the bytes.
pub fn resolve_artefact(mission_dir: &Path, reference: &str) -> ArtefactResolution {
    let Some(relative) = reference.strip_prefix(FILE_REF_SCHEME) else {
        return ArtefactResolution::Inline;
    };
    let rel_path = Path::new(relative);
    // Only Normal/CurDir components can name something inside the mission
    // dir; RootDir/Prefix/ParentDir are escape shapes that must never be
    // joined and probed.
    let honest = !relative.is_empty()
        && rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    let path = mission_dir.join(rel_path);
    let resolved = honest
        && std::fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_file())
            .unwrap_or(false);
    if resolved {
        ArtefactResolution::Resolved { path }
    } else {
        ArtefactResolution::Unresolved { path }
    }
}

/// Convert one evaluated pipeline's reports into `gate.result` payloads: one
/// event per gate, in pipeline (evaluation) order, each carrying its ladder
/// position. The index counts WITHIN the gate's kind/section — registration
/// order is evaluation order per section (gate.rs) — so (surface, kind,
/// index) names one evaluation exactly and sorting a replayed ladder on it
/// reproduces pipeline order.
///
/// The reports' artefact strings pass through verbatim: gate-local handles
/// stay gate-local (the textual case), and a gate that names a file uses
/// [`file_artefact_ref`] at construction so the scheme survives into the
/// log.
pub fn gate_result_events(surface: GateSurface, reports: &[GateReport]) -> Vec<EventKind> {
    let mut deterministic_index = 0u32;
    let mut model_judged_index = 0u32;
    reports
        .iter()
        .map(|report| {
            let index = match report.kind {
                GateKind::Deterministic => {
                    let index = deterministic_index;
                    deterministic_index += 1;
                    index
                }
                GateKind::ModelJudged => {
                    let index = model_judged_index;
                    model_judged_index += 1;
                    index
                }
            };
            EventKind::GateResult {
                gate: report.name.clone(),
                surface,
                kind: report.kind,
                index,
                verdict: report.outcome.verdict,
                artefact_ref: report.outcome.artefact.reference.clone(),
                artefact_detail: report.outcome.artefact.detail.clone(),
                score: report.outcome.score.map(|score| score.score),
                threshold: report.outcome.score.map(|score| score.threshold),
                rule_ids: report.outcome.rule_ids.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{ArtefactRef, GateOutcome, GateVerdict};

    fn report(name: &str, kind: GateKind, verdict: GateVerdict) -> GateReport {
        let outcome = match verdict {
            GateVerdict::Pass => GateOutcome::pass(ArtefactRef::new(format!("ref {name}"))),
            GateVerdict::Fail => {
                GateOutcome::fail(ArtefactRef::new(format!("ref {name}")).with_detail("boom"))
            }
        };
        GateReport {
            name: name.to_string(),
            kind,
            outcome,
        }
    }

    /// The ladder mapping: one event per report in pipeline order, with the
    /// index counting within each kind/section — a model-judged gate's index
    /// restarts at zero even when deterministic gates precede it (the two
    /// sections are numbered independently, exactly as GatePipeline stores
    /// them).
    #[test]
    fn gate_result_events_assign_per_section_indices_in_pipeline_order() {
        let mut det_pass = report("det-a", GateKind::Deterministic, GateVerdict::Pass);
        det_pass.outcome.score = Some(crate::gate::GateScore {
            score: 0.9,
            threshold: 0.5,
        });
        let reports = vec![
            det_pass,
            report("det-b", GateKind::Deterministic, GateVerdict::Fail),
            report("model-a", GateKind::ModelJudged, GateVerdict::Pass),
        ];

        let events = gate_result_events(GateSurface::FinalGate, &reports);
        assert_eq!(events.len(), 3);
        let shape: Vec<(String, GateKind, u32, GateVerdict)> = events
            .iter()
            .map(|event| match event {
                EventKind::GateResult {
                    gate,
                    kind,
                    index,
                    verdict,
                    ..
                } => (gate.clone(), *kind, *index, *verdict),
                _ => panic!("wrong variant"),
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    "det-a".to_string(),
                    GateKind::Deterministic,
                    0,
                    GateVerdict::Pass
                ),
                (
                    "det-b".to_string(),
                    GateKind::Deterministic,
                    1,
                    GateVerdict::Fail
                ),
                (
                    "model-a".to_string(),
                    GateKind::ModelJudged,
                    0,
                    GateVerdict::Pass
                ),
            ]
        );
        // Verbatim passthrough: artefact handle, captured detail, and the
        // score pair arrive exactly as the gate stated them.
        match &events[0] {
            EventKind::GateResult {
                artefact_ref,
                artefact_detail,
                score,
                threshold,
                ..
            } => {
                assert_eq!(artefact_ref, "ref det-a");
                assert_eq!(*artefact_detail, None);
                assert_eq!(*score, Some(0.9));
                assert_eq!(*threshold, Some(0.5));
            }
            _ => panic!("wrong variant"),
        }
        match &events[1] {
            EventKind::GateResult {
                artefact_detail, ..
            } => assert_eq!(artefact_detail.as_deref(), Some("boom")),
            _ => panic!("wrong variant"),
        }
    }

    /// An empty pipeline emits no events (the no-command-assertions case at
    /// approval, or no contract + no pack at the final gate).
    #[test]
    fn gate_result_events_empty_pipeline_emits_nothing() {
        assert!(gate_result_events(GateSurface::Approval, &[]).is_empty());
    }

    /// A `file:` reference whose mission-relative bytes exist resolves —
    /// and the payload path is under the mission dir.
    #[test]
    fn gate_result_event_file_ref_resolves_when_bytes_exist() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mission_dir = tmp.path();
        std::fs::create_dir_all(mission_dir.join("runs")).unwrap();
        std::fs::write(mission_dir.join("runs").join("r-1.jsonl"), b"{}").unwrap();

        let resolved = resolve_artefact(mission_dir, &file_artefact_ref("runs/r-1.jsonl"));
        match resolved {
            ArtefactResolution::Resolved { path } => {
                assert_eq!(path, mission_dir.join("runs/r-1.jsonl"))
            }
            other => panic!("expected resolved, got {other:?}"),
        }
    }

    /// The ticket's central discipline: a reference whose bytes are gone
    /// (the runs/ dir was cleaned, the mission pruned) reports UNRESOLVED —
    /// a classification, never an error.
    #[test]
    fn gate_result_event_file_ref_is_unresolved_when_bytes_are_gone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mission_dir = tmp.path();
        // The mission dir exists but runs/ was cleaned: nothing to find.
        assert_eq!(
            resolve_artefact(mission_dir, &file_artefact_ref("runs/r-1.jsonl")),
            ArtefactResolution::Unresolved {
                path: mission_dir.join("runs/r-1.jsonl")
            }
        );
        // A directory at the referenced path is not evidence bytes either.
        std::fs::create_dir_all(mission_dir.join("runs")).unwrap();
        assert!(matches!(
            resolve_artefact(mission_dir, &file_artefact_ref("runs")),
            ArtefactResolution::Unresolved { .. }
        ));
    }

    /// Escape-shaped references (absolute, parent-traversing, empty) can
    /// never resolve honestly inside a mission dir: unresolved, and the
    /// probe never touches the filesystem outside the anchor.
    #[test]
    fn gate_result_event_file_ref_escape_shapes_are_unresolved() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mission_dir = tmp.path();
        for reference in [
            file_artefact_ref("../outside.jsonl"),
            file_artefact_ref("runs/../../escape"),
            file_artefact_ref("/etc/passwd"),
            file_artefact_ref(""),
        ] {
            assert!(
                matches!(
                    resolve_artefact(mission_dir, &reference),
                    ArtefactResolution::Unresolved { .. }
                ),
                "{reference} must classify unresolved"
            );
        }
    }

    /// Gate-local handles (a command line, a description, a tracked suite
    /// path) carry no scheme: the evidence is textual and lives in the event
    /// payload, so there is nothing on disk that could go missing.
    #[test]
    fn gate_result_event_textual_ref_is_inline() {
        let tmp = tempfile::TempDir::new().unwrap();
        for reference in [
            "contract gate vacuous-filter",
            "cargo test --workspace",
            ".kranz/merge-gates.json",
        ] {
            assert_eq!(
                resolve_artefact(tmp.path(), reference),
                ArtefactResolution::Inline,
                "{reference} must classify inline"
            );
        }
    }

    /// A symlinked artefact is unresolved, never followed — the mission
    /// tree's no-follow posture applies to evidence reads too.
    #[cfg(unix)]
    #[test]
    fn gate_result_event_symlinked_artefact_is_unresolved() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let mission_dir = tmp.path().join("mission");
        std::fs::create_dir_all(mission_dir.join("runs")).unwrap();
        let elsewhere = tmp.path().join("elsewhere.jsonl");
        std::fs::write(&elsewhere, b"{}").unwrap();
        symlink(&elsewhere, mission_dir.join("runs").join("r-1.jsonl")).unwrap();

        assert!(matches!(
            resolve_artefact(&mission_dir, &file_artefact_ref("runs/r-1.jsonl")),
            ArtefactResolution::Unresolved { .. }
        ));
    }
}
