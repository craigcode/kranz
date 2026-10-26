//! Undrafted signal proposal tickets behind explicit human consent, plus
//! defect linkage confirmation (`docs/scoping/signal-intake.md` invariants 1–2,
//! §D-F; tickets `signal-intake-5-proposal`, `signal-intake-8-linkage`).

use crate::error::{EngineError, Result};
use crate::paths::MissionPaths;
use crate::signals::ledger::{DecisionOutcome, SignalLedger, SignalRecordKind};
use crate::signals::source::{
    DropReason, NormalisedSignal, ReplayCache, SignalSeverity, SignalSource,
};
use crate::signals::triage::{triage_signal, SignalsConfig, TriageDecision};
use crate::ticket::{Ticket, TicketState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Outcome of creating or updating an undrafted signal proposal ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalResult {
    pub signal_id: String,
    pub ticket_slug: String,
    pub created: bool,
    pub dedupe_count: u64,
    pub candidate_missions: Vec<String>,
}

/// Outcome of an explicit human consent action on a signal proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentResult {
    pub signal_id: String,
    pub ticket_slug: String,
    pub actor: String,
    pub confirmed_traced_mission: Option<String>,
}

/// Complete deterministic result of [`ingest_verified_signal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IngestResult {
    Dropped {
        reason: DropReason,
    },
    Parked {
        signal_id: String,
        rule: String,
        dedupe_count: u64,
    },
    NotACodeChange {
        signal_id: String,
        rule: String,
        dedupe_count: u64,
    },
    Proposed(ProposalResult),
}

