//! Gated merge orchestration (roadmap M6): a human-triggered Merge action
//! that refuses on a dirty tracked tree, runs the repo's tracked gate suite,
//! and advances the base to an exact, gate-tested integration commit only on
//! green — never pushing.
//!
//! `merge_mission` ties together three primitives that each already carry
//! their own safety contract: [`GitRepo::is_clean_tracked`] (refuse dirty),
//! [`crate::merge_gate::MergeSuiteGate`] (refuse on a failing gate, base
//! untouched — the suite runs through the [`crate::gate`] interface), and
//! [`GitRepo::merge_no_ff`] (clean-abort on conflict). The
//! merge and gates run in a detached scratch worktree; the primary base only
//! fast-forwards to that exact tested commit. Every git command on this path
//! runs via [`GitRepo::with_hooks_disabled`], so mission-planted
//! `.git/hooks/*` never execute with the server's environment. This module
//! never calls [`GitRepo::push_mission_branch`] or any other push.

use crate::error::Result;
use crate::gate::{Gate, GateVerdict};
use crate::git_ops::{with_kranz_trailers, GitRepo, KranzCommitMetadata, MergeOutcome};
use crate::merge_gate::{parse_gate_suite, MergeSuiteGate, MERGE_GATES_PATH};
use crate::scrub::{self, SecretFinding};
use std::path::Path;

/// Number of merge commits on the live base after the mission's pinned base
/// before the merge response flags likely sibling-merge semantic drift.
pub const STALE_BASE_MERGE_THRESHOLD: usize = 1;

/// Outcome of a gated merge attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeReport {
    /// The tracked working tree was dirty; no gates ran and base is untouched.
    RefusedDirtyTree,
    /// A gate failed; later gates and the merge itself never ran. Base is
    /// untouched.
    GateFailed {
        /// The failing gate's command string.
        gate: String,
        /// That gate's verbatim captured output.
        output: String,
    },
    /// The live base branch has no valid tracked merge-gate suite. Nothing
    /// ran and base is untouched; this fails closed instead of silently
    /// treating an empty suite as green.
    GateConfigInvalid { detail: String },
    /// The mission branch diff contains an unwaived secret finding. Base is
    /// untouched and no other gates ran.
    SecretScanFailed { findings: Vec<SecretFinding> },
    /// The merge conflicted and was rolled back (working tree left clean).
    Conflict {
        /// Conflicting paths git named (best-effort; may be empty).
        files: Vec<String>,
    },
    /// Git refused the merge before it ever started (no `MERGE_HEAD`), e.g. a
    /// divergent untracked file at a path the merge would overwrite. Base is
    /// untouched and nothing was aborted (there was no merge in progress).
    RefusedPreMerge {
        /// Git's verbatim refusal text.
        detail: String,
    },
    /// The live base's applicable ENFORCED Flight Rules set differs from the
    /// mission's approved pin (KRZ-342, design D-E): policy moved under the
    /// mission. The merge is refused before the gate suite runs — neither
    /// grandfather-skipping current policy nor silently applying new policy
    /// to an old consent artifact — and the caller records
    /// `standards.drifted`. Base is untouched.
    StandardsDrifted {
        /// The digest pinned at approval.
        approved_digest: String,
        /// The digest resolved from the live base (`None`: the live base no
        /// longer yields a readable standards manifest at all).
        current_digest: Option<String>,
        /// Id-level change lines for the applicable enforced set.
        changed_rules: Vec<String>,
    },
    /// An applicable enforced MUST could not produce a current authoritative
    /// merge verdict. Deterministic checkers run against the exact scratch
    /// integration tree; contextual/manual checkers must carry positive or
    /// exactly-waived final evidence from the completed mission.
    StandardsFailed {
        rule_id: String,
        checker: String,
        output: String,
    },
    /// The mission branch merged cleanly into base with a `--no-ff` commit.
    Merged {
        /// The new merge commit sha, now the tip of `base_branch`.
        commit: String,
        /// Informational warning when the mission's pinned base trails merge
        /// commits already landed on the live base branch.
        stale_base: Option<StaleBaseWarning>,
    },
}

/// Non-blocking merge-time warning for missions drafted from a stale base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBaseWarning {
    pub base_sha: String,
    pub live_base: String,
    pub merge_commits_since_base: usize,
}

