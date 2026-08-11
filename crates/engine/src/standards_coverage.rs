//! The Flight Rules rule coverage matrix (ticket
//! `.kranz/tickets/flight-rules-finding-provenance.md`, KRZ-343; design
//! `docs/scoping/flight-rules-engineering-standards.md`, decision D-H —
//! "standards evidence is first-class"): one fold over the mission's event
//! log that joins every applicable pinned rule to the evidence that named
//! it — gate verdicts carrying `ruleIds`, findings carrying a
//! [`crate::types::RuleCitation`] — and renders each rule's disposition as
//! `passed`, `failed`, `advisory`, `waived`, `not-evaluated`, or
//! `not-applicable`, with mechanism and artefact references.
//!
//! WHY a fold over the log alone: D-H makes the append-only log the audit
//! substrate, so the matrix must reconstruct without re-reading the pack,
//! the plan file, or any model prose. The pin itself rides the
//! `plan.approved` event's `standardsManifest` (KRZ-342 — the reducer
//! deliberately never re-reads one from `plan.revised`), the selection
//! provenance rides `standards.resolved`, the drift refusals ride
//! `standards.drifted`, and the evidence rides `gate.result` /
//! `validation.finding`. Every join is STRUCTURED: rule id + revision +
//! pinned digest, never a parse of `subject`/`evidence` text (the ticket's
//! "extend the shipped evidence spine; do NOT parse orchestrator prose").
//!
//! WHY absence is never pass (D-H): a rule nobody evaluated is
//! `not-evaluated`, full stop. `passed` requires POSITIVE evidence — a
//! `gate.result` pass that named the rule — and no failing join. The same
//! failing evidence renders `failed` against an effectively ENFORCED rule
//! and `advisory` against an approved one (D-B: an advisory rule's violation
//! could not have blocked). `waived` renders when every failing join on the
//! row is covered by a valid, unexpired, exactly-matching
//! `standards.waiver.approved` (KRZ-344, D-I) — the structured human
//! exception event, joined on the full binding (rule id + pinned revision +
//! manifest digest + approval seq + finding fingerprint + human surface).
//! The ordinary orchestrator finding-waiver remains insufficient authority:
//! a rule-cited finding waived as prose still renders `failed`/`advisory`.
//!
//! WHY `not-applicable` rows exist: evidence occasionally names a rule the
//! approved pin does not carry (a hand-authored or stale citation at
//! another revision/digest). Joining it would corrupt the matrix against
//! the consent artifact; dropping it would hide the citation. The fold
//! surfaces it as its own row — the mission was not judged by that rule,
//! and the audit says so.
//!
//! Determinism (the ticket's byte-identity hint): the fold consults no
//! clock, no filesystem, no hash map in output order — rows follow the
//! pin's stable id order (then not-applicable rows sorted by id/revision/
//! digest), evidence follows log seq order, and every timestamp is the
//! log's own data. Waiver expiry is judged against the LOG'S OWN FRONTIER
//! (the latest event instant in the mission's slice), never a wall clock,
//! so the same log folds byte-identically at any wall time — a replay
//! renders the world as of the evidence, and an enforcement decision
//! re-judges expiry against its own clock (KRZ-346). A mission with no pin
//! folds to `None`, and every consumer renders NOTHING — pre-Flight-Rules
//! logs stay byte-identical through report.md, provenance replay, and the
//! evidence bundle.

use crate::events::{Event, EventKind};
use crate::gate::GateVerdict;
use crate::types::{RuleCitation, StandardsPin};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One rule's disposition in the coverage matrix — D-H's closed vocabulary.
/// Serde kebab-case: the spellings are the design's own (`not-evaluated`,
/// `not-applicable`), so the machine form and the prose surfaces can never
/// drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleDisposition {
    /// Positive evidence joined the rule (a `gate.result` pass named it)
    /// and no failing evidence did. Never rendered from absence.
    Passed,
    /// Failing evidence joined an effectively ENFORCED rule.
    Failed,
    /// Failing evidence joined an effectively APPROVED rule — recorded,
    /// but the advisory lifecycle means it could not block (D-B).
    Advisory,
    /// Every joined failure was excepted by a valid, unexpired,
    /// exactly-matching `standards.waiver.approved` (KRZ-344, D-I) — the
    /// authorized human exception. One waiver subtracts exactly one
    /// failure: any failing join left uncovered renders
    /// `failed`/`advisory` instead.
    Waived,
    /// The rule applied but no evidence names it. The honest zero state:
    /// absence of evidence is never rendered as pass (D-H).
    NotEvaluated,
    /// Evidence cites a rule/revision/digest the approved pin does not
    /// carry — surfaced as its own row, never joined and never dropped.
    NotApplicable,
}

impl RuleDisposition {
    /// The wire/serde spelling for text surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Advisory => "advisory",
            Self::Waived => "waived",
            Self::NotEvaluated => "not-evaluated",
            Self::NotApplicable => "not-applicable",
        }
    }
}

/// One evidence join behind a row's disposition: which event named the
/// rule, the mechanism that produced it, its bearing, and the reference the
/// bytes re-found from — everything an auditor needs to locate the primary
/// record in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageEvidence {
    /// The event's seq — the join anchor into the log.
    pub seq: u64,
    /// The recording event's wire name (`gate.result` or
    /// `validation.finding`) — the entry names its own source kind.
    pub event: String,
    /// The mechanism that produced it: the gate name for a gate verdict,
    /// the citing run id for a finding.
    pub mechanism: String,
    /// The verdict bearing (`pass`/`fail`/`waived`; findings are failures
    /// by construction). `waived` marks a failing join covered by a valid
    /// `standards.waiver.approved` (KRZ-344) — the `waiver` field then
    /// names it.
    pub bearing: String,
    /// The artefact handle (a gate's `artefactRef`, verbatim — its
    /// resolution status stays with the gate ladder's total classifier) or
    /// the finding's subject.
    pub reference: String,
    /// The waiver that excepts this failure (KRZ-344, D-I), joined through
    /// the structured event — never parsed from orchestrator prose.
    /// Present exactly when `bearing` is `waived`; additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiver: Option<WaiverJoin>,
}

/// The waiver join behind a `waived` evidence entry (KRZ-344): everything
/// the audit needs to name the exception without re-reading the event —
/// its seq anchor, the approver principal + invocation surface, the
/// recorded reason, and the expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaiverJoin {
    /// The waiver event's seq — the join anchor into the log.
    pub seq: u64,
    pub approver: String,
    pub surface: String,
    pub reason: String,
    pub expires_at: DateTime<Utc>,
}

/// One row of the coverage matrix: a pinned rule (or a citation that
/// failed to join one), its pinned identity, the disposition the evidence
/// earned, and the joins behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleCoverage {
    pub id: String,
    pub revision: u64,
    /// The pinned effective lifecycle (`approved`/`enforced`) and RFC-2119
    /// level (`must`/`should`), verbatim from the pin (or from the citation
    /// on a `not-applicable` row).
    pub lifecycle: String,
    pub level: String,
    /// The pinned checker binding (`gate:<id>`, `agent-judgement`,
    /// `manual-attestation`) — the mechanism column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checker: Option<String>,
    /// The pinned normative statement, so the machine form names what was
    /// judged, not just ids. Empty (and absent) on `not-applicable` rows:
    /// a citation carries no statement, and the fold never invents one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub statement: String,
    pub disposition: RuleDisposition,
    /// The joined evidence in log (seq) order. Empty exactly when the
    /// disposition is `not-evaluated`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<CoverageEvidence>,
    /// Why a `not-applicable` row exists (the digest the citation joined
    /// against versus the pin's); absent on pinned rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One `standards.drifted` merge refusal, replayed: both digests and the
/// changed applicable enforced rules, pinned to its seq (KRZ-342 D-E/D-H).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftRecord {
    pub seq: u64,
    pub approved_digest: String,
    /// `None` when the live base no longer yielded a readable manifest at
    /// all (removed or malformed — the ultimate drift, failed closed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_digest: Option<String>,
    pub changed_rules: Vec<String>,
}