/// Deterministic base slug for a signal proposal (`signal-<source>-<fingerprint>`).
pub fn base_proposal_slug(source: &str, fingerprint: &str) -> String {
    let clean_source: String = source
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    let clean_fp: String = fingerprint
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let collapsed = clean_fp
        .split('-')
        .filter(|seg| !seg.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let suffix = if collapsed.is_empty() {
        "item".to_string()
    } else if collapsed.len() > 40 {
        collapsed[..40].trim_end_matches('-').to_string()
    } else {
        collapsed
    };
    format!("signal-{clean_source}-{suffix}")
}

/// Create or update an undrafted proposal ticket for a `Proposed` triage decision.
///
/// - If an open, unconsented proposal ticket for the same `(source, fingerprint)`
///   already exists in `TicketState::New`, updates its `Occurrences: <dedupe_count>`
///   line and records `Proposed` in the ledger without creating a second ticket.
/// - If the base slug is already taken by a consented, closed, or different-fingerprint
///   ticket, appends a numeric sequence suffix (`-2`, `-3`, …).
pub fn create_or_update_proposal(
    repo_root: &Path,
    signal: &NormalisedSignal,
    triage: &TriageDecision,
) -> Result<ProposalResult> {
    if triage.outcome != DecisionOutcome::Proposed {
        return Err(EngineError::InvalidState(format!(
            "cannot create proposal for non-proposed triage outcome '{}'",
            triage.outcome.as_str()
        )));
    }

    let base_slug = base_proposal_slug(&signal.source, &signal.fingerprint);
    let expected_fp_line = format!("Fingerprint: {}", signal.fingerprint);
    let expected_src_line = format!("Source: {}", signal.source);

    // Check if an open, unconsented proposal for the exact same (source, fingerprint)
    // already exists.
    let mut chosen_slug = base_slug.clone();
    let mut seq = 1u32;
    loop {
        let md_path = Ticket::md_path(repo_root, &chosen_slug);
        if !md_path.exists() {
            break;
        }
        let existing = Ticket::load(&md_path)?;
        let state = Ticket::read_state(repo_root, &chosen_slug);
        let same_signal_identity = existing.context.contains(&expected_fp_line)
            && existing.context.contains(&expected_src_line);
        if same_signal_identity
            && existing.is_unconsented_signal_proposal()
            && state == TicketState::New
        {
            update_proposal_occurrences(&md_path, triage.dedupe_count)?;
            SignalLedger::append(
                repo_root,
                SignalRecordKind::Proposed {
                    signal_id: triage.signal_id.clone(),
                    ticket_slug: chosen_slug.clone(),
                    fingerprint: signal.fingerprint.clone(),
                    dedupe_count: triage.dedupe_count,
                },
            )?;
            return Ok(ProposalResult {
                signal_id: triage.signal_id.clone(),
                ticket_slug: chosen_slug,
                created: false,
                dedupe_count: triage.dedupe_count,
                candidate_missions: triage.candidate_missions.clone(),
            });
        }
        seq += 1;
        chosen_slug = format!("{base_slug}-{seq}");
    }

    let body = render_proposal_markdown(
        &triage.signal_id,
        signal,
        triage.dedupe_count,
        &triage.candidate_missions,
    );
    Ticket::create_markdown(repo_root, &chosen_slug, &body)?;

    SignalLedger::append(
        repo_root,
        SignalRecordKind::Proposed {
            signal_id: triage.signal_id.clone(),
            ticket_slug: chosen_slug.clone(),
            fingerprint: signal.fingerprint.clone(),
            dedupe_count: triage.dedupe_count,
        },
    )?;

    Ok(ProposalResult {
        signal_id: triage.signal_id.clone(),
        ticket_slug: chosen_slug,
        created: true,
        dedupe_count: triage.dedupe_count,
        candidate_missions: triage.candidate_missions.clone(),
    })
}

fn update_proposal_occurrences(md_path: &Path, dedupe_count: u64) -> Result<()> {
    let text = std::fs::read_to_string(md_path)?;
    let mut updated = String::with_capacity(text.len() + 16);
    for (idx, line) in text.split('\n').enumerate() {
        if idx > 0 {
            updated.push('\n');
        }
        if line.starts_with("Occurrences: ") {
            updated.push_str(&format!("Occurrences: {dedupe_count}"));
        } else {
            updated.push_str(line);
        }
    }
    std::fs::write(md_path, updated.as_bytes())?;
    Ok(())
}

/// Compose the proposal ticket markdown strictly from validated normalised
/// fields (`source`, `fingerprint`, `severity`, `project`, `environment`,
/// `evidence_links`, `candidate_missions`), keeping all free text and demoted
/// fields inside the nonced untrusted fence.
pub fn render_proposal_markdown(
    signal_id: &str,
    signal: &NormalisedSignal,
    dedupe_count: u64,
    candidate_missions: &[String],
) -> String {
    let priority = match signal.severity {
        SignalSeverity::Critical | SignalSeverity::High => 1,
        SignalSeverity::Medium => 2,
        SignalSeverity::Low | SignalSeverity::Info => 3,
    };
    let title = format!(
        "Signal [{}] {} ({}) in {}",
        signal.source,
        signal.fingerprint,
        signal.severity.as_str(),
        signal.project
    );
    let env_display = signal.environment.as_deref().unwrap_or("unspecified");
    let mut goal = format!(
        "Investigate and resolve the {} severity signal `{}` reported by `{}` for project `{}` (environment: `{}`).",
        signal.severity.as_str(),
        signal.fingerprint,
        signal.source,
        signal.project,
        env_display,
    );
    if !signal.evidence_links.is_empty() {
        goal.push_str("\n\nVerified evidence links:\n");
        for link in &signal.evidence_links {
            goal.push_str("- ");
            goal.push_str(link);
            goal.push('\n');
        }
        goal = goal.trim_end().to_string();
    }

    let candidates_frontmatter = if candidate_missions.is_empty() {
        String::new()
    } else {
        format!("signal-candidates: [{}]\n", candidate_missions.join(", "))
    };
    let candidates_context = if candidate_missions.is_empty() {
        "none".to_string()
    } else {
        format!(
            "{} (unconfirmed — confirm at draft time with `--traced-from <mission-id>`)",
            candidate_missions.join(", ")
        )
    };
    let external_id_line = signal
        .external_id
        .as_deref()
        .map(|id| format!("External ID: {id}\n"))
        .unwrap_or_default();

    let fenced_evidence = signal.fenced_evidence_block();

    format!(
        "---\n\
         state: open\n\
         title: {title}\n\
         priority: {priority}\n\
         schedule: once\n\
         signal: {signal_id}\n\
         signal-proposal: unconsented\n\
         {candidates_frontmatter}---\n\
         \n\
         ## Goal\n\
         {goal}\n\
         \n\
         ## Context\n\
         Signal ID: {signal_id}\n\
         Source: {source}\n\
         Fingerprint: {fingerprint}\n\
         Project: {project}\n\
         Severity: {severity}\n\
         Environment: {env_display}\n\
         {external_id_line}Occurrences: {dedupe_count}\n\
         Suggested traced-from-mission candidates: {candidates_context}\n\
         \n\
         {fenced_evidence}\n\
         \n\
         ## Scoping answers\n\
         - Proposal created by deterministic signal triage; requires explicit human consent before drafting.\n\
         \n\
         ## Acceptance hints\n\
         - Reproduce the reported issue locally and add a regression test.\n\
         - Keep the full workspace gate suite green.\n",
        source = signal.source,
        fingerprint = signal.fingerprint,
        project = signal.project,
        severity = signal.severity.as_str(),
    )
}

/// Record explicit human consent for a signal proposal (`kranz signal consent`
/// or `kranz draft --consent-signal` / authenticated operator REST action),
/// optionally confirming a `traced-from-mission` defect link (`signal-intake-8-linkage`).
pub fn consent_signal_proposal(
    repo_root: &Path,
    slug_or_signal_id: &str,
    actor: &str,
    confirmed_traced_mission: Option<&str>,
    mission_id: Option<&str>,
) -> Result<ConsentResult> {
    let actor = actor.trim();
    if actor.is_empty() {
        return Err(EngineError::Config(
            "signal consent requires a non-empty human actor".into(),
        ));
    }

    let ticket = resolve_signal_ticket(repo_root, slug_or_signal_id)?;
    let signal_id = ticket
        .signal
        .clone()
        .unwrap_or_else(|| format!("sig-{}", ticket.slug));

    let mut confirmed_link = None;
    if let Some(traced) = confirmed_traced_mission
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !MissionPaths::is_safe_id(traced) {
            return Err(EngineError::Config(format!(
                "invalid traced-from-mission id '{traced}'"
            )));
        }
        Ticket::seed_traced_from_mission(repo_root, &ticket.slug, traced)?;
        SignalLedger::append(
            repo_root,
            SignalRecordKind::Linked {
                signal_id: signal_id.clone(),
                ticket_slug: ticket.slug.clone(),
                traced_from_mission: traced.to_string(),
                actor: actor.to_string(),
            },
        )?;
        confirmed_link = Some(traced.to_string());
    }

    Ticket::mark_signal_consented(repo_root, &ticket.slug)?;
    SignalLedger::append(
        repo_root,
        SignalRecordKind::Consented {
            signal_id: signal_id.clone(),
            ticket_slug: ticket.slug.clone(),
            actor: actor.to_string(),
            mission_id: mission_id.map(str::to_string),
        },
    )?;

    Ok(ConsentResult {
        signal_id,
        ticket_slug: ticket.slug,
        actor: actor.to_string(),
        confirmed_traced_mission: confirmed_link,
    })
}