/// Positive final evidence that may be consumed by merge-only checker forms.
/// Deterministic gates are always re-run against the scratch integration;
/// only an exact human waiver may permit one of those current failures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StandardsMergeEvidence {
    pub passed: std::collections::BTreeSet<String>,
    waived_final: std::collections::BTreeSet<String>,
    approval_seq: Option<u64>,
    waivers: Vec<crate::standards_waiver::WaiverRecord>,
    attestations: Vec<crate::standards_attestation::AttestationRecord>,
    evaluated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl StandardsMergeEvidence {
    pub fn from_mission_events(
        mission_id: &str,
        pin: Option<&crate::types::StandardsPin>,
        coverage: Option<&crate::standards_coverage::StandardsCoverage>,
        events: &[crate::events::Event],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let mut evidence = Self::default();
        if let Some(coverage) = coverage {
            for rule in &coverage.rules {
                match rule.disposition {
                    crate::standards_coverage::RuleDisposition::Passed => {
                        evidence.passed.insert(rule.id.clone());
                    }
                    crate::standards_coverage::RuleDisposition::Waived => {
                        evidence.waived_final.insert(rule.id.clone());
                    }
                    _ => {}
                }
            }
        }
        if let Some(pin) = pin {
            evidence.approval_seq = events
                .iter()
                .filter(|event| event.mission_id == mission_id)
                .filter_map(|event| match &event.kind {
                    crate::events::EventKind::PlanApproved { plan, .. }
                        if plan.standards_manifest.as_deref() == Some(pin) =>
                    {
                        Some(event.seq)
                    }
                    _ => None,
                })
                .next_back();
            evidence.waivers = events
                .iter()
                .filter(|event| event.mission_id == mission_id)
                .filter_map(crate::standards_waiver::WaiverRecord::from_event)
                .collect();
            evidence.attestations = events
                .iter()
                .filter(|event| event.mission_id == mission_id)
                .filter_map(crate::standards_attestation::AttestationRecord::from_event)
                .collect();
            evidence.evaluated_at = Some(now);
        }
        evidence
    }

    #[allow(clippy::too_many_arguments)]
    fn current_waiver(
        &self,
        repo: &GitRepo,
        live_base_sha: &str,
        tested_commit: &str,
        integration_paths: &[String],
        pin: &crate::types::StandardsPin,
        rule: &crate::types::PinnedRule,
        finding: Option<&crate::types::Finding>,
    ) -> Result<bool> {
        if !self.waived_final.contains(&rule.id) {
            return Ok(false);
        }
        let (Some(approval_seq), Some(now)) = (self.approval_seq, self.evaluated_at) else {
            return Ok(false);
        };
        let paths = crate::standards_waiver::affected_paths_with_context(
            rule,
            integration_paths,
            &pin.context_paths,
        );
        let diff = if rule.when_paths.is_empty() {
            repo.diff_full(live_base_sha, tested_commit)?
        } else if paths.is_empty() {
            String::new()
        } else {
            repo.diff_range_paths(live_base_sha, tested_commit, &paths)?
        };
        let diff_digest = crate::standards_waiver::sha256_hex(diff.as_bytes());
        let fingerprint = finding.map(|finding| {
            crate::standards_waiver::finding_fingerprint(crate::reducer::ENGINE_RUN_ID, finding)
        });
        Ok(self.waivers.iter().any(|waiver| {
            fingerprint
                .as_ref()
                .is_none_or(|fingerprint| waiver.finding_fingerprint == *fingerprint)
                && crate::standards_waiver::waiver_covers(
                    waiver,
                    rule,
                    pin,
                    approval_seq,
                    &waiver.finding_fingerprint,
                    now,
                )
                && waiver.paths == paths
                && waiver.diff_digest == diff_digest
        }))
    }

    fn current_attestation(
        &self,
        repo: &GitRepo,
        live_base_sha: &str,
        tested_commit: &str,
        integration_paths: &[String],
        pin: &crate::types::StandardsPin,
        rule: &crate::types::PinnedRule,
    ) -> Result<bool> {
        let Some(approval_seq) = self.approval_seq else {
            return Ok(false);
        };
        let paths = crate::standards_waiver::affected_paths_with_context(
            rule,
            integration_paths,
            &pin.context_paths,
        );
        let diff = if rule.when_paths.is_empty() {
            repo.diff_full(live_base_sha, tested_commit)?
        } else if paths.is_empty() {
            String::new()
        } else {
            repo.diff_range_paths(live_base_sha, tested_commit, &paths)?
        };
        let diff_digest = crate::standards_waiver::sha256_hex(diff.as_bytes());
        Ok(self.attestations.iter().rev().any(|record| {
            record.seq > approval_seq
                && record.rule_id == rule.id
                && record.rule_revision == rule.revision
                && record.manifest_digest == pin.digest
                && record.approval_seq == approval_seq
                && record.paths == paths
                && record.diff_digest == diff_digest
                && crate::standards_waiver::HUMAN_SURFACES.contains(&record.surface.as_str())
                && !record.approver.trim().is_empty()
        }))
    }
}

