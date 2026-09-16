//! Evidence bundle export (ticket `.kranz/tickets/evidence-bundle-export.md`,
//! KRZ-326 — the governance evidence layer's packaging step): assemble ONE
//! mission's portable audit package — inputs, gate results, diffs, reviewers,
//! escalations, cost, and the provenance chain — self-contained and suitable
//! for handing to an auditor who has no access to the repo.
//!
//! The bundle is a plain DIRECTORY, not an archive:
//!
//! ```text
//! <out>/
//!   manifest.json     — machine index: every entry with its sha256 + source
//!   summary.md        — the human-readable audit summary
//!   chain.json        — the provenance chain (provenance-replay's machine form)
//!   escalations.json  — the mission's escalation-ledger rows
//!   cost.json         — the mission's cost fold
//!   events.jsonl      — the raw (already-scrubbed) event log, verbatim
//!   artefacts/…       — bytes for every resolvable `file:` artefact ref,
//!                       plus the well-known mission documents (plan, report)
//! ```
//!
//! WHY a directory and not a `.tar`: neither `tar` nor `zip` is anywhere in
//! the dependency tree, and the ticket blesses the directory form — it is
//! also the MORE auditable container: every entry greps, diffs, and opens in
//! any tool with no extraction step, and there is no archive metadata
//! (mtimes, uid/gid, ordering) whose normalization would be a second
//! determinism surface. Determinism is therefore ENTRY identity: the same
//! log yields the same (relative-path → bytes) set, byte for byte. Nothing in
//! assembly consults a clock, a hash map, or a host path — the log's own
//! event timestamps travel as DATA (escalation rows), which is exactly what
//! "same log → same bundle" requires.
//!
//! The substrate's own rules, kept:
//!
//! - **Everything derives from the already-scrubbed log.** The bundle never
//!   reintroduces scrubbed values: `events.jsonl` crossed the redact-at-write
//!   boundary when it was appended, and every derived file folds FROM it.
//!   A log carrying `secret.redacted` audits yields a bundle with
//!   fingerprints only (test-pinned). Artefact bytes are the one half that
//!   did NOT cross a write boundary the engine controls — they are ordinary
//!   files in a worker-writable tree — so [`read_artefact`] scrubs them here,
//!   as text, and the manifest digests the redacted form (audit H5).
//! - **Missing evidence is named, never omitted and never an error.** A
//!   `file:` reference whose bytes are gone (a cleaned `runs/`, a pruned
//!   mission) becomes a manifest entry marked `unresolved` carrying the
//!   original reference — the same total-classifier discipline as
//!   [`crate::gate_results::resolve_artefact`].
//! - **No host paths.** References stay mission-relative; the absolute path
//!   the resolver probed never crosses into the bundle (the same reason the
//!   provenance chain records only the classification — a host path would
//!   leak the machine layout into the audit record). Bundle-relative paths
//!   are always `/`-joined so the package is host-platform neutral.
//! - **Read-only against the mission dir; the write target is outside it.**
//!   No lock (§4.3 read-only observers); [`export_evidence_bundle`] refuses
//!   an `--out` inside the mission dir before writing anything.
//!
//! WHY the raw log ships beside the folds: the chain, escalations, and cost
//! are all pure folds of `events.jsonl`; an auditor with no repo access can
//! only RE-CHECK that claim if the primary record is in the package. The log
//! is the one entry that is never unresolved — a mission without its log is
//! not a mission (the CLI's `require_mission` rule), so a missing/unreadable
//! log fails the export outright.

use crate::error::EngineError;
use crate::gate_results::{file_artefact_ref, resolve_artefact, ArtefactResolution};
use crate::outcomes::MissionOutcomes;
use crate::paths::MissionPaths;
use crate::provenance::{ArtefactStatus, ProvenanceChain};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

/// `manifest.json`'s `version` field: the bundle format version. Bump on any
/// layout/schema change so a reader can tell what it is holding.
pub const BUNDLE_FORMAT_VERSION: u32 = 1;

pub const MANIFEST_FILE: &str = "manifest.json";
pub const SUMMARY_FILE: &str = "summary.md";
pub const CHAIN_FILE: &str = "chain.json";
pub const ESCALATIONS_FILE: &str = "escalations.json";
pub const COST_FILE: &str = "cost.json";
pub const LOG_FILE: &str = "events.jsonl";
pub const ARTEFACTS_DIR: &str = "artefacts";

/// The well-known mission documents shipped as artefacts — the mission's
/// recorded inputs (plan, machine plan, research evidence, approval-time
/// estimate) and its completion report — in fixed bundle order. Absent ones
/// (a pre-approval mission, an in-flight mission with no report yet) appear
/// as unresolved entries exactly like any other missing evidence: named,
/// never silently omitted.
const MISSION_DOCUMENTS: [&str; 5] = [
    "plan.md",
    "plan.json",
    "research.md",
    "estimate.json",
    "report.md",
];

/// What one manifest entry is. Serde lowercase (the `ArtefactStatus` idiom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// `summary.md` — the human surface, folded from the log.
    Summary,
    /// `chain.json` — the provenance chain, folded from the log.
    Chain,
    /// `escalations.json` — the escalation-ledger rows, folded from the log.
    Escalations,
    /// `cost.json` — the cost fold.
    Cost,
    /// `events.jsonl` — the raw scrubbed log bytes (the primary record).
    Log,
    /// Bytes (or an unresolved placeholder) for one `file:` artefact
    /// reference or well-known mission document.
    Artefact,
}

/// One row of the machine index. `path`/`sha256` are absent exactly when the
/// entry is an unresolved artefact — there are no bytes to point at, and a
/// fabricated path would be a lie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    /// Bundle-relative path (`/`-joined) of the entry's bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Full lowercase-hex SHA-256 of the bytes at `path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Where the entry came from: the artefact reference verbatim
    /// (`file:runs/r-1.jsonl`) for artefacts, or the fold that produced a
    /// generated file (`derived:provenance-chain`, …).
    pub source: String,
    pub kind: EntryKind,
    /// The resolver's classification — artefact entries only. Generated
    /// files and the log are present by construction, so they carry no
    /// classification field at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ArtefactStatus>,
}

/// The machine index (`manifest.json`): every bundle entry with its sha256
/// and source reference, in bundle order — generated files first (fixed
/// order), then artefacts in first-appearance order across the chain (gates,
/// then sessions, then the well-known documents), each unique reference
/// appearing exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceManifest {
    pub version: u32,
    pub mission_id: String,
    pub entries: Vec<ManifestEntry>,
}

/// One bundle payload: a `/`-joined bundle-relative path and its bytes.
/// Logical paths (never host paths), so the in-memory form is already
/// platform-neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// The assembled bundle: the manifest plus every NON-manifest file's bytes,
/// in write order. `manifest.json` itself is serialized at write time (it
/// cannot list its own hash). Held in memory so two assemblies can be
/// compared for byte identity before anything touches disk.
#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceBundle {
    pub manifest: EvidenceManifest,
    pub files: Vec<BundleFile>,
}

/// The mission's cost fold, bundled (`cost.json`). All fields come from
/// [`crate::outcomes::mission_outcomes`] — the same fold the flight-surgeon
/// surfaces use, so the bundle can never disagree with them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissionCostSummary {
    /// Σ worker cost (recorded costUsd, token-priced fallback).
    pub total_cost_usd: f64,
    /// Commits on `feature.completed` whose subject is not an engine/meta
    /// template.
    pub non_meta_commits: u64,
    /// total_cost_usd / non_meta_commits — None when there are no non-meta
    /// commits (the ratio is meaningless, not zero).
    pub usd_per_commit: Option<f64>,
    /// created → terminal minus paused spans; None while the mission is in
    /// flight.
    pub cycle_time_ms: Option<u64>,
    /// Whether a terminal event has been recorded.
    pub closed: bool,
    /// Operator interventions (the outcomes fold's definition).
    pub interventions: u64,
}

/// What [`export_evidence_bundle`] wrote, for the CLI's one-line report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    pub out_dir: PathBuf,
    /// Files written, including `manifest.json`.
    pub files_written: usize,
    pub resolved_artefacts: usize,
    pub unresolved_artefacts: usize,
}