fn resolve_signal_ticket(repo_root: &Path, slug_or_signal_id: &str) -> Result<Ticket> {
    if Ticket::valid_slug(slug_or_signal_id) {
        let direct = Ticket::md_path(repo_root, slug_or_signal_id);
        if direct.exists() {
            let t = Ticket::load(&direct)?;
            if t.signal.is_some() || t.signal_proposal.is_some() {
                return Ok(t);
            }
            return Err(EngineError::InvalidState(format!(
                "ticket '{}' is not a signal proposal",
                t.slug
            )));
        }
    }
    for t in Ticket::list(repo_root) {
        if t.signal.as_deref() == Some(slug_or_signal_id) {
            return Ok(t);
        }
    }
    Err(EngineError::InvalidState(format!(
        "no signal proposal ticket found matching '{slug_or_signal_id}'"
    )))
}

/// Full deterministic intake pipeline for a monitoring [`SignalSource`]:
/// 1. Verify signature, size cap, timestamp skew, and replay cache (or record `Dropped`).
/// 2. Persist scrubbed raw payload and `Received` ledger record.
/// 3. Normalise and validate fields against strict allowlists.
/// 4. Triage deterministically (`Proposed`, `Parked`, or `NotACodeChange`).
/// 5. If `Proposed`, write/update an undrafted proposal ticket with
///    `signal-proposal: unconsented`. Never drafts, queues, or spawns a process.
pub fn ingest_verified_signal<S: SignalSource>(
    repo_root: &Path,
    source: &S,
    headers: &BTreeMap<String, String>,
    body: &[u8],
    config: &SignalsConfig,
    now: DateTime<Utc>,
    replay_cache: &mut ReplayCache,
) -> Result<IngestResult> {
    let verified = match source.verify(headers, body, now, replay_cache) {
        Ok(v) => v,
        Err(reason) => {
            let _ =
                SignalLedger::record_dropped(repo_root, source.source_id(), reason.as_str(), None);
            return Ok(IngestResult::Dropped { reason });
        }
    };

    let normalised = match source.normalise(headers, body) {
        Ok(n) => n,
        Err(_) => {
            let _ = SignalLedger::record_dropped(
                repo_root,
                source.source_id(),
                DropReason::MalformedPayload.as_str(),
                Some(&verified.delivery_id),
            );
            return Ok(IngestResult::Dropped {
                reason: DropReason::MalformedPayload,
            });
        }
    };

    let (received_entry, _raw_sha) = SignalLedger::record_received(
        repo_root,
        source.source_id(),
        Some(&verified.delivery_id),
        body,
        None,
    )?;
    let signal_id = match &received_entry.record {
        SignalRecordKind::Received { signal_id, .. } => signal_id.clone(),
        _ => unreachable!(),
    };

    let decision = triage_signal(repo_root, &signal_id, &normalised, config, now)?;
    match decision.outcome {
        DecisionOutcome::Parked => Ok(IngestResult::Parked {
            signal_id,
            rule: decision.rule,
            dedupe_count: decision.dedupe_count,
        }),
        DecisionOutcome::NotACodeChange => Ok(IngestResult::NotACodeChange {
            signal_id,
            rule: decision.rule,
            dedupe_count: decision.dedupe_count,
        }),
        DecisionOutcome::Proposed => {
            let proposal = create_or_update_proposal(repo_root, &normalised, &decision)?;
            Ok(IngestResult::Proposed(proposal))
        }
    }
}