/// Runs the gated merge: refuse-if-dirty, then gates, then `--no-ff` merge.
///
/// `executor` is forwarded to the [`crate::merge_gate::MergeSuiteGate`]
/// adapter as-is (production callers wrap the orchestrator's shell runner,
/// tests inject a scripted fake). This function never calls
/// [`GitRepo::push_mission_branch`] or any push — the base branch is only
/// ever advanced locally.
///
/// `standards_pin` is the mission's approved Flight Rules manifest pin
/// (KRZ-342, design D-E), folded from its event log: `None` keeps the merge
/// byte-identical. With a repo-tracked pin, the LIVE base policy is
/// re-resolved against the exact scratch integration diff once the
/// integration commit exists and BEFORE the gate suite runs — an applicable
/// enforced-set difference refuses with [`MergeReport::StandardsDrifted`].
pub fn merge_mission<F>(
    repo: &GitRepo,
    base_branch: &str,
    base_sha: &str,
    mission_branch: &str,
    metadata: Option<KranzCommitMetadata>,
    standards_pin: Option<&crate::types::StandardsPin>,
    executor: F,
) -> Result<MergeReport>
where
    F: Fn(&str, &Path) -> (bool, String),
{
    merge_mission_with_standards_evidence(
        repo,
        base_branch,
        base_sha,
        mission_branch,
        metadata,
        standards_pin,
        &StandardsMergeEvidence::default(),
        executor,
    )
}