/// Lowercase hex SHA-256 of `bytes` — the manifest's integrity digest. Full
/// 32-byte digest (unlike [`crate::prompts::hash_text`]'s 12-char identity
/// hash): the manifest is an audit surface, so collisions must be
/// cryptographic, not merely unlikely.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Serialize with a trailing newline so generated files are POSIX-clean text.
/// Deterministic: serde_json's struct order is declaration order and its
/// pretty printer has no environment input.
fn to_json_bytes<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(text.into_bytes())
}

/// Map a resolved `file:` reference to its `/`-joined bundle path under
/// `artefacts/`, keeping only `Normal` components (`CurDir` is dropped).
/// Returns None when the reference names nothing — impossible for a
/// RESOLVED reference (the resolver only resolves honest mission-relative
/// paths), so callers treat None as unresolved rather than erroring.
fn artefact_bundle_path(reference: &str) -> Option<String> {
    let relative = reference.strip_prefix(crate::gate_results::FILE_REF_SCHEME)?;
    let mut parts = Vec::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            // `./runs/x` and `runs/x` name the same bytes; the bundle path
            // must be one canonical spelling or the same file could ship
            // twice under two names.
            Component::CurDir => {}
            // Escape shapes never resolve; belt-and-braces, the bundle
            // never builds a path from one.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("{ARTEFACTS_DIR}/{}", parts.join("/")))
}

/// Resolve one `file:` reference against the mission dir and read its bytes
/// no-follow. Total, mirroring [`resolve_artefact`]: a reference that
/// classifies unresolved, or whose read fails between classification and
/// open (a racing prune), yields `(Unresolved, None)` — the bundle names
/// the gap instead of failing.
///
/// The bytes cross [`crate::scrub`] on the way in (audit H5). `events.jsonl`
/// was redacted at append time; artefacts are ordinary files in a tree a
/// worker can write, so the bundle applies the boundary itself rather than
/// inheriting a guarantee only the engine's own writers keep. Without this,
/// planting a secret in a finished transcript put it in the package the
/// module contract promises is scrubbed.
///
/// Artefacts are handled as TEXT: bytes are decoded lossily
/// (`from_utf8_lossy`), scrubbed, and the scrubbed text is what ships and
/// what the manifest digests. A binary artefact therefore travels with
/// U+FFFD in place of its invalid bytes. That is deliberate: the alternative
/// — passing non-UTF-8 through verbatim — makes one stray byte an opt-out of
/// redaction, and every artefact the engine produces (JSONL transcripts,
/// plan/report markdown, JSON) is text.
fn read_artefact(mission_dir: &Path, reference: &str) -> (ArtefactStatus, Option<Vec<u8>>) {
    let ArtefactResolution::Resolved { path } = resolve_artefact(mission_dir, reference) else {
        return (ArtefactStatus::Unresolved, None);
    };
    let read = crate::paths::open_read_nofollow(&path).and_then(|mut file| {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    match read {
        Ok(bytes) => {
            let scrubbed = crate::scrub::scrub(&String::from_utf8_lossy(&bytes));
            (ArtefactStatus::Resolved, Some(scrubbed.into_bytes()))
        }
        Err(_) => (ArtefactStatus::Unresolved, None),
    }
}

/// The serde wire name of a fieldless enum value ("approval", "pass",
/// "validator-scrutiny", …). Deriving from serde — rather than hand-writing
/// a parallel spelling — means the summary can never drift from the
/// spellings the log itself records.
fn wire_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .expect("gate/role enums always serialize to a string")
}

/// Collapse all whitespace runs to single spaces (goal text, decision
/// summaries) so one logical line of the summary stays one physical line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escape a markdown table cell: pipes would break the column structure,
/// newlines the row structure.
fn md_cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

/// Milliseconds as a compact duration ("12s", "47m", "2.3h", "3.1d") — the
/// CLI's `format_duration_ms` shape, kept local so the engine surface stays
/// renderer-free.
fn format_duration_ms(ms: u64) -> String {
    const S: u64 = 1_000;
    const M: u64 = 60 * S;
    const H: u64 = 60 * M;
    const D: u64 = 24 * H;
    if ms >= D {
        format!("{:.1}d", ms as f64 / D as f64)
    } else if ms >= H {
        format!("{:.1}h", ms as f64 / H as f64)
    } else if ms >= M {
        format!("{}m", ms / M)
    } else {
        format!("{}s", ms / S)
    }
}

/// Render `summary.md` from the chain, the cost fold, the escalation rows,
/// and the artefact manifest entries. Pure: no clock, no host paths — the
/// same inputs always render the same bytes.
fn render_summary(
    chain: &ProvenanceChain,
    cost: &MissionCostSummary,
    escalations: &[crate::outcomes::EscalationRow],
    artefact_entries: &[ManifestEntry],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Evidence bundle — mission {}\n\n",
        chain.mission_id
    ));
    out.push_str(
        "Portable audit package (KRZ-326). Everything below derives from the mission's\n\
         append-only event log (`events.jsonl`, included verbatim — every line crossed\n\
         the redact-at-write boundary when appended) plus the mission-relative artefact\n\
         bytes under `artefacts/`. Artefacts ship as scrubbed text: they are redacted\n\
         at export (not at write), and any byte that is not valid UTF-8 travels as the\n\
         replacement character. References whose bytes were no longer on disk at\n\
         export time are listed as `unresolved` in `manifest.json` — named, never\n\
         silently omitted.\n\n",
    );

    out.push_str("## Mission\n\n");
    match &chain.goal {
        Some(goal) => out.push_str(&format!("- Goal: {}\n", one_line(goal))),
        None => out.push_str("- Goal: (not recorded in the log)\n"),
    }
    if let (Some(mission_branch), Some(base_branch)) = (&chain.mission_branch, &chain.base_branch) {
        let pinned = chain
            .base_sha
            .as_deref()
            .map(|sha| format!(" @ {sha}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- Branch: {mission_branch} (base {base_branch}{pinned})\n"
        ));
    }
    match &chain.outcome {
        Some(terminal) => {
            let reason = terminal
                .reason
                .as_deref()
                .map(|reason| format!(" — {}", one_line(reason)))
                .unwrap_or_default();
            out.push_str(&format!(
                "- Outcome: {} at seq {}{}\n",
                terminal.status.as_str(),
                terminal.seq,
                reason
            ));
        }
        None => out.push_str("- Outcome: in flight (no terminal event recorded)\n"),
    }
    let usd_per_commit = cost
        .usd_per_commit
        .map(|usd| format!("${usd:.4}/commit"))
        .unwrap_or_else(|| "n/a (no non-meta commits)".to_string());
    out.push_str(&format!(
        "- Cost: ${:.4} across {} non-meta commits ({})\n",
        cost.total_cost_usd, cost.non_meta_commits, usd_per_commit
    ));
    let cycle = cost
        .cycle_time_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "n/a (in flight)".to_string());
    out.push_str(&format!(
        "- Cycle time: {cycle} | Interventions: {} | Closed: {}\n\n",
        cost.interventions,
        if cost.closed { "yes" } else { "no" }
    ));

    out.push_str("## Gate ladder (log order)\n\n");
    if chain.gates.is_empty() {
        out.push_str("(no gate.result events recorded)\n\n");
    } else {
        out.push_str(
            "| seq | surface | kind | # | gate | verdict | score | artefact | resolution |\n\
             |----:|---------|------|--:|------|---------|-------|----------|------------|\n",
        );
        for gate in &chain.gates {
            let score = match (gate.score, gate.threshold) {
                (Some(score), Some(threshold)) => format!("{score}/{threshold}"),
                _ => "—".to_string(),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | `{}` | {} |\n",
                gate.seq,
                wire_name(&gate.surface),
                wire_name(&gate.kind),
                gate.index,
                md_cell(&gate.gate),
                wire_name(&gate.verdict),
                score,
                md_cell(&gate.artefact_ref),
                gate.artefact.as_str(),
            ));
        }
        out.push('\n');
    }

    if !chain.gate_evaluations.is_empty() {
        out.push_str("## External gate decisions\n\n");
        for record in &chain.gate_evaluations {
            let request = &record.requested.request.params;
            let status = match &record.resolution {
                Some(resolution) => format!("{:?}", resolution.disposition),
                None if record.finished.is_some() => "awaiting engine resolution".into(),
                None => "interrupted or pending evaluation".into(),
            };
            out.push_str(&format!(
                "- `{}` / {:?} / `{}`: {}; consumed={} (effect completion is separate).\n",
                request.gate_id.as_str(),
                request.stage,
                request.attempt_id.as_str(),
                status,
                record.consumed.is_some()
            ));
        }
        out.push('\n');
    }

    // Flight Rules coverage (KRZ-343, design D-H): the rule coverage matrix
    // rides the chain, so the bundle renders the SAME fold the replay
    // computed — no second derivation to drift. It follows the gate ladder
    // it joins against. `None` (no approved standards pin — every
    // pre-Flight-Rules mission) renders nothing, so those summaries stay
    // byte-identical.
    if let Some(coverage) = &chain.standards {
        out.push_str(&crate::standards_coverage::render_coverage_markdown(
            coverage,
        ));
        out.push('\n');
    }

    out.push_str("## Sessions (workers and reviewers)\n\n");
    if chain.sessions.is_empty() {
        out.push_str("(no sessions recorded)\n\n");
    } else {
        out.push_str(
            "| seq | run | role | backend | model | prompt hash | transcript | resolution |\n\
             |----:|-----|------|---------|-------|-------------|------------|------------|\n",
        );
        for session in &chain.sessions {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | `{}` | `{}` | {} |\n",
                session.seq,
                md_cell(&session.run_id),
                wire_name(&session.role),
                session.backend.as_deref().unwrap_or("?"),
                md_cell(&session.model),
                session.prompt_hash,
                md_cell(&session.transcript_ref),
                session.transcript.as_str(),
            ));
        }
        out.push('\n');
    }

    out.push_str("## Human decisions\n\n");
    if chain.decisions.is_empty() {
        out.push_str("(no human decisions recorded)\n\n");
    } else {
        for decision in &chain.decisions {
            out.push_str(&format!(
                "- [seq {}] {} — {}\n",
                decision.seq,
                decision.kind.as_str(),
                one_line(&decision.summary)
            ));
        }
        out.push('\n');
    }

    out.push_str("## Escalations\n\n");
    if escalations.is_empty() {
        out.push_str("(no escalations recorded)\n\n");
    } else {
        for row in escalations {
            let latency = row
                .latency_ms
                .map(|ms| format!(" (latency {ms} ms)"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- [{}] {}: {} → {}{}\n",
                row.ts.to_rfc3339(),
                row.kind.as_str(),
                one_line(&row.summary),
                one_line(&row.decision),
                latency,
            ));
        }
        out.push('\n');
    }

    out.push_str("## Artefacts\n\n");
    out.push_str(
        "| bundle path | source | sha256 | status |\n\
         |-------------|--------|--------|--------|\n",
    );
    for entry in artefact_entries {
        let status = entry.status.map(|status| status.as_str()).unwrap_or("—");
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            entry
                .path
                .as_deref()
                .map(|path| format!("`{path}`"))
                .unwrap_or_else(|| "—".to_string()),
            md_cell(&entry.source),
            entry.sha256.as_deref().unwrap_or("—"),
            status,
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "Regenerate with `kranz evidence-bundle {}`; the same event log always yields\n\
         the same bundle bytes.\n",
        chain.mission_id
    ));
    out
}