/// The folded coverage matrix for one mission: the approved pin's
/// identity, its resolution provenance, every applicable rule's
/// disposition, and any merge-time drift refusals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandardsCoverage {
    pub pack_name: String,
    pub pack_dir: String,
    pub standards_root: String,
    /// sha256 over the pinned normalized manifest — the content binding
    /// every citation join checks.
    pub digest: String,
    /// `repo-tracked` or `external-pinned` (the pin's source).
    pub source: String,
    /// The seq of the `plan.approved` event the pin rode in on.
    pub approval_seq: u64,
    /// The `standards.resolved` selection record's seq, when the log
    /// carries it (a hand-cut log may hold a pinned plan without it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_seq: Option<u64>,
    /// The effective-time evaluation instant: the `standards.resolved`
    /// event's own `ts` — resolution runs in the same approve_plan call as
    /// the emission, so the append stamp IS the instant the effective
    /// statuses were judged (events.rs's D-H verification note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Every applicable rule's disposition, in the pin's stable id order,
    /// then any `not-applicable` citation rows (sorted by id, revision,
    /// digest).
    pub rules: Vec<RuleCoverage>,
    /// `standards.drifted` merge refusals, in log order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drift: Vec<DriftRecord>,
}

/// Fold one mission's standards coverage matrix from its event slice.
/// `events` may contain other missions' events (filtered out, the
/// provenance idiom). Pure and total: no clock, no filesystem, no git — a
/// mission with no approved standards pin folds to `None`, and every
/// render surface then emits nothing.
///
/// The pin source is the LATEST `plan.approved` carrying a
/// `standardsManifest` — exactly the reducer's authority (`plan.revised`
/// never re-reads one, so neither does this fold; the two can never
/// disagree about which manifest the mission consented to).
pub fn standards_coverage(mission_id: &str, events: &[Event]) -> Option<StandardsCoverage> {
    let mut pin: Option<(u64, StandardsPin)> = None;
    let mut resolution: Option<(u64, DateTime<Utc>)> = None;
    let mut findings: Vec<(u64, &crate::types::Finding, &str)> = Vec::new();
    let mut gates: Vec<(u64, &str, GateVerdict, &str, &[String])> = Vec::new();
    let mut drift: Vec<DriftRecord> = Vec::new();
    let mut waivers: Vec<crate::standards_waiver::WaiverRecord> = Vec::new();
    // The fold's evaluation instant: the log's own frontier (the latest
    // event instant in the mission's slice), never a wall clock — waiver
    // expiry is judged as of the evidence, keeping replays byte-identical.
    let mut frontier: Option<DateTime<Utc>> = None;

    for event in events.iter().filter(|e| e.mission_id == mission_id) {
        frontier = Some(frontier.map_or(event.ts, |seen| seen.max(event.ts)));
        if let Some(record) = crate::standards_waiver::WaiverRecord::from_event(event) {
            waivers.push(record);
        }
        match &event.kind {
            EventKind::PlanApproved { plan, .. } => {
                // Mirror the reducer's fold EXACTLY: the pin is replaced on
                // every plan.approved, cleared included — a re-approval
                // carrying no manifest means no standards govern from that
                // point, and the resolution record resets with the pin.
                resolution = None;
                pin = plan
                    .standards_manifest
                    .as_deref()
                    .map(|manifest| (event.seq, manifest.clone()));
            }
            EventKind::StandardsResolved { approval_seq, .. } => {
                // The selection provenance joins the pin EXACTLY: the
                // resolution event names the `plan.approved` seq it pins,
                // so a re-approval's second resolution never attributes the
                // FIRST approval's instant to the standing pin — and a
                // hand-cut log whose seqs disagree records no resolution
                // rather than a mismatched one.
                if pin.as_ref().is_some_and(|(seq, _)| seq == approval_seq) {
                    resolution = Some((event.seq, event.ts));
                }
            }
            EventKind::ValidationFinding {
                finding, run_id, ..
            } => findings.push((event.seq, finding, run_id.as_str())),
            EventKind::GateResult {
                gate,
                verdict,
                artefact_ref,
                rule_ids,
                ..
            } => gates.push((event.seq, gate.as_str(), *verdict, artefact_ref, rule_ids)),
            EventKind::StandardsDrifted {
                approved_digest,
                current_digest,
                changed_rules,
                ..
            } => drift.push(DriftRecord {
                seq: event.seq,
                approved_digest: approved_digest.clone(),
                current_digest: current_digest.clone(),
                changed_rules: changed_rules.clone(),
            }),
            _ => {}
        }
    }

    let (approval_seq, pin) = pin?;
    // A pinned mission has at least its plan.approved event, so the
    // frontier exists; the expect can never fire.
    let now = frontier.expect("a pinned mission slice is non-empty");
    // Waivers consumed by a join: one waiver subtracts EXACTLY ONE failure
    // (D-I), so a matched waiver can never cover a second entry — even one
    // with an identical fingerprint.
    let mut used_waivers: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut rules: Vec<RuleCoverage> = Vec::new();
    for pinned in &pin.rules {
        let mut evidence: Vec<CoverageEvidence> = Vec::new();
        // Findings join on the FULL citation key — id, pinned revision, and
        // pinned digest — so a citation against any other manifest snapshot
        // can never leak into this pin's row.
        for (seq, finding, run_id) in &findings {
            if let Some(citation) = &finding.rule {
                if citation.id == pinned.id
                    && citation.revision == pinned.revision
                    && citation.digest == pin.digest
                {
                    let mut entry = CoverageEvidence {
                        seq: *seq,
                        event: "validation.finding".to_string(),
                        mechanism: (*run_id).to_string(),
                        bearing: "fail".to_string(),
                        reference: finding.subject.clone(),
                        waiver: None,
                    };
                    // KRZ-344 (D-I): a valid, unexpired, exactly-matching
                    // human waiver excepts THIS one failure. The join is
                    // the structured event's full binding — rule id,
                    // pinned revision, manifest digest, approval seq,
                    // finding fingerprint, human surface — never a parse
                    // of orchestrator decision prose.
                    let fingerprint = crate::standards_waiver::finding_fingerprint(run_id, finding);
                    if let Some(waiver) = waivers.iter().find(|waiver| {
                        !used_waivers.contains(&waiver.seq)
                            && crate::standards_waiver::waiver_covers(
                                waiver,
                                pinned,
                                &pin,
                                approval_seq,
                                &fingerprint,
                                now,
                            )
                    }) {
                        used_waivers.insert(waiver.seq);
                        entry.bearing = "waived".to_string();
                        entry.waiver = Some(WaiverJoin {
                            seq: waiver.seq,
                            approver: waiver.approver.clone(),
                            surface: waiver.surface.clone(),
                            reason: waiver.reason.clone(),
                            expires_at: waiver.expires_at,
                        });
                    }
                    evidence.push(entry);
                }
            }
        }
        for (seq, gate, verdict, artefact_ref, rule_ids) in &gates {
            if rule_ids.iter().any(|id| id == &pinned.id) {
                evidence.push(CoverageEvidence {
                    seq: *seq,
                    event: "gate.result".to_string(),
                    mechanism: (*gate).to_string(),
                    bearing: match verdict {
                        GateVerdict::Pass => "pass".to_string(),
                        GateVerdict::Fail => "fail".to_string(),
                    },
                    reference: (*artefact_ref).to_string(),
                    // A waiver binds a finding fingerprint; gate-result
                    // failures join no waiver in this slice — the
                    // enforced-MUST gate binding (KRZ-346) owns its own
                    // exception check.
                    waiver: None,
                });
            }
        }
        evidence.sort_by_key(|entry| entry.seq);
        let unwaived_finding_failure = evidence
            .iter()
            .any(|entry| entry.event == "validation.finding" && entry.bearing == "fail");
        let gate_failure = evidence
            .iter()
            .any(|entry| entry.event == "gate.result" && entry.bearing == "fail");
        let waived_finding = evidence
            .iter()
            .any(|entry| entry.event == "validation.finding" && entry.bearing == "waived");
        let latest_pass = evidence
            .iter()
            .filter(|entry| entry.bearing == "pass")
            .map(|entry| entry.seq)
            .max();
        let latest_failure = evidence
            .iter()
            .filter(|entry| entry.bearing == "fail")
            .map(|entry| entry.seq)
            .max();
        let mode = crate::standards_enforcement::rule_mode(pinned);
        let disposition = if latest_pass
            .is_some_and(|pass| latest_failure.is_none_or(|failure| pass > failure))
        {
            // Coverage keeps the full history below, but disposition is the
            // latest checker state. A repaired rule that later passes must
            // not remain failed forever or offer an obsolete waiver action.
            RuleDisposition::Passed
        } else if unwaived_finding_failure
            || (gate_failure
                // A standards gate event and its cited finding are two
                // evidence views of ONE checker failure. An exact D-I waiver
                // binds the finding, so the companion gate event must not
                // resurrect the same block in replay.
                && !(mode == crate::standards_enforcement::RuleMode::Authoritative
                    && waived_finding))
        {
            if mode == crate::standards_enforcement::RuleMode::Authoritative {
                RuleDisposition::Failed
            } else {
                RuleDisposition::Advisory
            }
        } else if evidence.iter().any(|entry| entry.bearing == "waived") {
            // Every failing join is covered by a valid waiver (a surviving
            // "fail" bearing took the branch above); the row names the
            // exception rather than the block.
            RuleDisposition::Waived
        } else if evidence.iter().any(|entry| entry.bearing == "pass") {
            RuleDisposition::Passed
        } else {
            RuleDisposition::NotEvaluated
        };
        rules.push(RuleCoverage {
            id: pinned.id.clone(),
            revision: pinned.revision,
            lifecycle: pinned.effective_status.clone(),
            level: pinned.level.clone(),
            checker: pinned.checker.clone(),
            statement: pinned.statement.clone(),
            disposition,
            evidence,
            note: None,
        });
    }

    // Citations that join NO pinned rule (stale revision, another digest,
    // or a rule the mission never pinned): one not-applicable row per
    // distinct (id, revision, digest), evidence in seq order. A finding's
    // own row in the validation history is unaffected — this row is the
    // matrix's honest "cited, but not this mission's approved policy".
    let mut orphans: Vec<((String, u64, String), RuleCoverage)> = Vec::new();
    for (seq, finding, run_id) in &findings {
        let Some(citation) = &finding.rule else {
            continue;
        };
        let joins = pin.rules.iter().any(|pinned| {
            pinned.id == citation.id
                && pinned.revision == citation.revision
                && pin.digest == citation.digest
        });
        if joins {
            continue;
        }
        let entry = CoverageEvidence {
            seq: *seq,
            event: "validation.finding".to_string(),
            mechanism: (*run_id).to_string(),
            bearing: "fail".to_string(),
            reference: finding.subject.clone(),
            // A not-applicable citation never joins a waiver: the waiver
            // binds the PINNED rule, and this citation joined none.
            waiver: None,
        };
        let key = (
            citation.id.clone(),
            citation.revision,
            citation.digest.clone(),
        );
        match orphans.iter_mut().find(|(seen, _)| *seen == key) {
            Some((_, row)) => row.evidence.push(entry),
            None => orphans.push((key, not_applicable_row(citation, &pin, entry))),
        }
    }
    orphans.sort_by(|(a, _), (b, _)| a.cmp(b));
    rules.extend(orphans.into_iter().map(|(_, row)| row));

    Some(StandardsCoverage {
        pack_name: pin.pack_name.clone(),
        pack_dir: pin.pack_dir.clone(),
        standards_root: pin.standards_root.clone(),
        digest: pin.digest.clone(),
        source: pin.source.as_str().to_string(),
        approval_seq,
        resolution_seq: resolution.map(|(seq, _)| seq),
        resolved_at: resolution.map(|(_, ts)| ts),
        rules,
        drift,
    })
}