/// The production merge path, including the completed mission's replayed
/// standards evidence. Kept separate from [`merge_mission`] so existing
/// embedders with no Flight Rules pin retain their source-compatible call.
#[allow(clippy::too_many_arguments)]
pub fn merge_mission_with_standards_evidence<F>(
    repo: &GitRepo,
    base_branch: &str,
    base_sha: &str,
    mission_branch: &str,
    metadata: Option<KranzCommitMetadata>,
    standards_pin: Option<&crate::types::StandardsPin>,
    standards_evidence: &StandardsMergeEvidence,
    executor: F,
) -> Result<MergeReport>
where
    F: Fn(&str, &Path) -> (bool, String),
{
    // Every git command this merge issues — primary tree and scratch worktree
    // alike — runs with hooks disabled: the scratch worktree shares the
    // primary `.git`, so mission-authored gate code could plant
    // `.git/hooks/*` and have the merge's own checkout/merge/worktree
    // commands execute it with the server's full environment (exactly what
    // the sanitized gate executor withholds). Worker-side git is untouched.
    let repo = &repo.with_hooks_disabled()?;

    if !repo.is_clean_tracked_strict()? {
        return Ok(MergeReport::RefusedDirtyTree);
    }
    let primary_head_before_gates = repo.head_sha()?;
    let primary_branch_before_gates = repo.current_branch()?;

    // Resolve moving refs once. Every read, merge, and final advance below
    // uses these SHAs so a late branch update cannot bypass validation.
    let live_base_sha = repo.rev_parse(base_branch)?;
    let mission_tip_sha = repo.rev_parse(mission_branch)?;

    let diff = repo.diff_full(base_sha, &mission_tip_sha)?;
    let allowlist = repo
        .show_file(&live_base_sha, scrub::SECRET_ALLOWLIST_PATH)?
        .map(|bytes| scrub::read_allowlist_text(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default();
    let findings = scrub::filter_allowed(scrub::scan_unified_diff(&diff), &allowlist);
    if !findings.is_empty() {
        return Ok(MergeReport::SecretScanFailed { findings });
    }

    let changed_paths = repo.changed_paths(base_sha, &mission_tip_sha)?;
    let gate_bytes = match repo.show_file(&live_base_sha, MERGE_GATES_PATH)? {
        Some(bytes) => bytes,
        None => {
            return Ok(MergeReport::GateConfigInvalid {
                detail: format!(
                    "live base branch {base_branch:?} has no tracked {MERGE_GATES_PATH}; add an explicit repo gate suite before merging"
                ),
            })
        }
    };
    let gate_suite = match parse_gate_suite(&gate_bytes) {
        Ok(suite) => suite,
        Err(detail) => return Ok(MergeReport::GateConfigInvalid { detail }),
    };

    let stale_base = stale_base_warning(repo, base_branch, base_sha)?;
    let merge_message = metadata
        .as_ref()
        .map(|metadata| with_kranz_trailers(&format!("Merge {mission_branch}"), metadata));

    // Sweep scratch leftovers from any earlier merge that died between add
    // and remove (a panic, kill -9, power loss): stale kranz-merge-* temp
    // dirs and their .git/worktrees registrations would otherwise pile up
    // forever. Best-effort — this merge proceeds either way.
    remove_stale_scratch_worktrees(repo);

    let scratch_path =
        std::env::temp_dir().join(format!("kranz-merge-{}", uuid::Uuid::new_v4().simple()));
    repo.add_detached_worktree(&scratch_path, &live_base_sha)?;
    // RAII cleanup, created immediately after the worktree so a panic or an
    // overlooked error path cannot leak it. Drop is best-effort (warn, never
    // propagate): a merge that already landed must be reported as Merged,
    // not turned into an error by a failed cleanup — the sweep above retries
    // the removal on the next merge anyway.
    let _scratch_cleanup = ScratchWorktree {
        repo,
        path: scratch_path.clone(),
    };

    let scratch = GitRepo::open(&scratch_path)?.with_hooks_disabled()?;
    match scratch.merge_no_ff_with_message(&mission_tip_sha, merge_message.as_deref())? {
        MergeOutcome::Conflict { files } => return Ok(MergeReport::Conflict { files }),
        MergeOutcome::RefusedPreMerge { detail } => {
            return Ok(MergeReport::RefusedPreMerge { detail })
        }
        MergeOutcome::Clean => {}
    }
    let tested_commit = scratch.head_sha()?;

    // Flight Rules policy-drift check (KRZ-342, design D-E): re-resolve the
    // LIVE base standards policy against the exact scratch integration diff
    // (live_base..tested_commit — what this merge would add to the base) and
    // compare the applicable ENFORCED set against the approved pin's. A
    // difference refuses BEFORE the gate suite: the mission needs explicit
    // revalidation/reapproval, not a green gate run under policy the
    // operator never consented to. `None` pin ⇒ skip, byte-identical.
    if let Some(pin) = standards_pin {
        let integration_paths = repo.changed_paths(&live_base_sha, &tested_commit)?;
        if let Some(drift) =
            crate::pack::resolution::merge_drift(repo, &live_base_sha, pin, &integration_paths)
                .map_err(crate::error::EngineError::Git)?
        {
            return Ok(MergeReport::StandardsDrifted {
                approved_digest: drift.approved_digest,
                current_digest: drift.current_digest,
                changed_rules: drift.changed_rules,
            });
        }

        // KRZ-346 (D-F): checker bindings and commands come only from the
        // approval pin. Deterministic merge rules execute once per stable
        // gate id against the exact scratch integration tree. Approved rules
        // and enforced SHOULDs still execute but remain advisory; only an
        // enforced MUST can refuse the merge.
        let rules = crate::standards_enforcement::applicable_rules(
            pin,
            &[crate::pack::standards::RuleStage::Merge],
            &integration_paths,
        );
        let mut gate_rules: std::collections::BTreeMap<
            String,
            (&crate::types::PinnedGate, Vec<&crate::types::PinnedRule>),
        > = std::collections::BTreeMap::new();
        for rule in &rules {
            match crate::standards_enforcement::checker_binding(pin, rule, &integration_paths) {
                crate::standards_enforcement::CheckerBinding::Gate(gate) => {
                    gate_rules
                        .entry(gate.id.clone())
                        .or_insert_with(|| (gate, Vec::new()))
                        .1
                        .push(rule);
                }
                crate::standards_enforcement::CheckerBinding::AgentJudgement => {
                    if crate::standards_enforcement::rule_mode(rule)
                        == crate::standards_enforcement::RuleMode::Authoritative
                        && !standards_evidence.passed.contains(&rule.id)
                        && !standards_evidence.current_waiver(
                            repo,
                            &live_base_sha,
                            &tested_commit,
                            &integration_paths,
                            pin,
                            rule,
                            None,
                        )?
                    {
                        return Ok(MergeReport::StandardsFailed {
                            rule_id: rule.id.clone(),
                            checker: rule.checker.clone().unwrap_or_default(),
                            output: "no positive or exact-waived final checker evidence is present for this completed mission".to_string(),
                        });
                    }
                }
                crate::standards_enforcement::CheckerBinding::ManualAttestation => {
                    if crate::standards_enforcement::rule_mode(rule)
                        == crate::standards_enforcement::RuleMode::Authoritative
                        && !standards_evidence.current_attestation(
                            repo,
                            &live_base_sha,
                            &tested_commit,
                            &integration_paths,
                            pin,
                            rule,
                        )?
                        && !standards_evidence.current_waiver(
                            repo,
                            &live_base_sha,
                            &tested_commit,
                            &integration_paths,
                            pin,
                            rule,
                            None,
                        )?
                    {
                        return Ok(MergeReport::StandardsFailed {
                            rule_id: rule.id.clone(),
                            checker: "manual-attestation".to_string(),
                            output: "no authorized attestation or exact waiver matches the scratch integration diff".to_string(),
                        });
                    }
                }
                crate::standards_enforcement::CheckerBinding::Unavailable(detail) => {
                    if crate::standards_enforcement::rule_mode(rule)
                        == crate::standards_enforcement::RuleMode::Authoritative
                    {
                        return Ok(MergeReport::StandardsFailed {
                            rule_id: rule.id.clone(),
                            checker: rule
                                .checker
                                .clone()
                                .unwrap_or_else(|| "<missing>".to_string()),
                            output: detail,
                        });
                    }
                }
            }
        }
        for (_gate_id, (gate, bound_rules)) in gate_rules {
            let (passed, output) = executor(&gate.command, scratch.root());
            if passed {
                continue;
            }
            for rule in bound_rules.into_iter().filter(|rule| {
                crate::standards_enforcement::rule_mode(rule)
                    == crate::standards_enforcement::RuleMode::Authoritative
            }) {
                let finding = crate::standards_enforcement::failure_finding(
                    pin,
                    rule,
                    &scrub::scrub(&format!(
                        "checker `{}` failed for {} r{}: {}",
                        gate.id, rule.id, rule.revision, output
                    )),
                );
                if !standards_evidence.current_waiver(
                    repo,
                    &live_base_sha,
                    &tested_commit,
                    &integration_paths,
                    pin,
                    rule,
                    Some(&finding),
                )? {
                    return Ok(MergeReport::StandardsFailed {
                        rule_id: rule.id.clone(),
                        checker: format!("gate:{}", gate.id),
                        output,
                    });
                }
            }
        }
    }

    // The suite runs through the first-class gate interface (gate.rs) —
    // behavior is unchanged: same commands, same declared order, stop at
    // first failure, suite bytes read from the live base branch above.
    let outcome =
        MergeSuiteGate::new(scratch.root(), &changed_paths, gate_suite, executor).evaluate();
    if outcome.verdict == GateVerdict::Fail {
        return Ok(MergeReport::GateFailed {
            gate: outcome.artefact.reference,
            output: outcome.artefact.detail.unwrap_or_default(),
        });
    }

    let post_gate_head = scratch.head_sha()?;
    let tracked_tree_clean = scratch.is_clean_tracked_strict()?;
    if post_gate_head != tested_commit || !tracked_tree_clean {
        return Ok(MergeReport::RefusedPreMerge {
            detail: format!(
                "merge gates mutated the integrated tree: expected HEAD {tested_commit}, \
                 found {post_gate_head}, tracked files clean={tracked_tree_clean}; \
                 refusing to land bytes other than the tested commit"
            ),
        });
    }

    // Production holds the repo-wide busy guard throughout this call.
    // Re-check anyway so external/manual movement fails closed.
    let current_base = repo.rev_parse(base_branch)?;
    if current_base != live_base_sha {
        return Ok(MergeReport::RefusedPreMerge {
            detail: format!(
                "base branch {base_branch:?} moved from {live_base_sha} to {current_base} while gates ran; retry the merge"
            ),
        });
    }

    let primary_head_after_gates = repo.head_sha()?;
    let primary_branch_after_gates = repo.current_branch()?;
    let primary_tree_clean = repo.is_clean_tracked_strict()?;
    if primary_head_after_gates != primary_head_before_gates
        || primary_branch_after_gates != primary_branch_before_gates
        || !primary_tree_clean
    {
        return Ok(MergeReport::RefusedPreMerge {
            detail: format!(
                "merge gates mutated the primary checkout: expected \
                 {primary_branch_before_gates}@{primary_head_before_gates}, found \
                 {primary_branch_after_gates}@{primary_head_after_gates}, tracked files \
                 clean={primary_tree_clean}; refusing to overwrite operator work"
            ),
        });
    }

    repo.checkout(base_branch)?;
    strip_identical_untracked_twins(repo, base_sha, &mission_tip_sha)?;
    match repo.fast_forward_to(&tested_commit)? {
        MergeOutcome::Clean => Ok(MergeReport::Merged {
            commit: tested_commit,
            stale_base,
        }),
        MergeOutcome::RefusedPreMerge { detail } => Ok(MergeReport::RefusedPreMerge { detail }),
        MergeOutcome::Conflict { .. } => unreachable!("--ff-only cannot create conflicts"),
    }
}

/// RAII cleanup for the merge's detached scratch worktree.
///
/// Removal lives in `Drop` so a panic mid-merge (or an error path this module
/// missed) cannot leak the temp directory and its
/// `.git/worktrees/kranz-merge-*` registration. Cleanup is best-effort: a
/// failure is logged at warn and never propagated, so a merge whose report is
/// already decided keeps that report — [`remove_stale_scratch_worktrees`]
/// retries the removal at the start of the next merge.
struct ScratchWorktree<'a> {
    repo: &'a GitRepo,
    path: std::path::PathBuf,
}

impl Drop for ScratchWorktree<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.repo.remove_worktree(&self.path) {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "failed to remove merge scratch worktree; the next merge will retry"
            );
        }
    }
}