/// Assemble one mission's evidence bundle in memory. Read-only against the
/// mission dir (no lock — §4.3 read-only observers), no clock, no network,
/// no git: the same log and artefact bytes always assemble the same bundle.
///
/// Fallible where honesty demands it: a mission whose log is missing or
/// corrupt fails (the log is the primary record — there is no bundle without
/// it), and a `config.changed` patch the reducer would reject fails the
/// provenance fold exactly as it fails the replay. Artefact gaps NEVER fail:
/// they are manifest entries.
pub fn assemble_evidence_bundle(
    repo_root: &Path,
    mission_id: &str,
) -> anyhow::Result<EvidenceBundle> {
    let paths = MissionPaths::new(repo_root, mission_id);
    paths.require_no_follow()?;
    let mission_dir = paths.mission_dir();

    // The primary record, read ONCE: the same buffer is parsed+validated
    // for the folds AND shipped verbatim as the bundle's log copy
    // (12th-pass review). Two separate opens — parse here, reread raw bytes
    // there — would let a concurrent append (or a torn final line the
    // parser dropped) desync the shipped `events.jsonl` from the
    // chain/cost/escalations folded from it; the auditor's re-fold of the
    // shipped bytes must reproduce the bundle exactly. The torn-tail rule
    // (`read_events_and_log_bytes`): a torn final line is excluded from
    // BOTH the events and the shipped bytes — bytes-shipped == bytes-parsed.
    let (events, log_bytes) =
        crate::event_log::EventLog::read_events_and_log_bytes(&paths.events_file())?;

    let chain = crate::provenance::provenance_chain(&mission_dir, mission_id, &events)?;
    let outcomes: MissionOutcomes = crate::outcomes::mission_outcomes(mission_id, &events);
    let cost = MissionCostSummary {
        total_cost_usd: outcomes.cost_usd,
        non_meta_commits: outcomes.non_meta_commits,
        usd_per_commit: (outcomes.non_meta_commits > 0)
            .then(|| outcomes.cost_usd / outcomes.non_meta_commits as f64),
        cycle_time_ms: outcomes.cycle_time_ms,
        closed: outcomes.is_closed,
        interventions: outcomes.interventions,
    };

    // Artefact references in first-appearance order — gates (log order),
    // sessions, then the well-known documents — deduplicated by the verbatim
    // reference string. First-appearance is a pure function of the log, so
    // bundle ordering is deterministic without consulting anything else.
    // Inline references (no `file:` scheme) ship NO manifest entry: their
    // evidence is textual and already travels verbatim in the chain.
    let mut references: Vec<String> = Vec::new();
    let mut push_reference = |reference: String| {
        if reference.starts_with(crate::gate_results::FILE_REF_SCHEME)
            && !references.contains(&reference)
        {
            references.push(reference);
        }
    };
    for gate in &chain.gates {
        push_reference(gate.artefact_ref.clone());
    }
    let mut gate_expected = std::collections::BTreeMap::new();
    for record in &chain.gate_evaluations {
        for artifact in record.requested.retained_inputs.iter().chain(
            record
                .finished
                .iter()
                .flat_map(|finished| &finished.artifacts),
        ) {
            let reference = file_artefact_ref(artifact.path.as_str());
            let expected = (artifact.retained_digest.clone(), artifact.retained_bytes);
            gate_expected
                .entry(reference.clone())
                .and_modify(|prior: &mut Option<_>| {
                    if prior.as_ref() != Some(&expected) {
                        *prior = None;
                    }
                })
                .or_insert(Some(expected));
            push_reference(reference);
        }
    }
    for session in &chain.sessions {
        push_reference(file_artefact_ref(&session.transcript_ref));
    }
    for document in MISSION_DOCUMENTS {
        push_reference(file_artefact_ref(document));
    }

    let mut artefact_entries: Vec<ManifestEntry> = Vec::new();
    let mut artefact_files: Vec<BundleFile> = Vec::new();
    for reference in &references {
        let (mut status, mut bytes) = read_artefact(&mission_dir, reference);
        if let Some(expected) = gate_expected.get(reference) {
            let matches =
                expected
                    .as_ref()
                    .zip(bytes.as_ref())
                    .is_some_and(|((digest, length), bytes)| {
                        *length == bytes.len() as u64
                            && *digest == crate::gate_evaluation::protocol::Digest::of(bytes)
                    });
            if !matches {
                status = ArtefactStatus::Unresolved;
                bytes = None;
            }
        }
        match artefact_bundle_path(reference).zip(bytes) {
            Some((path, bytes)) => {
                artefact_entries.push(ManifestEntry {
                    path: Some(path.clone()),
                    sha256: Some(sha256_hex(&bytes)),
                    source: reference.clone(),
                    kind: EntryKind::Artefact,
                    status: Some(status),
                });
                artefact_files.push(BundleFile { path, bytes });
            }
            None => artefact_entries.push(ManifestEntry {
                path: None,
                sha256: None,
                source: reference.clone(),
                kind: EntryKind::Artefact,
                status: Some(ArtefactStatus::Unresolved),
            }),
        }
    }

    // The human summary reads the artefact entries, so it is rendered after
    // them — but it still SORTS first in the bundle (fixed generated order).
    let summary = render_summary(&chain, &cost, &outcomes.escalations, &artefact_entries);

    let mut files: Vec<BundleFile> = Vec::new();
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut push_generated = |path: &str, source: &str, kind: EntryKind, bytes: Vec<u8>| {
        entries.push(ManifestEntry {
            path: Some(path.to_string()),
            sha256: Some(sha256_hex(&bytes)),
            source: source.to_string(),
            kind,
            status: None,
        });
        files.push(BundleFile {
            path: path.to_string(),
            bytes,
        });
    };
    push_generated(
        SUMMARY_FILE,
        "derived:human-summary",
        EntryKind::Summary,
        summary.into_bytes(),
    );
    push_generated(
        CHAIN_FILE,
        "derived:provenance-chain",
        EntryKind::Chain,
        to_json_bytes(&chain)?,
    );
    push_generated(
        ESCALATIONS_FILE,
        "derived:escalations-fold",
        EntryKind::Escalations,
        to_json_bytes(&outcomes.escalations)?,
    );
    push_generated(
        COST_FILE,
        "derived:cost-fold",
        EntryKind::Cost,
        to_json_bytes(&cost)?,
    );
    push_generated(LOG_FILE, "file:events.jsonl", EntryKind::Log, log_bytes);
    files.extend(artefact_files);
    entries.extend(artefact_entries);

    Ok(EvidenceBundle {
        manifest: EvidenceManifest {
            version: BUNDLE_FORMAT_VERSION,
            mission_id: mission_id.to_string(),
            entries,
        },
        files,
    })
}