/// One orphan citation's row: the citation's own identity spellings (the
/// pin cannot vouch for them) and a note naming the digest mismatch.
fn not_applicable_row(
    citation: &RuleCitation,
    pin: &StandardsPin,
    entry: CoverageEvidence,
) -> RuleCoverage {
    RuleCoverage {
        id: citation.id.clone(),
        revision: citation.revision,
        lifecycle: citation.lifecycle.clone(),
        level: citation.level.clone(),
        checker: citation.checker.clone(),
        statement: String::new(),
        disposition: RuleDisposition::NotApplicable,
        evidence: vec![entry],
        note: Some(format!(
            "cited against digest sha256:{} — the approved pin (sha256:{}) carries no such \
             rule/revision, so the mission was not judged by it",
            citation.digest, pin.digest
        )),
    }
}

/// Collapse whitespace runs to single spaces (the evidence_bundle
/// `one_line` idiom) so one logical cell stays one physical line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escape a markdown table cell (the evidence_bundle `md_cell` idiom):
/// pipes would break the column structure, newlines the row structure.
fn md_cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

/// The coverage matrix as markdown — the ONE renderer report.md and the
/// evidence-bundle summary share, so the two surfaces can never drift.
/// Pure: same coverage in, same bytes out.
pub fn render_coverage_markdown(coverage: &StandardsCoverage) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "## Flight Rules standards coverage\n");
    let _ = writeln!(
        out,
        "Pack `{}` (`{}`, source {}) — standards root `{}`, digest `sha256:{}`.",
        coverage.pack_name,
        coverage.pack_dir,
        coverage.source,
        coverage.standards_root,
        coverage.digest
    );
    let resolution = match (coverage.resolution_seq, coverage.resolved_at) {
        (Some(seq), Some(ts)) => format!(
            "selection recorded as `standards.resolved` seq {seq} (evaluated {})",
            ts.to_rfc3339()
        ),
        _ => "no `standards.resolved` event in this log (a hand-cut log)".to_string(),
    };
    let _ = writeln!(
        out,
        "Pinned at plan approval (seq {}); {resolution}. This pin — not a later branch or \
         filesystem read — governs the matrix.\n",
        coverage.approval_seq
    );
    if coverage.rules.is_empty() {
        let _ = writeln!(
            out,
            "No rules applied to this mission's selection inputs.\n"
        );
    } else {
        let _ = writeln!(
            out,
            "| rule | rev | lifecycle | level | mechanism | disposition | evidence |\n\
             |------|----:|-----------|-------|-----------|-------------|----------|"
        );
        for rule in &coverage.rules {
            let checker = rule.checker.as_deref().unwrap_or("-");
            let evidence = if rule.evidence.is_empty() {
                "—".to_string()
            } else {
                rule.evidence
                    .iter()
                    .map(|entry| {
                        let mut cell = format!(
                            "{} seq {} {} {} `{}`",
                            entry.event,
                            entry.seq,
                            md_cell(&entry.mechanism),
                            entry.bearing,
                            md_cell(&entry.reference)
                        );
                        // A waived failure names its exception through the
                        // structured event (KRZ-344, D-I): seq anchor,
                        // approver + surface, expiry, reason.
                        if let Some(waiver) = &entry.waiver {
                            let _ = write!(
                                cell,
                                " (waiver seq {} by {} via {}, expires {}: \"{}\")",
                                waiver.seq,
                                md_cell(&waiver.approver),
                                md_cell(&waiver.surface),
                                waiver.expires_at.to_rfc3339(),
                                md_cell(&waiver.reason)
                            );
                        }
                        cell
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let disposition = match &rule.note {
                Some(note) => format!("{} ({})", rule.disposition.as_str(), md_cell(note)),
                None => rule.disposition.as_str().to_string(),
            };
            let _ = writeln!(
                out,
                "| {} | r{} | {} | {} | {} | {} | {} |",
                md_cell(&rule.id),
                rule.revision,
                rule.lifecycle,
                rule.level,
                md_cell(checker),
                disposition,
                evidence
            );
        }
        let _ = writeln!(
            out,
            "\nAbsence of evidence is never rendered as pass: `not-evaluated` means no gate \
             verdict or finding named the rule, and `not-applicable` marks citations the \
             approved pin does not carry. `waived` names an authorized human exception \
             (`standards.waiver.approved`, D-I) — one waiver subtracts exactly one failure, \
             and any rule, finding, scope, diff, or expiry change restores the block.\n"
        );
    }
    if !coverage.drift.is_empty() {
        let _ = writeln!(
            out,
            "Policy drift refusals (merge re-resolved the live base against the approved \
             pin):"
        );
        for record in &coverage.drift {
            let current = record
                .current_digest
                .as_deref()
                .map(|digest| format!("sha256:{digest}"))
                .unwrap_or_else(|| "(no readable manifest on the live base)".to_string());
            let _ = writeln!(
                out,
                "- seq {}: approved `sha256:{}` → current `{}`: {}",
                record.seq,
                record.approved_digest,
                current,
                record.changed_rules.join("; ")
            );
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PinnedRule, Plan, StandardsPinSource};

    // ---- fixtures ---------------------------------------------------------

    /// A fixed instant for every fixture event (the fold treats ts as data;
    /// a constant keeps expected outputs pinnable).
    fn ts() -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 1, 2, 3, 4, 5).unwrap()
    }

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: ts(),
            mission_id: "m-1".to_string(),
            kind,
        }
    }

    fn pinned_rule(id: &str, revision: u64, status: &str, level: &str) -> PinnedRule {
        PinnedRule {
            id: id.to_string(),
            revision,
            rfc: "RFC-001".to_string(),
            level: level.to_string(),
            effective_status: status.to_string(),
            statement: format!("statement for {id}"),
            domains: Vec::new(),
            stages: vec!["validation".to_string()],
            when_paths: Vec::new(),
            task_classes: Vec::new(),
            checker: Some("gate:zz-gate".to_string()),
            waivable: false,
        }
    }

    fn pin(rules: Vec<PinnedRule>) -> StandardsPin {
        StandardsPin {
            pack_name: "zz-pack".to_string(),
            pack_dir: "vendor/pack".to_string(),
            standards_root: "standards".to_string(),
            digest: "ab".repeat(32),
            source: StandardsPinSource::RepoTracked,
            task_class: None,
            touch_set: vec!["crates/**".to_string()],
            context_paths: Vec::new(),
            gates: Vec::new(),
            rules,
        }
    }

    fn plan_with_pin(rules: Vec<PinnedRule>) -> Plan {
        Plan {
            goal: "g".to_string(),
            validation_contract: Vec::new(),
            milestones: Vec::new(),
            considered_alternatives: None,
            command_grants: Vec::new(),
            touch_set: Vec::new(),
            standards_manifest: Some(Box::new(pin(rules))),
        }
    }

    fn citation(id: &str, revision: u64, digest: &str, status: &str) -> RuleCitation {
        RuleCitation {
            id: id.to_string(),
            revision,
            source: "zz-pack standards".to_string(),
            digest: digest.to_string(),
            lifecycle: status.to_string(),
            level: "must".to_string(),
            checker: Some("gate:zz-gate".to_string()),
        }
    }

    fn finding_with_rule(subject: &str, rule: Option<RuleCitation>) -> crate::types::Finding {
        crate::types::Finding {
            subject: subject.to_string(),
            severity: "major".to_string(),
            evidence: "zz evidence".to_string(),
            suggested_fix: String::new(),
            class: String::new(),
            rule,
        }
    }

    fn gate_result(
        gate: &str,
        verdict: GateVerdict,
        artefact_ref: &str,
        rule_ids: Vec<String>,
    ) -> EventKind {
        EventKind::GateResult {
            gate: gate.to_string(),
            surface: crate::gate::GateSurface::FinalGate,
            kind: crate::gate::GateKind::Deterministic,
            index: 0,
            verdict,
            artefact_ref: artefact_ref.to_string(),
            artefact_detail: None,
            score: None,
            threshold: None,
            rule_ids,
        }
    }

    /// The full-matrix fixture: a pin with four rules — one passed (gate
    /// pass named it), one failed (enforced, cited by a finding), one
    /// advisory (approved, cited by a finding), one never evaluated — plus
    /// one orphan citation (a revision the pin does not carry). The pin's
    /// rules are in stable id order, exactly as the resolver pins them.
    fn full_matrix_events() -> Vec<Event> {
        let rules = vec![
            pinned_rule("ZZ-ADV-001", 3, "approved", "should"),
            pinned_rule("ZZ-FAIL-001", 2, "enforced", "must"),
            pinned_rule("ZZ-PASS-001", 1, "enforced", "must"),
            pinned_rule("ZZ-QUIET-001", 1, "enforced", "must"),
        ];
        vec![
            ev(
                1,
                EventKind::PlanApproved {
                    plan: plan_with_pin(rules),
                    base_sha: Some("deadbeef".to_string()),
                },
            ),
            ev(
                2,
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
                    approval_seq: 1,
                },
            ),
            ev(
                3,
                gate_result(
                    "zz-gate",
                    GateVerdict::Pass,
                    "file:runs/gate-zz.jsonl",
                    vec!["ZZ-PASS-001".to_string()],
                ),
            ),
            ev(
                4,
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: "v-1".to_string(),
                    finding: finding_with_rule(
                        "a-1",
                        Some(citation("ZZ-FAIL-001", 2, &"ab".repeat(32), "enforced")),
                    ),
                },
            ),
            ev(
                5,
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: "v-1".to_string(),
                    finding: finding_with_rule(
                        "a-2",
                        Some(citation("ZZ-ADV-001", 3, &"ab".repeat(32), "approved")),
                    ),
                },
            ),
            // The orphan: the pin carries ZZ-FAIL-001 at r2, never r1.
            ev(
                6,
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: "v-2".to_string(),
                    finding: finding_with_rule(
                        "a-3",
                        Some(citation("ZZ-FAIL-001", 1, &"ab".repeat(32), "enforced")),
                    ),
                },
            ),
        ]
    }

    // ---- the fold --------------------------------------------------------

    #[test]
    fn flight_rules_provenance_coverage_matrix_assigns_each_disposition() {
        let coverage = standards_coverage("m-1", &full_matrix_events()).expect("a pin folds");
        assert_eq!(coverage.pack_name, "zz-pack");
        assert_eq!(coverage.digest, "ab".repeat(32));
        assert_eq!(coverage.approval_seq, 1);
        assert_eq!(coverage.resolution_seq, Some(2));
        assert_eq!(coverage.resolved_at, Some(ts()));

        let by_key = |id: &str, revision: u64| {
            coverage
                .rules
                .iter()
                .find(|row| row.id == id && row.revision == revision)
                .unwrap_or_else(|| panic!("row {id} r{revision} present"))
        };
        // passed: the gate pass named ZZ-PASS-001; positive evidence.
        let passed = by_key("ZZ-PASS-001", 1);
        assert_eq!(passed.disposition, RuleDisposition::Passed);
        assert_eq!(passed.evidence.len(), 1);
        assert_eq!(passed.evidence[0].event, "gate.result");
        assert_eq!(passed.evidence[0].mechanism, "zz-gate");
        assert_eq!(passed.evidence[0].bearing, "pass");
        assert_eq!(passed.evidence[0].reference, "file:runs/gate-zz.jsonl");
        // failed: an unwaived finding cites an enforced rule at the pinned
        // revision and digest.
        let failed = by_key("ZZ-FAIL-001", 2);
        assert_eq!(failed.disposition, RuleDisposition::Failed);
        assert_eq!(failed.evidence.len(), 1);
        assert_eq!(failed.evidence[0].event, "validation.finding");
        assert_eq!(failed.evidence[0].reference, "a-1");
        // advisory: the same failing evidence against an approved rule.
        let advisory = by_key("ZZ-ADV-001", 3);
        assert_eq!(advisory.disposition, RuleDisposition::Advisory);
        // not-evaluated: applicable, but nothing named it.
        let quiet = by_key("ZZ-QUIET-001", 1);
        assert_eq!(quiet.disposition, RuleDisposition::NotEvaluated);
        assert!(quiet.evidence.is_empty());
        // not-applicable: the r1 citation joins no pinned rule.
        let orphan = by_key("ZZ-FAIL-001", 1);
        assert_eq!(orphan.disposition, RuleDisposition::NotApplicable);
        assert_eq!(orphan.evidence.len(), 1);
        assert_eq!(orphan.evidence[0].reference, "a-3");
        let note = orphan.note.as_deref().expect("the row explains itself");
        assert!(
            note.contains(&format!("sha256:{}", "ab".repeat(32))),
            "{note}"
        );
        // Pin order first (the fixture is stable id order, like a real
        // pin), the orphan row after.
        let ids: Vec<(&str, u64)> = coverage
            .rules
            .iter()
            .map(|row| (row.id.as_str(), row.revision))
            .collect();
        assert_eq!(
            ids,
            [
                ("ZZ-ADV-001", 3),
                ("ZZ-FAIL-001", 2),
                ("ZZ-PASS-001", 1),
                ("ZZ-QUIET-001", 1),
                ("ZZ-FAIL-001", 1),
            ]
        );
    }

    #[test]
    fn flight_rules_enforcement_enforced_should_failure_is_advisory() {
        let rule = pinned_rule("ZZ-SHOULD-001", 1, "enforced", "should");
        let events = vec![
            ev(
                1,
                EventKind::PlanApproved {
                    plan: plan_with_pin(vec![rule]),
                    base_sha: Some("deadbeef".to_string()),
                },
            ),
            ev(
                2,
                gate_result(
                    "zz-gate",
                    GateVerdict::Fail,
                    "inline:failed",
                    vec!["ZZ-SHOULD-001".to_string()],
                ),
            ),
        ];
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");
        assert_eq!(coverage.rules[0].disposition, RuleDisposition::Advisory);
    }

    #[test]
    fn flight_rules_dashboard_latest_pass_supersedes_historical_failure() {
        let rule = pinned_rule("ZZ-REPAIRED-001", 1, "enforced", "must");
        let events = vec![
            ev(
                1,
                EventKind::PlanApproved {
                    plan: plan_with_pin(vec![rule]),
                    base_sha: Some("deadbeef".to_string()),
                },
            ),
            ev(
                2,
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: crate::reducer::ENGINE_RUN_ID.to_string(),
                    finding: finding_with_rule(
                        "flight-rule:ZZ-REPAIRED-001",
                        Some(citation("ZZ-REPAIRED-001", 1, &"ab".repeat(32), "enforced")),
                    ),
                },
            ),
            ev(
                3,
                gate_result(
                    "zz-gate",
                    GateVerdict::Pass,
                    "inline:passed after repair",
                    vec!["ZZ-REPAIRED-001".to_string()],
                ),
            ),
        ];
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");
        assert_eq!(coverage.rules[0].disposition, RuleDisposition::Passed);
        assert_eq!(
            coverage.rules[0].evidence.len(),
            2,
            "history remains visible"
        );
    }

    // ---- additive contract fields (D-H) -----------------------------------

    /// The finding's rule citation is additive: a pre-KRZ-343 finding (no
    /// `rule` key) folds with `None`, a rule-less finding serializes
    /// byte-identically to the legacy shape, and a cited finding
    /// round-trips the full join key verbatim.
    #[test]
    fn flight_rules_provenance_finding_rule_citation_folds_byte_compatibly() {
        // A legacy finding — no `rule` key — folds with the citation None.
        let legacy = r#"{
            "subject": "a-1",
            "severity": "major",
            "evidence": "it broke",
            "suggestedFix": "fix it",
            "class": ""
        }"#;
        let folded: crate::types::Finding = serde_json::from_str(legacy).unwrap();
        assert_eq!(folded.rule, None);

        // A rule-less finding serializes byte-identically to the legacy
        // shape: the new field never hits the wire as null/empty.
        let plain = finding_with_rule("a-1", None);
        let json = serde_json::to_string(&plain).unwrap();
        assert!(
            !json.contains("rule"),
            "a rule-less finding carries no rule key: {json}"
        );
        let reparsed: crate::types::Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(reparsed.rule, None);

        // A cited finding round-trips the whole join key.
        let cited = finding_with_rule(
            "a-1",
            Some(citation("ZZ-FAIL-001", 2, &"ab".repeat(32), "enforced")),
        );
        let json = serde_json::to_value(&cited).unwrap();
        assert_eq!(json["rule"]["id"], "ZZ-FAIL-001");
        assert_eq!(json["rule"]["revision"], 2);
        assert_eq!(json["rule"]["digest"], "ab".repeat(32));
        assert_eq!(json["rule"]["lifecycle"], "enforced");
        assert_eq!(json["rule"]["level"], "must");
        assert_eq!(json["rule"]["checker"], "gate:zz-gate");
        assert_eq!(json["rule"]["source"], "zz-pack standards");
        // …and the human/assertion handle stays exactly what it was —
        // provenance is never smuggled into `subject`.
        assert_eq!(json["subject"], "a-1");
        let back: crate::types::Finding = serde_json::from_value(json).unwrap();
        assert_eq!(back.rule, cited.rule);
        assert_eq!(back.subject, cited.subject);
        assert_eq!(back.class, cited.class);
    }

    /// `ruleIds` on `gate.result` is additive: a gate with no standards
    /// linkage serializes byte-identically to the pre-KRZ-343 shape, a
    /// linked gate round-trips its ids, and the emission mapping
    /// ([`crate::gate_results::gate_result_events`]) carries them from the
    /// outcome verbatim.
    #[test]
    fn flight_rules_provenance_gate_result_rule_ids_are_additive() {
        // The wire shape without linkage: no ruleIds key at all.
        let plain = gate_result("zz-gate", GateVerdict::Pass, "ref", Vec::new());
        let json = serde_json::to_value(&plain).unwrap();
        assert!(
            json["payload"].get("ruleIds").is_none(),
            "no linkage, no key: {json}"
        );

        // With linkage: the ids ride the payload and round-trip.
        let linked = gate_result(
            "zz-gate",
            GateVerdict::Pass,
            "ref",
            vec!["ZZ-PASS-001".to_string(), "ZZ-QUIET-001".to_string()],
        );
        let json = serde_json::to_value(&linked).unwrap();
        assert_eq!(
            json["payload"]["ruleIds"],
            serde_json::json!(["ZZ-PASS-001", "ZZ-QUIET-001"])
        );
        let back: EventKind = serde_json::from_value(json).unwrap();
        let EventKind::GateResult { rule_ids, .. } = back else {
            panic!("wrong variant");
        };
        assert_eq!(rule_ids, ["ZZ-PASS-001", "ZZ-QUIET-001"]);

        // The emission mapping: outcome.rule_ids → event, in pipeline order.
        let outcome = crate::gate::GateOutcome::pass(crate::gate::ArtefactRef::new("ref zz"))
            .with_rule_ids(vec!["ZZ-PASS-001".to_string()]);
        let reports = vec![crate::gate::GateReport {
            name: "zz-gate".to_string(),
            kind: crate::gate::GateKind::Deterministic,
            outcome,
        }];
        let events =
            crate::gate_results::gate_result_events(crate::gate::GateSurface::FinalGate, &reports);
        let EventKind::GateResult { rule_ids, .. } = &events[0] else {
            panic!("wrong variant");
        };
        assert_eq!(rule_ids, &["ZZ-PASS-001".to_string()]);
    }

    /// D-H's central honesty rule: an applicable rule nothing evaluated is
    /// `not-evaluated` — never pass. The markdown says so out loud, and the
    /// only `passed` row in the full fixture is the one with a passing gate
    /// verdict behind it.
    #[test]
    fn flight_rules_provenance_absent_evidence_is_never_pass() {
        let coverage = standards_coverage("m-1", &full_matrix_events()).expect("a pin folds");
        let quiet = coverage
            .rules
            .iter()
            .find(|row| row.id == "ZZ-QUIET-001")
            .expect("pinned rule present");
        assert_eq!(quiet.disposition, RuleDisposition::NotEvaluated);
        assert!(quiet.evidence.is_empty());

        let md = render_coverage_markdown(&coverage);
        assert!(
            md.contains(
                "| ZZ-QUIET-001 | r1 | enforced | must | gate:zz-gate | not-evaluated | — |"
            ),
            "{md}"
        );
        assert!(
            md.contains("Absence of evidence is never rendered as pass"),
            "{md}"
        );
        // The one `passed` cell belongs to the gate-verdict row.
        let passed_lines: Vec<&str> = md
            .lines()
            .filter(|line| line.contains("| passed |"))
            .collect();
        assert_eq!(passed_lines.len(), 1, "{md}");
        assert!(passed_lines[0].contains("ZZ-PASS-001"), "{md}");
    }

    /// The `waived` slot renders (D-H's vocabulary), though this slice wires
    /// no waiver signal — KRZ-344's `standards.waiver.approved` is the fold
    /// input. This test pins the render against a synthetic row so the slot
    /// cannot rot.
    #[test]
    fn flight_rules_provenance_waived_disposition_slot_renders() {
        let mut coverage = standards_coverage("m-1", &full_matrix_events()).expect("a pin folds");
        let row = coverage
            .rules
            .iter_mut()
            .find(|row| row.id == "ZZ-FAIL-001" && row.revision == 2)
            .expect("the failed row");
        row.disposition = RuleDisposition::Waived;
        row.evidence[0].bearing = "waived".to_string();
        let md = render_coverage_markdown(&coverage);
        assert!(
            md.contains("| ZZ-FAIL-001 | r2 | enforced | must | gate:zz-gate | waived |"),
            "{md}"
        );
        // Serde kebab-case: the machine form spells it the design's way.
        let json = serde_json::to_value(RuleDisposition::Waived).unwrap();
        assert_eq!(json, "waived");
        let json = serde_json::to_value(RuleDisposition::NotEvaluated).unwrap();
        assert_eq!(json, "not-evaluated");
        let json = serde_json::to_value(RuleDisposition::NotApplicable).unwrap();
        assert_eq!(json, "not-applicable");
    }

    /// A `standards.drifted` refusal is named in the matrix with both
    /// digests and the changed applicable rules (D-H/KRZ-342) — the merge
    /// refusal's evidence joins the same record the dispositions live in.
    #[test]
    fn flight_rules_provenance_drift_refusal_is_named_with_both_digests() {
        let mut events = full_matrix_events();
        events.push(ev(
            7,
            EventKind::StandardsDrifted {
                approved_digest: "ab".repeat(32),
                current_digest: Some("cd".repeat(32)),
                surface: "merge".to_string(),
                changed_rules: vec![
                    "ZZ-FAIL-001 r2 -> r3 (changed on the live base since approval)".to_string(),
                ],
            },
        ));
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");
        assert_eq!(coverage.drift.len(), 1);
        assert_eq!(coverage.drift[0].seq, 7);
        assert_eq!(coverage.drift[0].approved_digest, "ab".repeat(32));
        assert_eq!(
            coverage.drift[0].current_digest.as_deref(),
            Some("cd".repeat(32).as_str())
        );
        let md = render_coverage_markdown(&coverage);
        assert!(md.contains("Policy drift refusals"), "{md}");
        assert!(
            md.contains(&format!("approved `sha256:{}`", "ab".repeat(32))),
            "{md}"
        );
        assert!(
            md.contains(&format!("current `sha256:{}`", "cd".repeat(32))),
            "{md}"
        );
        assert!(md.contains("ZZ-FAIL-001 r2 -> r3"), "{md}");
    }

    /// Byte-identity: the same log folds to the same struct, the same
    /// markdown, and the same JSON — twice, independently (the ticket's
    /// determinism hint). The matrix consults no clock and no filesystem.
    #[test]
    fn flight_rules_provenance_same_inputs_render_byte_identical() {
        let events = full_matrix_events();
        let first = standards_coverage("m-1", &events).expect("a pin folds");
        let second = standards_coverage("m-1", &events).expect("a pin folds");
        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_string_pretty(&first).unwrap(),
            serde_json::to_string_pretty(&second).unwrap()
        );
        assert_eq!(
            render_coverage_markdown(&first),
            render_coverage_markdown(&second)
        );
        // Other missions' events in the slice never leak into this
        // mission's matrix (the provenance fold's discipline).
        let mut mixed = full_matrix_events();
        mixed.push(Event {
            mission_id: "m-other".to_string(),
            ..ev(
                99,
                EventKind::StandardsDrifted {
                    approved_digest: "x".to_string(),
                    current_digest: None,
                    surface: "merge".to_string(),
                    changed_rules: vec!["ZZ-X r1".to_string()],
                },
            )
        });
        let filtered = standards_coverage("m-1", &mixed).expect("a pin folds");
        assert_eq!(filtered, first);
        assert!(filtered.drift.is_empty());
    }

    /// Pre-Flight-Rules logs fold to `None` — no pin, no matrix, and every
    /// consumer renders nothing (the byte-compat regression contract). A
    /// `plan.revised` carrying a manifest NEVER supplies one either: the
    /// reducer deliberately keeps the approval-time pin authority, and this
    /// fold mirrors it exactly.
    #[test]
    fn flight_rules_provenance_pre_flight_rules_logs_fold_to_none() {
        let legacy = vec![
            ev(
                1,
                EventKind::MissionCreated {
                    goal: "g".to_string(),
                    base_branch: "main".to_string(),
                    mission_branch: "kranz/mission-m-1".to_string(),
                    config: crate::types::MissionConfig::default(),
                },
            ),
            ev(
                2,
                EventKind::PlanApproved {
                    plan: Plan {
                        goal: "g".to_string(),
                        validation_contract: Vec::new(),
                        milestones: Vec::new(),
                        considered_alternatives: None,
                        command_grants: Vec::new(),
                        touch_set: Vec::new(),
                        standards_manifest: None,
                    },
                    base_sha: None,
                },
            ),
            ev(
                3,
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: "v-1".to_string(),
                    finding: finding_with_rule("a-1", None),
                },
            ),
        ];
        assert!(standards_coverage("m-1", &legacy).is_none());

        // A revision's carried manifest is never folded (apply_revised_plan
        // keeps the approval pin; the fold agrees).
        let mut revised = legacy.clone();
        revised.push(ev(
            4,
            EventKind::PlanRevised {
                revision: 1,
                plan: plan_with_pin(vec![pinned_rule("ZZ-SNEAK-001", 1, "enforced", "must")]),
            },
        ));
        assert!(standards_coverage("m-1", &revised).is_none());
    }

    /// The markdown renderer's full shape: pin identity header, the
    /// resolution provenance line, one row per disposition with mechanism
    /// and artefact references, and the orphan's self-explaining note.
    #[test]
    fn flight_rules_provenance_markdown_renders_pin_rows_and_mechanisms() {
        let coverage = standards_coverage("m-1", &full_matrix_events()).expect("a pin folds");
        let md = render_coverage_markdown(&coverage);
        assert!(md.contains("## Flight Rules standards coverage"), "{md}");
        assert!(
            md.contains(&format!(
                "Pack `zz-pack` (`vendor/pack`, source repo-tracked) — standards root \
                 `standards`, digest `sha256:{}`.",
                "ab".repeat(32)
            )),
            "{md}"
        );
        assert!(md.contains("Pinned at plan approval (seq 1)"), "{md}");
        assert!(md.contains("`standards.resolved` seq 2"), "{md}");
        assert!(md.contains("evaluated 2026-01-02T03:04:05"), "{md}");
        // Mechanism and artefact reference ride the evidence cell.
        assert!(
            md.contains("gate.result seq 3 zz-gate pass `file:runs/gate-zz.jsonl`"),
            "{md}"
        );
        assert!(
            md.contains("validation.finding seq 4 v-1 fail `a-1`"),
            "{md}"
        );
        // The orphan row says why it is not applicable.
        assert!(
            md.contains("not-applicable (cited against digest sha256:"),
            "{md}"
        );
    }

    // ---- the waiver join (KRZ-344, D-I) -----------------------------------

    /// The waiver fixture: a pin whose enforced ZZ-FAIL-001 (r2) declares
    /// `waivable: true`, cited by one finding at seq 4 (run v-1, subject
    /// a-1); ZZ-OTHER-001 passes via a gate verdict, so every test can
    /// prove the waiver grants no authority beyond its one finding.
    fn waiver_matrix_events() -> Vec<Event> {
        let mut fail = pinned_rule("ZZ-FAIL-001", 2, "enforced", "must");
        fail.waivable = true;
        vec![
            ev(
                1,
                EventKind::PlanApproved {
                    plan: plan_with_pin(vec![
                        fail,
                        pinned_rule("ZZ-OTHER-001", 1, "enforced", "must"),
                    ]),
                    base_sha: Some("deadbeef".to_string()),
                },
            ),
            ev(
                2,
                gate_result(
                    "zz-gate",
                    GateVerdict::Pass,
                    "file:runs/gate-zz.jsonl",
                    vec!["ZZ-OTHER-001".to_string()],
                ),
            ),
            ev(
                4,
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: "v-1".to_string(),
                    finding: waiver_finding("a-1"),
                },
            ),
        ]
    }

    /// The fixture finding the waiver binds (kept in one place so the
    /// fingerprint the test computes is byte-identical to the fold's).
    fn waiver_finding(subject: &str) -> crate::types::Finding {
        finding_with_rule(
            subject,
            Some(citation("ZZ-FAIL-001", 2, &"ab".repeat(32), "enforced")),
        )
    }

    fn waiver_fingerprint(subject: &str) -> String {
        crate::standards_waiver::finding_fingerprint("v-1", &waiver_finding(subject))
    }

    /// A fully valid waiver over the fixture finding; expiry one hour past
    /// the fixture instant, so the fixed-ts log frontier sees it live.
    fn valid_waiver(fingerprint: &str) -> EventKind {
        EventKind::StandardsWaiverApproved {
            rule_id: "ZZ-FAIL-001".to_string(),
            rule_revision: 2,
            manifest_digest: "ab".repeat(32),
            approval_seq: 1,
            finding_fingerprint: fingerprint.to_string(),
            paths: vec!["crates/engine/src/x.rs".to_string()],
            diff_digest: "cd".repeat(32),
            reason: "upstream false positive, tracked as zz-123".to_string(),
            approver: "local-operator".to_string(),
            surface: "cli".to_string(),
            expires_at: ts() + chrono::Duration::hours(1),
        }
    }

    /// `valid_waiver` with one field mutated — each invalidation clause of
    /// the D-I binding gets its own exact probe.
    fn mutated_waiver(fingerprint: &str, mutate: impl FnOnce(&mut EventKind)) -> EventKind {
        let mut kind = valid_waiver(fingerprint);
        mutate(&mut kind);
        kind
    }

    fn fail_row(coverage: &StandardsCoverage) -> &RuleCoverage {
        coverage
            .rules
            .iter()
            .find(|row| row.id == "ZZ-FAIL-001")
            .expect("the fixture row")
    }

    /// A valid, unexpired, exactly-matching waiver renders `waived`, and
    /// the row names the exception through the structured event — the seq
    /// anchor, the approver and surface, the expiry, and the reason — in
    /// the ONE markdown renderer report.md, provenance replay, and the
    /// evidence bundle share (D-H/D-I; the ticket's "replay/report/
    /// evidence name the waiver" hint).
    #[test]
    fn flight_rules_waiver_valid_waiver_renders_waived_and_names_the_exception() {
        let mut events = waiver_matrix_events();
        events.push(ev(7, valid_waiver(&waiver_fingerprint("a-1"))));
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");

        let row = fail_row(&coverage);
        assert_eq!(row.disposition, RuleDisposition::Waived);
        assert_eq!(row.evidence.len(), 1);
        assert_eq!(row.evidence[0].bearing, "waived");
        let join = row.evidence[0].waiver.as_ref().expect("the waiver joins");
        assert_eq!(join.seq, 7);
        assert_eq!(join.approver, "local-operator");
        assert_eq!(join.surface, "cli");
        assert_eq!(join.reason, "upstream false positive, tracked as zz-123");
        assert_eq!(join.expires_at, ts() + chrono::Duration::hours(1));

        // Unrelated rows receive no authority: the passing rule is
        // untouched by a waiver that never named it.
        let other = coverage
            .rules
            .iter()
            .find(|row| row.id == "ZZ-OTHER-001")
            .expect("the passing row");
        assert_eq!(other.disposition, RuleDisposition::Passed);

        let md = render_coverage_markdown(&coverage);
        assert!(
            md.contains("| ZZ-FAIL-001 | r2 | enforced | must | gate:zz-gate | waived |"),
            "{md}"
        );
        assert!(
            md.contains(
                "validation.finding seq 4 v-1 waived `a-1` (waiver seq 7 by local-operator \
                 via cli, expires "
            ),
            "{md}"
        );
        assert!(
            md.contains("upstream false positive, tracked as zz-123"),
            "{md}"
        );
        // The machine form names it too (camelCase, additive). The
        // fixture pin's first rule is ZZ-FAIL-001.
        let json = serde_json::to_value(&coverage).unwrap();
        let waiver = &json["rules"][0]["evidence"][0]["waiver"];
        assert_eq!(waiver["seq"], 7);
        assert_eq!(waiver["approver"], "local-operator");
        assert_eq!(waiver["surface"], "cli");
        assert!(waiver["expiresAt"].is_string());
    }

    #[test]
    fn flight_rules_enforcement_exact_waiver_covers_companion_gate_failure() {
        let mut events = waiver_matrix_events();
        events.push(ev(
            6,
            gate_result(
                "zz-gate",
                GateVerdict::Fail,
                "inline:failed",
                vec!["ZZ-FAIL-001".to_string()],
            ),
        ));
        events.push(ev(7, valid_waiver(&waiver_fingerprint("a-1"))));
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");
        let row = fail_row(&coverage);
        assert_eq!(row.disposition, RuleDisposition::Waived);
        assert_eq!(row.evidence.len(), 2);
        assert!(row.evidence.iter().any(|entry| entry.bearing == "waived"));
        assert!(row
            .evidence
            .iter()
            .any(|entry| entry.event == "gate.result"));
    }

    /// One waiver subtracts EXACTLY ONE matching failure (D-I): a second
    /// failing join on the same rule — a distinct finding, or even an
    /// identical duplicate one waiver could pattern-match twice — keeps
    /// the row failed, because the fold consumes each waiver once.
    #[test]
    fn flight_rules_waiver_subtracts_exactly_one_failure() {
        // Distinct second finding (subject a-9, same rule/revision/digest).
        let mut events = waiver_matrix_events();
        events.push(ev(
            5,
            EventKind::ValidationFinding {
                milestone_id: "ms-1".to_string(),
                run_id: "v-1".to_string(),
                finding: waiver_finding("a-9"),
            },
        ));
        events.push(ev(7, valid_waiver(&waiver_fingerprint("a-1"))));
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");
        let row = fail_row(&coverage);
        assert_eq!(row.disposition, RuleDisposition::Failed);
        assert_eq!(row.evidence.len(), 2);
        assert_eq!(row.evidence[0].bearing, "waived");
        assert_eq!(row.evidence[1].bearing, "fail");
        assert!(row.evidence[1].waiver.is_none());

        // Identical duplicate (same run, same content, later seq): the
        // fingerprint matches both, but the consumed waiver cannot cover
        // the second occurrence.
        let mut events = waiver_matrix_events();
        events.push(ev(
            5,
            EventKind::ValidationFinding {
                milestone_id: "ms-1".to_string(),
                run_id: "v-1".to_string(),
                finding: waiver_finding("a-1"),
            },
        ));
        events.push(ev(7, valid_waiver(&waiver_fingerprint("a-1"))));
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");
        let row = fail_row(&coverage);
        assert_eq!(row.disposition, RuleDisposition::Failed);
        assert_eq!(row.evidence[0].bearing, "waived");
        assert_eq!(row.evidence[1].bearing, "fail");
    }

    /// Expiry restores the block: the fold judges the waiver against the
    /// log's own frontier, so an event appended after the expiry instant
    /// flips the row back to failed — deterministically, with no wall
    /// clock. (An enforcement decision re-judges expiry against its own
    /// clock; this fold is the audit.)
    #[test]
    fn flight_rules_waiver_expiry_restores_the_block() {
        let mut events = waiver_matrix_events();
        events.push(ev(7, valid_waiver(&waiver_fingerprint("a-1"))));
        let live = standards_coverage("m-1", &events).expect("a pin folds");
        assert_eq!(fail_row(&live).disposition, RuleDisposition::Waived);

        let mut later = ev(8, EventKind::MissionPaused {});
        later.ts = ts() + chrono::Duration::hours(2);
        events.push(later);
        let expired = standards_coverage("m-1", &events).expect("a pin folds");
        let row = fail_row(&expired);
        assert_eq!(row.disposition, RuleDisposition::Failed);
        assert_eq!(row.evidence[0].bearing, "fail");
        assert!(row.evidence[0].waiver.is_none());
    }

    /// A revision bump, a substituted manifest, or a re-approval
    /// invalidates the waiver: the recorded revision / manifest digest /
    /// approval seq must match the standing pin exactly, or the join
    /// simply does not happen.
    #[test]
    fn flight_rules_waiver_mismatched_revision_digest_or_pin_joins_nothing() {
        let fingerprint = waiver_fingerprint("a-1");
        let probes = [
            mutated_waiver(&fingerprint, |kind| {
                if let EventKind::StandardsWaiverApproved { rule_revision, .. } = kind {
                    *rule_revision = 3;
                }
            }),
            mutated_waiver(&fingerprint, |kind| {
                if let EventKind::StandardsWaiverApproved {
                    manifest_digest, ..
                } = kind
                {
                    *manifest_digest = "ff".repeat(32);
                }
            }),
            mutated_waiver(&fingerprint, |kind| {
                if let EventKind::StandardsWaiverApproved { approval_seq, .. } = kind {
                    *approval_seq = 99;
                }
            }),
        ];
        for (idx, probe) in probes.into_iter().enumerate() {
            let mut events = waiver_matrix_events();
            events.push(ev(7, probe));
            let coverage = standards_coverage("m-1", &events).expect("a pin folds");
            let row = fail_row(&coverage);
            assert_eq!(
                row.disposition,
                RuleDisposition::Failed,
                "probe {idx} must not join"
            );
            assert!(row.evidence[0].waiver.is_none(), "probe {idx}");
        }
    }

    /// A finding-fingerprint change invalidates the waiver: a waiver bound
    /// to one finding covers no other — the block stays.
    #[test]
    fn flight_rules_waiver_fingerprint_mismatch_restores_the_block() {
        let mut events = waiver_matrix_events();
        // Bound to a DIFFERENT finding's fingerprint (subject a-9, which
        // no recorded failure carries).
        events.push(ev(7, valid_waiver(&waiver_fingerprint("a-9"))));
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");
        let row = fail_row(&coverage);
        assert_eq!(row.disposition, RuleDisposition::Failed);
        assert!(row.evidence[0].waiver.is_none());
    }

    /// The unauthorized-actor clause, consumption-side (D-I): a model may
    /// request a waiver but can never approve one, so an event claiming a
    /// model/orchestrator/worker surface — or no accountable approver at
    /// all — carries no authority and the block stands. No engine code
    /// path emits this event; this is the second fence against a hand-cut
    /// log laundering model discretion into human approval.
    #[test]
    fn flight_rules_waiver_model_or_anonymous_surface_carries_no_authority() {
        let fingerprint = waiver_fingerprint("a-1");
        let probes = [
            mutated_waiver(&fingerprint, |kind| {
                if let EventKind::StandardsWaiverApproved { surface, .. } = kind {
                    *surface = "model".to_string();
                }
            }),
            mutated_waiver(&fingerprint, |kind| {
                if let EventKind::StandardsWaiverApproved { surface, .. } = kind {
                    *surface = "orchestrator".to_string();
                }
            }),
            mutated_waiver(&fingerprint, |kind| {
                if let EventKind::StandardsWaiverApproved { approver, .. } = kind {
                    *approver = "  ".to_string();
                }
            }),
        ];
        for (idx, probe) in probes.into_iter().enumerate() {
            let mut events = waiver_matrix_events();
            events.push(ev(7, probe));
            let coverage = standards_coverage("m-1", &events).expect("a pin folds");
            assert_eq!(
                fail_row(&coverage).disposition,
                RuleDisposition::Failed,
                "probe {idx} must carry no authority"
            );
        }
    }

    /// A `waivable: false` rule can never be excepted: the record path
    /// refuses to write the event, and a hand-cut event joins nothing —
    /// the fold re-checks the pinned waiver posture rather than trusting
    /// the log (fail closed).
    #[test]
    fn flight_rules_waiver_non_waivable_rule_never_joins() {
        // full_matrix_events pins ZZ-FAIL-001 r2 with waivable: false,
        // cited by the seq-4 finding.
        let mut events = full_matrix_events();
        events.push(ev(7, valid_waiver(&waiver_fingerprint("a-1"))));
        let coverage = standards_coverage("m-1", &events).expect("a pin folds");
        let row = fail_row(&coverage);
        assert_eq!(row.disposition, RuleDisposition::Failed);
        assert!(row.evidence[0].waiver.is_none());
    }

    /// The event is additive (AGENTS.md contract rule): the wire name and
    /// camelCase payload round-trip, an empty path set omits the key, and
    /// a log without waiver events folds byte-identically to before — no
    /// `waiver` key materializes anywhere in the machine form.
    #[test]
    fn flight_rules_waiver_event_is_additive_and_old_logs_fold_unchanged() {
        let kind = valid_waiver(&"ef".repeat(32));
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "standards.waiver.approved");
        assert_eq!(json["payload"]["ruleId"], "ZZ-FAIL-001");
        assert_eq!(json["payload"]["ruleRevision"], 2);
        assert_eq!(json["payload"]["manifestDigest"], "ab".repeat(32));
        assert_eq!(json["payload"]["approvalSeq"], 1);
        assert_eq!(json["payload"]["findingFingerprint"], "ef".repeat(32));
        assert_eq!(
            json["payload"]["paths"],
            serde_json::json!(["crates/engine/src/x.rs"])
        );
        assert_eq!(json["payload"]["diffDigest"], "cd".repeat(32));
        assert_eq!(
            json["payload"]["reason"],
            "upstream false positive, tracked as zz-123"
        );
        assert_eq!(json["payload"]["approver"], "local-operator");
        assert_eq!(json["payload"]["surface"], "cli");
        assert!(json["payload"]["expiresAt"].is_string());
        let back: EventKind = serde_json::from_value(json).unwrap();
        assert!(matches!(back, EventKind::StandardsWaiverApproved { .. }));
        assert_eq!(back.type_name(), "standards.waiver.approved");

        let sparse = mutated_waiver(&"ef".repeat(32), |kind| {
            if let EventKind::StandardsWaiverApproved { paths, .. } = kind {
                paths.clear();
            }
        });
        let json = serde_json::to_value(&sparse).unwrap();
        assert!(
            json["payload"].get("paths").is_none(),
            "an empty path set carries no key: {json}"
        );

        // Pre-KRZ-344 logs: no waiver events, so no waiver key anywhere.
        let coverage = standards_coverage("m-1", &full_matrix_events()).expect("a pin folds");
        let json = serde_json::to_string(&coverage).unwrap();
        assert!(
            !json.contains("\"waiver\""),
            "old logs fold byte-identically: {json}"
        );
    }
}