/// Best-effort removal of `kranz-merge-*` scratch worktrees leaked by an
/// earlier merge that died between add and remove, plus a
/// `git worktree prune` for registrations whose directories are already gone
/// (invisible to `worktree remove`). Failures are logged and swallowed — a
/// stale leftover must never block a fresh merge.
fn remove_stale_scratch_worktrees(repo: &GitRepo) {
    match repo.list_worktrees() {
        Ok(worktrees) => {
            for worktree in worktrees {
                let path = Path::new(&worktree);
                let is_scratch = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("kranz-merge-"));
                if !is_scratch {
                    continue;
                }
                if let Err(error) = repo.remove_worktree(path) {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "failed to remove stale merge scratch worktree"
                    );
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to list worktrees for stale merge-scratch cleanup")
        }
    }
    if let Err(error) = repo.prune_worktrees() {
        tracing::warn!(%error, "failed to prune stale worktree registrations");
    }
}

fn stale_base_warning(
    repo: &GitRepo,
    base_branch: &str,
    base_sha: &str,
) -> Result<Option<StaleBaseWarning>> {
    let merge_commits_since_base = repo.merge_commit_count(base_sha, base_branch)?;
    if merge_commits_since_base < STALE_BASE_MERGE_THRESHOLD {
        return Ok(None);
    }
    Ok(Some(StaleBaseWarning {
        base_sha: base_sha.to_string(),
        live_base: base_branch.to_string(),
        merge_commits_since_base,
    }))
}

/// Removes untracked working-tree files the incoming merge would touch, but
/// ONLY when their bytes are byte-identical to the version already committed
/// on `mission_branch` — the operator-visibility "preview twins" written
/// straight into the primary tree at canonical mission paths (plan.md,
/// report.md, revised-plan.md) alongside the same paths tracked on the
/// mission branch. Removing an identical file is lossless (the merge would
/// write back the exact same bytes); a divergent or tracked file is left
/// alone so git's own conflict/refusal machinery handles it.
fn strip_identical_untracked_twins(
    repo: &GitRepo,
    base_sha: &str,
    mission_branch: &str,
) -> Result<()> {
    for path in repo.changed_paths(base_sha, mission_branch)? {
        if !repo.is_untracked(&path)? {
            continue;
        }
        let incoming = match repo.show_file(mission_branch, &path)? {
            Some(bytes) => bytes,
            None => continue,
        };
        let full_path = repo.root().join(&path);
        let current = match std::fs::read(&full_path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        if current == incoming {
            std::fs::remove_file(&full_path)?;
        }
    }
    Ok(())
}