/// Absolutize `path` and fold `.`/`..` LEXICALLY, without touching the
/// filesystem: `std::path::absolute` PRESERVES `..` on this host, so the
/// fold is what makes an `outside/../.kranz/...` shape comparable with
/// `starts_with`. A `..` above the root is inert (`/..` == `/`). Lexical
/// folding is sound for the containment check only because the write path
/// below verifies no component it traverses is a symlink — a folded `a/..`
/// equals `a` only when `a` cannot redirect.
fn absolute_lexical(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.file_name().is_some() {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

/// The planned bundle output directory: the canonical anchor to open and
/// the missing components to create beneath it.
struct OutDirPlan {
    /// Canonical path of the deepest EXISTING ancestor (the trusted anchor —
    /// canonicalization resolves system symlinks such as macOS `/var`, the
    /// same trust basis [`crate::paths::open_parent_nofollow`]'s weaker tier
    /// uses for out-of-model paths).
    anchor: PathBuf,
    /// Missing components below the anchor, created no-follow at pin time.
    tail: Vec<String>,
    /// The canonical path the pinned out dir will have (`anchor` + `tail` —
    /// canonical by construction: the anchor is canonical and the tail is
    /// created as real directories under it).
    canonical_out: PathBuf,
}

/// Plan the out dir WITHOUT creating anything: absolutize + lexically fold,
/// walk up to the deepest existing ancestor (a SYMLINKED or non-directory
/// ancestor is a refusal — `symlink_metadata` inspects the component
/// itself, never its target), canonicalize the anchor, and compute the
/// canonical out path. The containment check runs on this plan before any
/// directory is created, so a refusal writes nothing (12th-pass review).
fn plan_out_dir(out_dir: &Path) -> anyhow::Result<OutDirPlan> {
    let normalized = absolute_lexical(out_dir)?;
    let mut anchor = normalized.as_path();
    loop {
        match std::fs::symlink_metadata(anchor) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                if file_type.is_symlink() {
                    return Err(EngineError::InvalidState(format!(
                        "bundle output {} resolves through a symlinked component: {}",
                        out_dir.display(),
                        anchor.display()
                    ))
                    .into());
                }
                if !file_type.is_dir() {
                    return Err(EngineError::InvalidState(format!(
                        "bundle output {} is blocked by a non-directory component: {}",
                        out_dir.display(),
                        anchor.display()
                    ))
                    .into());
                }
                break;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                anchor = anchor.parent().ok_or_else(|| {
                    EngineError::InvalidState(format!(
                        "bundle output {} has no existing ancestor",
                        out_dir.display()
                    ))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let canonical_anchor = anchor.canonicalize()?;
    let mut tail = Vec::new();
    let mut canonical_out = canonical_anchor.clone();
    // The anchor is a lexical prefix of `normalized` by construction; every
    // component below it is `Normal` (the fold left nothing else).
    for component in normalized
        .strip_prefix(anchor)
        .map_err(|_| {
            EngineError::InvalidState(format!(
                "bundle output {} escaped its anchor",
                out_dir.display()
            ))
        })?
        .components()
    {
        let Component::Normal(name) = component else {
            return Err(EngineError::InvalidState(format!(
                "bundle output {} has a non-normal component below its anchor",
                out_dir.display()
            ))
            .into());
        };
        let name = name.to_str().ok_or_else(|| {
            EngineError::InvalidState(format!(
                "bundle output {} has a non-UTF-8 component",
                out_dir.display()
            ))
        })?;
        tail.push(name.to_string());
        canonical_out.push(name);
    }
    Ok(OutDirPlan {
        anchor: canonical_anchor,
        tail,
        canonical_out,
    })
}

/// Pin the planned out dir as a RETAINED capability: open the canonical
/// anchor ambient, then create and open every missing tail component
/// per-component no-follow ([`crate::paths::open_real_subdir`] — a component
/// planted as a symlink mid-walk is refused, never followed). Every later
/// write goes through the returned capability, never back through the
/// display path that was checked — closing the check-then-write window.
fn pin_out_dir(plan: &OutDirPlan) -> anyhow::Result<Dir> {
    let mut dir = Dir::open_ambient_dir(&plan.anchor, ambient_authority())?;
    let mut walked = plan.anchor.clone();
    for component in &plan.tail {
        walked.push(component);
        dir = crate::paths::open_real_subdir(&dir, component, &walked, true)?;
    }
    Ok(dir)
}

/// Write the bundle through the pinned no-follow capability: the emptiness
/// check, per-entry parent creation, and every file write go through `out`
/// (never back through the display path), so nothing crosses a symlink
/// between check and write. Bundle paths are re-validated on the way out
/// (relative, non-empty `Normal` components only) so a hostile or buggy
/// assembly cannot write outside the out dir, and each file is
/// `create_new` + `FollowSymlinks::No` — the out dir was empty, so a
/// pre-existing name (a planted symlink most of all) fails instead of
/// being written through.
fn write_bundle_files(bundle: &EvidenceBundle, out_dir: &Path, out: &Dir) -> anyhow::Result<usize> {
    let mut entries = out.entries().map_err(|error| {
        EngineError::InvalidState(format!(
            "bundle output {} is not an empty directory: {error}",
            out_dir.display()
        ))
    })?;
    if entries.next().is_some() {
        return Err(EngineError::InvalidState(format!(
            "bundle output {} is not empty; choose a fresh --out or remove it",
            out_dir.display()
        ))
        .into());
    }

    let manifest_bytes = to_json_bytes(&bundle.manifest)?;
    let mut written = 0usize;
    // The manifest writes last: it indexes the other entries, and a partial
    // write then leaves a tree whose index is absent rather than wrong.
    for (relative, bytes) in bundle
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.bytes.as_slice()))
        .chain([(MANIFEST_FILE, manifest_bytes.as_slice())])
    {
        let mut names = Vec::new();
        for component in relative.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(
                    EngineError::InvalidState(format!("unsafe bundle path {relative:?}")).into(),
                );
            }
            names.push(component);
        }
        let (leaf, parents) = names.split_last().expect("validated non-empty");
        let mut dir = None;
        let mut display = out_dir.to_path_buf();
        for parent in parents {
            display.push(parent);
            dir = Some(crate::paths::open_real_subdir(
                dir.as_ref().unwrap_or(out),
                parent,
                &display,
                true,
            )?);
        }
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let mut file = dir.as_ref().unwrap_or(out).open_with(leaf, &options)?;
        file.write_all(bytes)?;
        written += 1;
    }
    Ok(written)
}

/// Write an assembled bundle to `out_dir`, returning the number of files
/// written (including `manifest.json`). The directory must not already hold
/// anything: silently mixing two exports would leave stale artefacts no
/// manifest entry names — the same honesty discipline as unresolved entries.
/// The out dir is created and written through a pinned no-follow capability
/// ([`plan_out_dir`] / [`pin_out_dir`]): a symlinked existing component is
/// refused, and nothing written ever crosses a symlink.
pub fn write_evidence_bundle(bundle: &EvidenceBundle, out_dir: &Path) -> anyhow::Result<usize> {
    let plan = plan_out_dir(out_dir)?;
    let out = pin_out_dir(&plan)?;
    write_bundle_files(bundle, out_dir, &out)
}

/// Assemble + write the bundle, with the one placement rule enforced: the
/// write target must be OUTSIDE the mission dir (a bundle written into the
/// tree it audits would both mutate the read-only surface and risk shipping
/// itself as evidence).
///
/// The rule is enforced in two tiers, both BEFORE anything is written
/// (12th-pass review): a lexical tier (absolutize + fold `..`, then
/// `starts_with`) that catches the direct and `..`-shaped in-mission paths
/// without touching the filesystem, and a canonical tier — `absolute`
/// preserves `..` on this host and a symlinked component makes a lexical
/// `starts_with` lie — that canonicalizes the out dir's deepest existing
/// ancestor and compares the canonical out path against the canonical
/// mission dir. The write itself then goes through the pinned no-follow
/// capability from [`plan_out_dir`] / [`pin_out_dir`].
pub fn export_evidence_bundle(
    repo_root: &Path,
    mission_id: &str,
    out_dir: &Path,
) -> anyhow::Result<ExportOutcome> {
    let paths = MissionPaths::new(repo_root, mission_id);
    paths.require_no_follow()?;
    let refusal = || {
        EngineError::InvalidState(format!(
            "bundle output {} must be outside the mission dir {}",
            out_dir.display(),
            paths.mission_dir().display()
        ))
    };
    // Lexical tier: refuses the direct and `..`-shaped placements before
    // any filesystem write (a refusal leaves nothing behind).
    let out_lexical = absolute_lexical(out_dir)?;
    let mission_lexical = absolute_lexical(&paths.mission_dir())?;
    if out_lexical.starts_with(&mission_lexical) {
        return Err(refusal().into());
    }
    // Canonical tier: the lexical fold cannot see symlinks, so compare the
    // canonical out path against the canonical mission dir. A symlinked
    // existing component of the out path is refused by the plan itself.
    // (A not-yet-existing mission dir skips this tier — there is no audited
    // tree to contaminate, and the assembly below fails the unknown mission
    // honestly.)
    let plan = plan_out_dir(out_dir)?;
    match std::fs::symlink_metadata(paths.mission_dir()) {
        Ok(_) => {
            if plan
                .canonical_out
                .starts_with(paths.mission_dir().canonicalize()?)
            {
                return Err(refusal().into());
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let bundle = assemble_evidence_bundle(repo_root, mission_id)?;
    let out = pin_out_dir(&plan)?;
    let files_written = write_bundle_files(&bundle, out_dir, &out)?;
    let resolved_artefacts = bundle
        .manifest
        .entries
        .iter()
        .filter(|entry| entry.status == Some(ArtefactStatus::Resolved))
        .count();
    let unresolved_artefacts = bundle
        .manifest
        .entries
        .iter()
        .filter(|entry| entry.status == Some(ArtefactStatus::Unresolved))
        .count();
    Ok(ExportOutcome {
        out_dir: out_dir.to_path_buf(),
        files_written,
        resolved_artefacts,
        unresolved_artefacts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{EventLog, LockForce};
    use crate::events::EventKind;
    use crate::gate::{GateKind, GateSurface, GateVerdict};
    use crate::types::{GrantKind, MissionConfig, Plan, Role, RunResult, TokenUsage};
    use std::collections::BTreeMap;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Seed a mission's `events.jsonl` with the given kinds, in order (the
    /// provenance fixture idiom); the log handle drops — and flushes — before
    /// any assembly reads.
    fn seed_mission(repo_root: &Path, id: &str, kinds: Vec<EventKind>) -> MissionPaths {
        let paths = MissionPaths::new(repo_root, id);
        let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
        for kind in kinds {
            log.append(kind).unwrap();
        }
        paths
    }

    fn sample_plan() -> Plan {
        Plan {
            goal: "ship the thing".into(),
            validation_contract: vec![],
            milestones: vec![],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
            reviewer_independence: None,
        }
    }

    fn created() -> EventKind {
        EventKind::MissionCreated {
            goal: "ship the thing".into(),
            base_branch: "main".into(),
            mission_branch: "kranz/mission-x".into(),
            config: MissionConfig::default(),
        }
    }

    fn gate_result(
        gate: &str,
        surface: GateSurface,
        kind: GateKind,
        index: u32,
        artefact_ref: &str,
    ) -> EventKind {
        EventKind::GateResult {
            gate: gate.to_string(),
            surface,
            kind,
            index,
            verdict: GateVerdict::Pass,
            artefact_ref: artefact_ref.to_string(),
            artefact_detail: None,
            score: None,
            threshold: None,
            rule_ids: Vec::new(),
        }
    }

    fn worker_spawned(run_id: &str, role: Role, model: &str) -> EventKind {
        EventKind::WorkerSpawned {
            backend: None,
            run_id: run_id.to_string(),
            role,
            feature_id: None,
            milestone_id: None,
            candidate: None,
            executor_route: None,
            sdk_session_id: format!("sess-{run_id}"),
            model: model.to_string(),
            quant: "n/a".to_string(),
            weight_hash: None,
            prompt_hash: "aaaabbbbcccc".to_string(),
            transcript_path: MissionPaths::transcript_rel(run_id),
        }
    }

    /// The full fixture: both gate surfaces; an inline ref, a resolved file
    /// ref, a file ref whose bytes were never written, and a DUPLICATE file
    /// ref (the manifest-dedup pin); a completed worker run with cost and a
    /// non-meta commit; a grant park + approval; a blocked→unblocked pair; a
    /// steer — ending COMPLETED. Documents: plan.md/plan.json/report.md are
    /// written, research.md/estimate.json deliberately absent (the unresolved
    /// arm for well-known documents).
    fn seed_full_mission(root: &Path) -> MissionPaths {
        let paths = seed_mission(
            root,
            "m-1",
            vec![
                created(),
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: Some("deadbeef".to_string()),
                },
                gate_result(
                    "vacuous-filter",
                    GateSurface::Approval,
                    GateKind::Deterministic,
                    0,
                    "contract gate vacuous-filter",
                ),
                gate_result(
                    "merge-gate-suite",
                    GateSurface::Approval,
                    GateKind::Deterministic,
                    1,
                    "file:runs/gate-base.jsonl",
                ),
                gate_result(
                    "merge-gate-suite-recheck",
                    GateSurface::Approval,
                    GateKind::Deterministic,
                    2,
                    // The same reference as the previous gate: the manifest
                    // must list it exactly once.
                    "file:runs/gate-base.jsonl",
                ),
                gate_result(
                    "plan-review",
                    GateSurface::Approval,
                    GateKind::ModelJudged,
                    0,
                    "file:runs/gone.jsonl",
                ),
                worker_spawned("r-1", Role::Worker, "gpt-5"),
                EventKind::WorkerCompleted {
                    run_id: "r-1".into(),
                    result: RunResult::Pass,
                    tokens: TokenUsage {
                        input: 100,
                        output: 50,
                        cache_read: 0,
                        cache_write: 0,
                    },
                    cost_usd: Some(0.42),
                    report: None,
                },
                EventKind::FeatureCompleted {
                    feature_id: "f-1-1".into(),
                    commits: vec!["abc1234 implement the widget".into()],
                },
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                worker_spawned("r-2", Role::Worker, "my-local-model"),
                worker_spawned("r-3", Role::ValidatorScrutiny, "sonnet"),
                EventKind::MilestoneBlocked {
                    block_context: None,
                    milestone_id: "ms-1".into(),
                    reason: "fix-cycle cap".into(),
                },
                EventKind::MilestoneUnblocked {
                    block_context: None,
                    milestone_id: "ms-1".into(),
                    reason: "user skipped findings".into(),
                    validator_guidance: None,
                },
                EventKind::UserMessage {
                    text: "skip the flaky test".into(),
                    interrupt: false,
                },
                gate_result(
                    "merge-gate-suite",
                    GateSurface::FinalGate,
                    GateKind::Deterministic,
                    0,
                    ".kranz/merge-gates.json",
                ),
                EventKind::MissionCompleted {},
            ],
        );
        // Bytes for the resolvable refs and the shipped documents.
        std::fs::write(paths.runs_dir().join("gate-base.jsonl"), b"{}").unwrap();
        std::fs::write(paths.runs_dir().join("r-1.jsonl"), b"{}").unwrap();
        std::fs::write(paths.plan_md_file(), b"# plan\n").unwrap();
        std::fs::write(paths.plan_file(), b"{}").unwrap();
        std::fs::write(paths.report_file(), b"# report\n").unwrap();
        paths
    }

    /// Recursively collect a written bundle tree as (relative `/`-joined
    /// path → bytes), sorted — the entry-identity comparison the directory
    /// container's determinism is defined over.
    fn collect_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let relative = path
                        .strip_prefix(dir)
                        .unwrap()
                        .components()
                        .map(|c| c.as_os_str().to_str().unwrap().to_string())
                        .collect::<Vec<_>>()
                        .join("/");
                    out.insert(relative, std::fs::read(&path).unwrap());
                }
            }
        }
        out
    }

    fn manifest_entry<'m>(manifest: &'m EvidenceManifest, source: &str) -> &'m ManifestEntry {
        manifest
            .entries
            .iter()
            .find(|entry| entry.source == source)
            .unwrap_or_else(|| panic!("manifest entry {source} missing"))
    }

    /// Ticket acceptance hint 1: the bundle opens standalone — manifest,
    /// human summary, chain, escalations, cost, the raw log, and the
    /// artefact bytes — with NO reference into the source machine's paths
    /// anywhere in any file. Every resolved manifest entry's sha256 matches
    /// the bytes it names; the missing ref is an unresolved entry; the
    /// duplicated ref appears exactly once.
    #[test]
    fn evidence_bundle_opens_standalone_with_no_host_paths() {
        let tmp = TempDir::new().unwrap();
        seed_full_mission(tmp.path());
        let out = tmp.path().join("bundle-out");
        let outcome = export_evidence_bundle(tmp.path(), "m-1", &out).unwrap();

        for name in [
            MANIFEST_FILE,
            SUMMARY_FILE,
            CHAIN_FILE,
            ESCALATIONS_FILE,
            COST_FILE,
            LOG_FILE,
        ] {
            assert!(out.join(name).is_file(), "{name} missing from the bundle");
        }
        for shipped in [
            "artefacts/runs/gate-base.jsonl",
            "artefacts/runs/r-1.jsonl",
            "artefacts/plan.md",
            "artefacts/plan.json",
            "artefacts/report.md",
        ] {
            assert!(
                out.join(shipped).is_file(),
                "{shipped} missing from artefacts/"
            );
        }
        // 5 generated + manifest + 5 resolved artefacts; 5 unresolved
        // (gone.jsonl, r-2, r-3 transcripts, research.md, estimate.json).
        assert_eq!(outcome.files_written, 11);
        assert_eq!(outcome.resolved_artefacts, 5);
        assert_eq!(outcome.unresolved_artefacts, 5);

        // The host temp path appears in NO bundle file (the test greps the
        // whole tree for it).
        let host = tmp.path().to_string_lossy().to_string();
        let files = collect_files(&out);
        for (relative, bytes) in &files {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains(&host),
                "host path leaked into bundle file {relative}"
            );
        }

        // The manifest round-trips and every resolved entry's sha256 matches
        // the shipped bytes.
        let manifest: EvidenceManifest =
            serde_json::from_str(&std::fs::read_to_string(out.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(manifest.version, BUNDLE_FORMAT_VERSION);
        assert_eq!(manifest.mission_id, "m-1");
        for entry in &manifest.entries {
            if let (Some(path), Some(sha256)) = (&entry.path, &entry.sha256) {
                let bytes = std::fs::read(out.join(path)).unwrap();
                assert_eq!(&sha256_hex(&bytes), sha256, "sha256 mismatch for {path}");
            }
        }
        // The duplicated gate ref produced exactly ONE artefact entry.
        assert_eq!(
            manifest
                .entries
                .iter()
                .filter(|entry| entry.source == "file:runs/gate-base.jsonl")
                .count(),
            1
        );
        // Inline refs carry no manifest entry (their evidence is in the chain).
        assert!(manifest
            .entries
            .iter()
            .all(|entry| entry.source != "contract gate vacuous-filter"));
        // The never-written ref is an unresolved entry with the original
        // reference and no path/sha — named, never omitted.
        let gone = manifest_entry(&manifest, "file:runs/gone.jsonl");
        assert_eq!(gone.status, Some(ArtefactStatus::Unresolved));
        assert!(gone.path.is_none() && gone.sha256.is_none());
        // The chain parses and carries the ladder.
        let chain: ProvenanceChain =
            serde_json::from_str(&std::fs::read_to_string(out.join(CHAIN_FILE)).unwrap()).unwrap();
        assert_eq!(chain.gates.len(), 5);
        // The cost fold crossed: one $0.42 run, one non-meta commit.
        let cost: MissionCostSummary =
            serde_json::from_str(&std::fs::read_to_string(out.join(COST_FILE)).unwrap()).unwrap();
        assert_eq!(cost.total_cost_usd, 0.42);
        assert_eq!(cost.non_meta_commits, 1);
        assert_eq!(cost.usd_per_commit, Some(0.42));
        assert!(cost.closed);
    }

    /// Ticket acceptance hint 2: same log → identical bundle. Two assemblies
    /// are byte-identical in memory, and two written trees are
    /// entry-identical (the directory container's determinism definition).
    #[test]
    fn evidence_bundle_is_byte_identical_across_exports() {
        let tmp = TempDir::new().unwrap();
        seed_full_mission(tmp.path());

        let first = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        let second = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_string_pretty(&first.manifest).unwrap(),
            serde_json::to_string_pretty(&second.manifest).unwrap()
        );

        let out_a = tmp.path().join("out-a");
        let out_b = tmp.path().join("out-b");
        export_evidence_bundle(tmp.path(), "m-1", &out_a).unwrap();
        export_evidence_bundle(tmp.path(), "m-1", &out_b).unwrap();
        assert_eq!(collect_files(&out_a), collect_files(&out_b));
    }

    /// Ticket acceptance hint 3: a log carrying redaction audits yields a
    /// bundle with FINGERPRINTS only — the secret value planted pre-redaction
    /// appears in no bundle file, while the audit fingerprint crosses in the
    /// raw log.
    #[test]
    fn evidence_bundle_redacted_secret_leaves_fingerprints_only() {
        let tmp = TempDir::new().unwrap();
        let secret = "sk-ant-F00barBazQuux9_7";
        let text = format!("the key is {secret} ok");
        // The fingerprint the write boundary will record for this value.
        let findings = crate::scrub::scan_text(&text);
        assert_eq!(findings.len(), 1, "fixture must trip exactly one rule");
        let fingerprint = findings[0].fingerprint.clone();

        seed_mission(
            tmp.path(),
            "m-sec",
            vec![
                created(),
                EventKind::UserMessage {
                    text,
                    interrupt: false,
                },
                EventKind::MissionCompleted {},
            ],
        );

        let out = tmp.path().join("bundle-sec");
        export_evidence_bundle(tmp.path(), "m-sec", &out).unwrap();
        let files = collect_files(&out);
        assert!(!files.is_empty());
        for (relative, bytes) in &files {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains(secret),
                "secret value leaked into bundle file {relative}"
            );
        }
        // The fingerprint crosses in the verbatim log (the secret.redacted
        // audit line), and the redaction marker replaced the value.
        let log = String::from_utf8_lossy(&files[LOG_FILE]).to_string();
        assert!(log.contains(&fingerprint), "audit fingerprint missing");
        assert!(log.contains("[REDACTED]"));
    }

    /// Audit H5: artefact BYTES cross the same redact boundary the log
    /// crossed at append time. A hostile writer who plants a secret straight
    /// into a finished transcript (never through `append_redacting`) must not
    /// get it into the package the operator hands an auditor, and the
    /// manifest sha256 must be the digest of the REDACTED bytes so the
    /// package still verifies against itself.
    #[test]
    fn evidence_bundle_scrubs_artefact_bytes_and_hashes_the_redacted_form() {
        let tmp = TempDir::new().unwrap();
        let secret = "sk-ant-F00barBazQuux9_7";
        let paths = seed_full_mission(tmp.path());
        // Overwrite a finished transcript the way a worker with write access
        // to the mission dir would: raw bytes, no scrub on the way in.
        let planted = format!("{{\"text\":\"the key is {secret} ok\"}}\n");
        std::fs::write(paths.runs_dir().join("r-1.jsonl"), planted.as_bytes()).unwrap();

        let out = tmp.path().join("bundle-artefact-secret");
        export_evidence_bundle(tmp.path(), "m-1", &out).unwrap();
        let files = collect_files(&out);
        for (relative, bytes) in &files {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains(secret),
                "secret value leaked into bundle file {relative}"
            );
        }
        let shipped = &files["artefacts/runs/r-1.jsonl"];
        assert!(String::from_utf8_lossy(shipped).contains("[REDACTED]"));

        // The manifest digest is over the bytes the bundle actually ships.
        let manifest: EvidenceManifest =
            serde_json::from_str(&std::fs::read_to_string(out.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        let entry = manifest_entry(&manifest, "file:runs/r-1.jsonl");
        assert_eq!(entry.sha256.as_deref(), Some(sha256_hex(shipped).as_str()));
    }

    /// A non-UTF-8 artefact still ships, as lossy-decoded scrubbed text: the
    /// bundle has ONE rule for artefact bytes and an invalid byte must not be
    /// a way to opt out of it.
    #[test]
    fn evidence_bundle_scrubs_non_utf8_artefact_bytes_lossily() {
        let tmp = TempDir::new().unwrap();
        let secret = "sk-ant-F00barBazQuux9_7";
        let paths = seed_full_mission(tmp.path());
        let mut planted = format!("the key is {secret} ok").into_bytes();
        planted.push(0xff);
        std::fs::write(paths.runs_dir().join("r-1.jsonl"), &planted).unwrap();

        let bundle = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        let shipped = bundle
            .files
            .iter()
            .find(|file| file.path == "artefacts/runs/r-1.jsonl")
            .expect("artefact shipped");
        let text = String::from_utf8(shipped.bytes.clone()).expect("lossy decode yields UTF-8");
        assert!(!text.contains(secret));
        assert!(text.contains("[REDACTED]"));
        assert!(
            text.contains('\u{fffd}'),
            "invalid byte became a replacement"
        );
    }

    /// Ticket acceptance hint 4: with `runs/` pruned, every file-backed gate
    /// artefact and every transcript degrades to an unresolved manifest entry
    /// — and the export still completes.
    #[test]
    fn evidence_bundle_missing_artefact_bytes_become_unresolved_manifest_entries() {
        let tmp = TempDir::new().unwrap();
        let paths = seed_full_mission(tmp.path());
        std::fs::remove_dir_all(paths.runs_dir()).unwrap();

        let bundle = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        for source in [
            "file:runs/gate-base.jsonl",
            "file:runs/gone.jsonl",
            "file:runs/r-1.jsonl",
            "file:runs/r-2.jsonl",
            "file:runs/r-3.jsonl",
            "file:research.md",
            "file:estimate.json",
        ] {
            let entry = manifest_entry(&bundle.manifest, source);
            assert_eq!(
                entry.status,
                Some(ArtefactStatus::Unresolved),
                "{source} must be unresolved with its bytes gone"
            );
            assert!(entry.path.is_none() && entry.sha256.is_none());
        }
        // The documents outside runs/ still resolve.
        for source in ["file:plan.md", "file:plan.json", "file:report.md"] {
            assert_eq!(
                manifest_entry(&bundle.manifest, source).status,
                Some(ArtefactStatus::Resolved),
                "{source} must still resolve"
            );
        }
        // No artefact bytes shipped under runs/.
        assert!(bundle
            .files
            .iter()
            .all(|file| !file.path.starts_with("artefacts/runs/")));
    }

    /// The placement rule: the write target must be outside the mission dir
    /// (a bundle inside the tree it audits would mutate the read-only
    /// surface). Refused before anything is written.
    #[test]
    fn evidence_bundle_refuses_out_dir_inside_the_mission_dir() {
        let tmp = TempDir::new().unwrap();
        let paths = seed_full_mission(tmp.path());
        let inside = paths.mission_dir().join("bundle");
        let result = export_evidence_bundle(tmp.path(), "m-1", &inside);
        assert!(result.is_err(), "an in-mission --out must be refused");
        assert!(!inside.exists(), "nothing must be written on refusal");
    }

    /// A non-empty output directory is refused: silently mixing two exports
    /// would leave stale files no manifest entry names.
    #[test]
    fn evidence_bundle_refuses_a_non_empty_out_dir() {
        let tmp = TempDir::new().unwrap();
        seed_full_mission(tmp.path());
        let out = tmp.path().join("bundle-used");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("stale.txt"), b"stale").unwrap();
        let result = export_evidence_bundle(tmp.path(), "m-1", &out);
        assert!(result.is_err(), "a non-empty --out must be refused");
        assert_eq!(
            std::fs::read_to_string(out.join("stale.txt")).unwrap(),
            "stale"
        );
    }

    /// 12th-pass review: the bundle's log copy is the SAME buffer the folds
    /// were derived from — the log is read once, never re-opened for the raw
    /// bytes. A torn final line (a crash write the parser drops) is excluded
    /// from BOTH the parsed events and the shipped bytes, so the shipped log
    /// always re-folds to the shipped chain/cost/escalations.
    /// bytes-shipped == bytes-parsed.
    #[test]
    fn evidence_single_snapshot_torn_tail_is_excluded_from_parse_and_bytes() {
        use std::io::Write as _;
        let tmp = TempDir::new().unwrap();
        let paths = seed_full_mission(tmp.path());
        let pristine = std::fs::read(paths.events_file()).unwrap();
        // A crash-torn append the writer never finished: partial JSON, no
        // newline — the parser drops it (with a warning).
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(paths.events_file())
            .unwrap();
        file.write_all(b"{\"seq\":999,\"ts\":\"torn").unwrap();
        drop(file);

        let bundle = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        let shipped = bundle
            .files
            .iter()
            .find(|file| file.path == LOG_FILE)
            .expect("the raw log ships");
        assert_eq!(
            shipped.bytes, pristine,
            "the torn tail is in NEITHER the events nor the shipped bytes"
        );
        // The folds are unaffected (the same gate ladder as the clean log).
        assert_eq!(bundle.manifest.mission_id, "m-1");
        // And the shipped bytes alone reproduce the fold: a re-parse of the
        // bundle's log copy yields exactly the events the mission log's
        // valid prefix yields.
        let replay = paths.runs_dir().join("replay.jsonl");
        std::fs::write(&replay, &shipped.bytes).unwrap();
        let folded = crate::event_log::EventLog::read_events(&paths.events_file()).unwrap();
        let refolded = crate::event_log::EventLog::read_events(&replay).unwrap();
        assert_eq!(refolded.len(), folded.len());
        assert_eq!(
            refolded.last().map(|event| event.seq),
            folded.last().map(|event| event.seq)
        );
    }

    // ---- out-dir containment (12th-pass review) --------------------------

    /// `std::path::absolute` preserves `..` on this host, so containment
    /// must fold `..` lexically AND compare canonical paths: the
    /// `outside/../.kranz/missions/<id>/bundle` shape must be refused
    /// exactly like the direct in-mission path — before anything is written.
    #[test]
    fn evidence_outdir_containment_refuses_dotdot_escape_into_the_mission() {
        let tmp = TempDir::new().unwrap();
        let paths = seed_full_mission(tmp.path());
        let escape = tmp
            .path()
            .join("outside")
            .join("..")
            .join(".kranz")
            .join("missions")
            .join("m-1")
            .join("bundle");
        let result = export_evidence_bundle(tmp.path(), "m-1", &escape);
        assert!(result.is_err(), "the `..` shape must be refused");
        assert!(
            !paths.mission_dir().join("bundle").exists(),
            "nothing must be written on refusal"
        );
    }

    /// A symlinked out-dir component pointing into the audited mission:
    /// refused (the plan's symlink screen, with canonical containment behind
    /// it) — the bundle must never write through a link into the tree it
    /// audits. Unix-only, like every symlink-creating test in the repo.
    #[cfg(unix)]
    #[test]
    fn evidence_outdir_containment_refuses_a_symlinked_component() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let paths = seed_full_mission(tmp.path());
        let link = tmp.path().join("linked-out");
        symlink(paths.mission_dir(), &link).unwrap();
        let result = export_evidence_bundle(tmp.path(), "m-1", &link.join("bundle"));
        let err = result.expect_err("a symlinked out-dir component must be refused");
        assert!(err.to_string().contains("symlinked"), "{err}");
        assert!(
            !paths.mission_dir().join("bundle").exists(),
            "nothing must be written through the link"
        );
    }

    /// The honest path: a normal external out dir still exports, with
    /// multi-level missing components created through the no-follow pin.
    #[test]
    fn evidence_outdir_containment_normal_external_dir_works() {
        let tmp = TempDir::new().unwrap();
        seed_full_mission(tmp.path());
        let out = tmp.path().join("fresh").join("bundle-out");
        let outcome = export_evidence_bundle(tmp.path(), "m-1", &out).unwrap();
        assert!(outcome.files_written > 0);
        assert!(out.join(MANIFEST_FILE).is_file());
        assert!(out.join(LOG_FILE).is_file());
    }

    // ---- KRZ-343: the standards coverage matrix rides the bundle ----------

    /// A plan carrying a three-rule standards pin (KRZ-342's consent
    /// shape): one failed by a citing finding, one passed by a naming gate,
    /// one never evaluated.
    fn pinned_plan() -> Plan {
        let rule = |id: &str, revision: u64, status: &str| crate::types::PinnedRule {
            id: id.to_string(),
            revision,
            rfc: "RFC-001".to_string(),
            level: "must".to_string(),
            effective_status: status.to_string(),
            statement: format!("statement for {id}"),
            domains: Vec::new(),
            stages: vec!["validation".to_string()],
            when_paths: Vec::new(),
            task_classes: Vec::new(),
            checker: Some("gate:zz-gate".to_string()),
            waivable: false,
        };
        Plan {
            standards_manifest: Some(Box::new(crate::types::StandardsPin {
                pack_name: "zz-pack".to_string(),
                pack_dir: "vendor/pack".to_string(),
                standards_root: "standards".to_string(),
                digest: "ab".repeat(32),
                source: crate::types::StandardsPinSource::RepoTracked,
                task_class: None,
                touch_set: vec!["crates/**".to_string()],
                context_paths: Vec::new(),
                gates: Vec::new(),
                rules: vec![
                    rule("ZZ-FAIL-001", 2, "enforced"),
                    rule("ZZ-PASS-001", 1, "enforced"),
                    rule("ZZ-QUIET-001", 1, "enforced"),
                ],
            })),
            ..sample_plan()
        }
    }

    /// A pinned mission: approval + the resolution record, a gate pass
    /// naming ZZ-PASS-001 with a file artefact whose bytes were NEVER
    /// written (the unresolved-artefact arm), a finding citing ZZ-FAIL-001,
    /// and ZZ-QUIET-001 evaluated by nothing — ending COMPLETED.
    fn seed_pinned_mission(root: &Path) -> MissionPaths {
        let mut gate = gate_result(
            "zz-gate",
            GateSurface::FinalGate,
            GateKind::Deterministic,
            0,
            "file:runs/gone.jsonl",
        );
        if let EventKind::GateResult { rule_ids, .. } = &mut gate {
            *rule_ids = vec!["ZZ-PASS-001".to_string()];
        }
        seed_mission(
            root,
            "m-1",
            vec![
                created(),
                EventKind::PlanApproved {
                    plan: pinned_plan(),
                    base_sha: Some("deadbeef".to_string()),
                },
                EventKind::StandardsResolved {
                    source: "repo-tracked".to_string(),
                    pack_name: "zz-pack".to_string(),
                    standards_root: "standards".to_string(),
                    digest: "ab".repeat(32),
                    stage: "approval".to_string(),
                    task_class: None,
                    touch_set: vec!["crates/**".to_string()],
                    context_paths: Vec::new(),
                    rules: Vec::new(),
                    approval_seq: 2,
                },
                gate,
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".into(),
                    run_id: "v-1".into(),
                    finding: crate::types::Finding {
                        subject: "a-1".into(),
                        severity: "major".into(),
                        evidence: "the rule failed".into(),
                        suggested_fix: String::new(),
                        class: String::new(),
                        rule: Some(crate::types::RuleCitation {
                            id: "ZZ-FAIL-001".to_string(),
                            revision: 2,
                            source: "zz-pack standards".to_string(),
                            digest: "ab".repeat(32),
                            lifecycle: "enforced".to_string(),
                            level: "must".to_string(),
                            checker: Some("gate:zz-gate".to_string()),
                        }),
                    },
                },
                EventKind::MissionCompleted {},
            ],
        )
    }

    /// KRZ-343 (D-H): the bundle renders the coverage matrix from the SAME
    /// fold the replay computed — summary.md carries the dispositions with
    /// mechanism and artefact references, chain.json carries the machine
    /// form — and the assembly stays byte-identical across runs.
    #[test]
    fn flight_rules_provenance_bundle_renders_coverage_byte_identically() {
        let tmp = TempDir::new().unwrap();
        seed_pinned_mission(tmp.path());
        let first = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        let second = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        assert_eq!(first, second, "same log → byte-identical bundle");

        let summary = first
            .files
            .iter()
            .find(|file| file.path == SUMMARY_FILE)
            .expect("summary ships");
        let summary = String::from_utf8(summary.bytes.clone()).unwrap();
        assert!(
            summary.contains("## Flight Rules standards coverage"),
            "{summary}"
        );
        assert!(
            summary.contains("| ZZ-FAIL-001 | r2 | enforced | must | gate:zz-gate | failed |"),
            "{summary}"
        );
        assert!(
            summary.contains("| ZZ-PASS-001 | r1 | enforced | must | gate:zz-gate | passed |"),
            "{summary}"
        );
        assert!(
            summary
                .contains("| ZZ-QUIET-001 | r1 | enforced | must | gate:zz-gate | not-evaluated |"),
            "{summary}"
        );
        // The evidence cell names the artefact reference verbatim…
        assert!(
            summary.contains("gate.result seq 4 zz-gate pass `file:runs/gone.jsonl`"),
            "{summary}"
        );

        let chain = first
            .files
            .iter()
            .find(|file| file.path == CHAIN_FILE)
            .expect("the chain ships");
        let chain = String::from_utf8(chain.bytes.clone()).unwrap();
        assert!(chain.contains("\"standards\""), "{chain}");
        assert!(
            chain.contains("\"disposition\": \"not-evaluated\""),
            "{chain}"
        );
    }

    /// The replay contract survives the matrix (KRZ-343): a referenced
    /// artefact whose bytes are gone stays `unresolved` in the manifest —
    /// the coverage row still names the reference, and nothing about the
    /// missing bytes becomes an error or a pass.
    #[test]
    fn flight_rules_provenance_bundle_removed_artefacts_stay_unresolved() {
        let tmp = TempDir::new().unwrap();
        seed_pinned_mission(tmp.path());
        // runs/gone.jsonl was never written: the gate's file ref is gone.
        let bundle = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        let entry = manifest_entry(&bundle.manifest, "file:runs/gone.jsonl");
        assert_eq!(entry.status, Some(ArtefactStatus::Unresolved));
        assert_eq!(entry.path, None, "an unresolved entry has no bytes path");
        // …and the matrix still renders the reference, marked passed ONLY
        // because the gate stated a pass verdict — never because evidence
        // was absent.
        let summary = bundle
            .files
            .iter()
            .find(|file| file.path == SUMMARY_FILE)
            .expect("summary ships");
        let summary = String::from_utf8(summary.bytes.clone()).unwrap();
        assert!(summary.contains("`file:runs/gone.jsonl`"), "{summary}");
        assert!(
            summary.contains("Absence of evidence is never rendered as pass"),
            "{summary}"
        );
    }

    /// The byte-compat regression contract: the pre-Flight-Rules fixture
    /// (no pin, no standards events) bundles with NO coverage section and
    /// NO standards key in chain.json — byte-identical to what the export
    /// produced before KRZ-343.
    #[test]
    fn flight_rules_provenance_bundle_pre_flight_rules_mission_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        seed_full_mission(tmp.path());
        let bundle = assemble_evidence_bundle(tmp.path(), "m-1").unwrap();
        let summary = bundle
            .files
            .iter()
            .find(|file| file.path == SUMMARY_FILE)
            .expect("summary ships");
        let summary = String::from_utf8(summary.bytes.clone()).unwrap();
        assert!(
            !summary.contains("Flight Rules standards coverage"),
            "no pin, no matrix: {summary}"
        );
        let chain = bundle
            .files
            .iter()
            .find(|file| file.path == CHAIN_FILE)
            .expect("the chain ships");
        let chain = String::from_utf8(chain.bytes.clone()).unwrap();
        assert!(
            !chain.contains("\"standards\""),
            "a pre-Flight-Rules chain carries no standards key: {chain}"
        );
    }
}
