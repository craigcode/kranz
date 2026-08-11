//! Flight Rules deterministic resolution, approval pinning, and drift
//! refusal (ticket `.kranz/tickets/flight-rules-resolution-pin.md`, KRZ-342;
//! design `docs/scoping/flight-rules-engineering-standards.md`, decisions
//! D-D, D-E, and D-G over the KRZ-341 schema-4 corpus).
//!
//! WHY the engine, not a model, selects (D-D): applicability is a pure
//! function of declared rule metadata and three selection inputs — the
//! workflow stage, the mission task class, and the touch paths — so the same
//! inputs always select the same stable-sorted rules. Domains are
//! browsing/reporting labels and are NEVER a selection input. Lifecycle is
//! part of the predicate (D-B): `retired` rules never apply, `draft` rules
//! apply only on lint/authoring surfaces (this module serves mission
//! surfaces, so drafts never resolve here), and `approved`/`enforced` rules
//! apply.
//!
//! WHY two touch-path interpretations: approval resolves against the
//! APPROVED TOUCH SET — gitignore-style globs, not real paths — so a
//! declared glob and a rule's `when-paths` prefix are compared by OVERLAP
//! (either's literal extent sits at/below the other), a conservative
//! SUPERSET: any actual path admitted by the touch set that a rule scopes to
//! was already selected at approval. Final validation and merge resolve
//! against REAL changed paths with the exact prefix match
//! ([`crate::merge_gate::when_paths_match`]). The superset property is what
//! makes "newly applicable at validation" mean "the mission escaped its
//! approved envelope" rather than a matching artifact — an empty touch set
//! selects no path-scoped rule at approval, so an enforced path-scoped rule
//! the actual diff activates is correctly an escape.
//!
//! WHY the pin is engine-authored (D-E): the plan contract's
//! `standardsManifest` is a consent artifact. Approval reloads the TRUSTED
//! source — tracked base blobs for a repo-relative pack
//! ([`super::standards::load_at_ref`], the merge-gates ownership idiom: a
//! mission-branch edit is structurally invisible to it) or a single
//! capability read for an external pack — resolves, and writes the pin. A
//! plan CARRYING a manifest that differs from the fresh resolution is stale
//! or substituted and approval rejects it; a plan carrying none is pinned by
//! the engine (the planner never authors policy). Every later stage consumes
//! the pin, never a later filesystem or branch read: an external pack edit
//! after approval cannot change a run, and a mission that edits its own
//! repo-relative pack is judged by the OLD base version.
//!
//! WHY drift refusal at merge (D-E): the approved pin records what the
//! operator consented to. Merge re-resolves the LIVE base policy against the
//! exact scratch integration diff; if the applicable ENFORCED set differs
//! from the approved one — a rule added, removed, re-revised, re-scoped, or
//! re-bound — merge refuses with `standards.drifted` rather than
//! grandfather-skipping current policy or silently applying new policy to an
//! old consent artifact. The comparison re-resolves BOTH sides with the same
//! inputs (merge stage, the pin's task class, the integration paths), so an
//! unchanged base policy can never false-positive.

use super::standards::{
    load_at_ref, Checker, RfcStatus, RuleMeta, RuleStage, StandardsManifest, StandardsTrust,
};
use crate::git_ops::GitRepo;
use crate::types::{MissionConfig, PinnedGate, PinnedRule, StandardsPin, StandardsPinSource};
use std::path::Path;

/// The resolution surface recorded on `standards.resolved` for the
/// approval-time pinning resolution. Stage-specific projections (KRZ-345)
/// emit their own surfaces; this slice resolves the mission-wide set once,
/// at approval.
pub const APPROVAL_SURFACE: &str = "approval";

/// How the touch-paths selection input is interpreted (D-D).
pub enum TouchInput<'a> {
    /// The approved touch-set globs (approval): a rule's `when-paths` apply
    /// when any non-negated glob COULD admit a path at or below a declared
    /// prefix — literal-stem overlap, the conservative superset.
    Declared(&'a [String]),
    /// Real changed paths (final validation, merge): exact at/below-prefix
    /// matching, the merge-gate idiom.
    Actual(&'a [String]),
}

// ---------------------------------------------------------------------------
// The D-D predicate
// ---------------------------------------------------------------------------

/// The D-D applicability predicate over one rule's scope fields. A rule
/// applies when (1) `stage` appears in its `stages`, (2) its `task_classes`
/// is empty or contains the mission task class (routing normalization:
/// trimmed, case-insensitive — a classless mission matches no scoped rule),
/// and (3) its `when_paths` is empty or matches under `touch`'s
/// interpretation. Domains are never consulted.
fn rule_applies(
    rule: &RuleMeta,
    stage: RuleStage,
    task_class: Option<&str>,
    touch: &TouchInput,
) -> bool {
    if !rule.stages.contains(&stage) {
        return false;
    }
    if !rule.task_classes.is_empty() {
        let Some(task_class) = task_class else {
            return false;
        };
        let wanted = crate::routing::normalize_task_class(task_class);
        if !rule
            .task_classes
            .iter()
            .any(|class| crate::routing::normalize_task_class(class) == wanted)
        {
            return false;
        }
    }
    match touch {
        TouchInput::Actual(paths) => crate::merge_gate::when_paths_match(&rule.when_paths, paths),
        TouchInput::Declared(globs) => declared_touch_overlaps(&rule.when_paths, globs),
    }
}

/// The declared touch-set interpretation (approval): `when_paths` empty
/// matches everything; otherwise at least one non-negated glob's literal
/// extent must overlap a declared prefix. Overlap is symmetric at/below on
/// `/`-boundaries: `crates/**` overlaps `crates/engine` (the mission may
/// reach it), `crates/engine/types.rs` overlaps `crates`, and `docs/**`
/// overlaps neither. Negated (`!`) globs are ignored — they can only shrink
/// the real envelope, so ignoring them keeps the selection a superset.
fn declared_touch_overlaps(when_paths: &[String], globs: &[String]) -> bool {
    if when_paths.is_empty() {
        return true;
    }
    globs
        .iter()
        .filter(|glob| !glob.starts_with('!'))
        .map(|glob| glob_literal_stem(glob))
        .any(|stem| {
            when_paths
                .iter()
                .any(|prefix| paths_overlap(&stem, prefix.trim_end_matches('/')))
        })
}

/// The directory-bounded literal stem of a gitignore-style glob: everything
/// before the first glob metachar (`*`, `?`, `[`, `{`, or an escape `\`),
/// cut back to the last `/`. `crates/eng*/*.rs` stems to `crates`; `*.rs`
/// stems to `""` (the repo root — which overlaps every prefix, the safe
/// over-selection direction).
fn glob_literal_stem(glob: &str) -> String {
    let bytes = glob.as_bytes();
    let mut end = bytes.len();
    for (idx, byte) in bytes.iter().enumerate() {
        if matches!(byte, b'*' | b'?' | b'[' | b'{' | b'\\') {
            end = idx;
            break;
        }
    }
    let literal = &glob[..end];
    match literal.rfind('/') {
        Some(idx) => literal[..idx].to_string(),
        None => String::new(),
    }
}

/// `/`-boundary overlap between a glob stem and a rule prefix: equal, or one
/// sits below the other. An empty side is the repo root and overlaps
/// everything below it.
fn paths_overlap(a: &str, b: &str) -> bool {
    a.is_empty()
        || b.is_empty()
        || a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

/// Whether a rule may be selected on a mission surface (D-B): effective
/// `approved` or `enforced`. Retired never applies; draft is confined to
/// lint/authoring surfaces, which this module never serves.
fn selectable(manifest: &StandardsManifest, rule: &RuleMeta) -> bool {
    matches!(
        manifest.effective_status(rule),
        RfcStatus::Approved | RfcStatus::Enforced
    )
}

/// Resolve one stage's applicable rules over a live manifest (D-D):
/// lifecycle-filtered and stable-sorted by id, so identical inputs always
/// produce an identical selection.
pub fn resolve(
    manifest: &StandardsManifest,
    stage: RuleStage,
    task_class: Option<&str>,
    touch: &TouchInput,
) -> Vec<RuleMeta> {
    let mut selected: Vec<RuleMeta> = manifest
        .rules
        .iter()
        .filter(|rule| selectable(manifest, rule) && rule_applies(rule, stage, task_class, touch))
        .cloned()
        .collect();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    selected
}

/// The mission-wide applicable set pinned at approval (D-E/D-G's "one
/// resolved set"): the union over the four workflow stages — a rule is
/// selected when it applies at ANY of them. Stage projections later filter
/// this set back down per stage.
pub fn resolve_mission_set(
    manifest: &StandardsManifest,
    task_class: Option<&str>,
    touch: &TouchInput,
) -> Vec<RuleMeta> {
    let mut selected: Vec<RuleMeta> = manifest
        .rules
        .iter()
        .filter(|rule| {
            selectable(manifest, rule)
                && [
                    RuleStage::Planning,
                    RuleStage::Implementation,
                    RuleStage::Validation,
                    RuleStage::Merge,
                ]
                .iter()
                .any(|stage| rule_applies(rule, *stage, task_class, touch))
        })
        .cloned()
        .collect();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    selected
}

// ---------------------------------------------------------------------------
// The pin (D-E)
// ---------------------------------------------------------------------------

/// Snapshot one resolved rule into its pinned form — the canonical spellings
/// the plan contract carries as strings.
fn pin_rule(manifest: &StandardsManifest, rule: &RuleMeta) -> PinnedRule {
    PinnedRule {
        id: rule.id.clone(),
        revision: rule.revision,
        rfc: rule.rfc.clone(),
        level: rule.level.as_str().to_string(),
        effective_status: manifest.effective_status(rule).as_str().to_string(),
        statement: rule.statement.clone(),
        domains: rule.domains.clone(),
        stages: rule
            .stages
            .iter()
            .map(RuleStage::as_str)
            .map(str::to_string)
            .collect(),
        when_paths: rule.when_paths.clone(),
        task_classes: rule.task_classes.clone(),
        checker: rule.checker.as_ref().map(Checker::render),
        waivable: rule.waivable,
    }
}

/// Build the approval pin from a freshly resolved trusted manifest.
pub fn pin_from_manifest(
    manifest: &StandardsManifest,
    source: StandardsPinSource,
    pack_name: &str,
    pack_dir: &str,
    task_class: Option<&str>,
    touch_set: &[String],
) -> StandardsPin {
    let resolved = resolve_mission_set(manifest, task_class, &TouchInput::Declared(touch_set));
    StandardsPin {
        pack_name: pack_name.to_string(),
        pack_dir: pack_dir.to_string(),
        standards_root: manifest.root.clone(),
        digest: manifest.digest.clone(),
        source,
        task_class: task_class.map(crate::routing::normalize_task_class),
        touch_set: touch_set.to_vec(),
        gates: manifest
            .pack_gates
            .iter()
            .map(|gate| PinnedGate {
                id: gate.name.clone(),
                command: gate.command.clone(),
                when_paths: gate.when_paths.clone(),
            })
            .collect(),
        rules: resolved
            .iter()
            .map(|rule| pin_rule(manifest, rule))
            .collect(),
    }
}

/// Re-resolve a PIN at one stage over `touch` — final validation and merge
/// read the approved snapshot through this, never a live source. A pinned
/// stage string that no longer parses scopes the rule to NO stage: the pin
/// is engine-written, so an unparseable entry means a hand-edited plan, and
/// the merge drift check then fails closed against the live side.
pub fn resolve_pin(pin: &StandardsPin, stage: RuleStage, touch: &TouchInput) -> Vec<PinnedRule> {
    let task_class = pin.task_class.as_deref();
    let mut selected: Vec<PinnedRule> = pin
        .rules
        .iter()
        .filter(|rule| {
            let stages: Vec<RuleStage> = rule
                .stages
                .iter()
                .filter_map(|name| RuleStage::parse(name))
                .collect();
            if stages.len() != rule.stages.len() || !stages.contains(&stage) {
                return false;
            }
            if !rule.task_classes.is_empty() {
                let Some(task_class) = task_class else {
                    return false;
                };
                let wanted = crate::routing::normalize_task_class(task_class);
                if !rule
                    .task_classes
                    .iter()
                    .any(|class| crate::routing::normalize_task_class(class) == wanted)
                {
                    return false;
                }
            }
            match touch {
                TouchInput::Actual(paths) => {
                    crate::merge_gate::when_paths_match(&rule.when_paths, paths)
                }
                TouchInput::Declared(globs) => declared_touch_overlaps(&rule.when_paths, globs),
            }
        })
        .cloned()
        .collect();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    selected
}

// ---------------------------------------------------------------------------
// Approval: trusted-source load + stale/substituted rejection (D-E)
// ---------------------------------------------------------------------------

/// Resolve the standards pin for a plan approval: load the TRUSTED source,
/// resolve the mission-wide applicable set against the plan's touch set, and
/// reconcile with the manifest the plan already carries. Returns the pin to
/// attach to the plan (`None` — and a byte-identical approval — when no
/// standards-configured pack governs).
///
/// Trusted sources (D-A/D-E):
/// - no `packDir` configured: no standards — a carried manifest is a
///   substitution and rejected;
/// - a repo-relative `packDir`: tracked blobs at `base_ref`
///   ([`load_at_ref`]) — a mission-branch or worktree edit is invisible to
///   this read. A malformed base corpus fails BEFORE any approval side
///   effect. A worktree pack that DECLARES standards while the base has none
///   is an untracked-policy attempt and is refused naming the remedy;
/// - an absolute `packDir` (external/untracked): one capability read,
///   `External` trust — an effectively enforced rule fails the load here
///   (D-A/D-J: advisory-only until tracked or signed/versioned).
///
/// A carried manifest equal to the fresh resolution approves; a differing
/// one is stale or substituted and is rejected naming both digests.
pub fn approval_pin(
    repo: &GitRepo,
    cfg: &MissionConfig,
    repo_root: &Path,
    base_ref: &str,
    task_class: Option<&str>,
    carried: Option<&StandardsPin>,
    touch_set: &[String],
) -> Result<Option<StandardsPin>, String> {
    let Some(configured) = cfg.pack_dir.as_deref() else {
        return match carried {
            None => Ok(None),
            Some(_) => Err(
                "plan carries a standardsManifest but no packDir is configured — a \
                 substituted manifest is never approved"
                    .to_string(),
            ),
        };
    };

    let raw = Path::new(configured);
    let fresh: Option<StandardsPin> = if raw.is_absolute() {
        // External pack: capability-read once, here; the pin is the only
        // authority from approval on (D-E). The loader applies External
        // trust, so an effectively enforced rule is a refusal naming the
        // trust remedy.
        let pack =
            super::Pack::load_with_trust(raw, StandardsTrust::External)?.ok_or_else(|| {
                format!(
                    "packDir `{configured}` resolves to {}, which has no {} — it is not a pack",
                    raw.display(),
                    super::PACK_MANIFEST
                )
            })?;
        match &pack.standards {
            None => None,
            Some(manifest) => Some(pin_from_manifest(
                manifest,
                StandardsPinSource::ExternalPinned,
                &pack.name,
                configured,
                task_class,
                touch_set,
            )),
        }
    } else {
        super::validate_pack_relative_path(configured, "mission config", "packDir")?;
        let pack_rel = crate::merge_gate::normalize_relative_path(configured, false);
        match load_at_ref(repo, base_ref, &pack_rel)? {
            Some(manifest) => {
                let name = pack_name_at_ref(repo, base_ref, &pack_rel)?
                    .unwrap_or_else(|| pack_rel.clone());
                Some(pin_from_manifest(
                    &manifest,
                    StandardsPinSource::RepoTracked,
                    &name,
                    &pack_rel,
                    task_class,
                    touch_set,
                ))
            }
            None => {
                // The base ref carries no standards for this packDir. If the
                // WORKTREE pack declares a corpus anyway, the operator is
                // pointing at an untracked pack — blocking policy needs base
                // history (D-A), so refuse naming the remedy rather than
                // silently running standards-free. A schema-2/3 pack (or a
                // missing/unparseable one — run start owns that failure)
                // stays byte-identical.
                let worktree_declares_standards = match super::Pack::load_with_trust(
                    &repo_root.join(raw),
                    StandardsTrust::External,
                ) {
                    Ok(pack) => pack.is_some_and(|pack| pack.standards.is_some()),
                    Err(error) => {
                        return Err(format!(
                            "packDir `{configured}` is not tracked on base branch `{base_ref}` \
                             and its worktree pack cannot be accepted as advisory-only: {error}"
                        ));
                    }
                };
                if worktree_declares_standards {
                    return Err(format!(
                        "packDir `{configured}` declares a standards corpus but is not tracked \
                         on base branch `{base_ref}` — policy a mission is judged by needs base \
                         history: commit the pack to the base branch (a vendor-style tracked, \
                         repo-relative pack), or point packDir at an absolute path for an \
                         advisory-only external pack"
                    ));
                }
                None
            }
        }
    };

    // The projection budget (KRZ-345): fail approval naming the excess BEFORE
    // any approval side effect — never truncate an enforced rule to fit.
    check_projection_budget(&fresh)?;

    match (carried, fresh) {
        (None, fresh) => Ok(fresh),
        (Some(_), None) => Err(format!(
            "plan carries a standardsManifest but no standards govern at the trusted source \
             for packDir `{configured}` — a substituted manifest is never approved"
        )),
        (Some(carried), Some(fresh)) if *carried == fresh => Ok(Some(fresh)),
        (Some(carried), Some(fresh)) => Err(format!(
            "plan carries a stale or substituted standardsManifest (digest sha256:{}, {} \
             rule(s)) — the trusted source for packDir `{configured}` resolves to sha256:{} \
             ({} rule(s)); re-draft the plan against the current policy",
            carried.digest,
            carried.rules.len(),
            fresh.digest,
            fresh.rules.len()
        )),
    }
}

/// The projection-budget approval gate (KRZ-345, design D-D/D-J): the
/// applicable normative statements must fit the hard projection caps, or
/// approval FAILS naming the excess — an enforced rule is never silently
/// dropped to fit a prompt budget. Checked against the fresh trusted
/// resolution so every approval path (initial, carried, revised) fails
/// closed on the same contract.
fn check_projection_budget(fresh: &Option<StandardsPin>) -> Result<(), String> {
    if let Some(pin) = fresh {
        super::projection::check_budget(
            pin.rules
                .iter()
                .map(|rule| (rule.id.as_str(), rule.statement.as_str())),
        )?;
    }
    Ok(())
}

/// The pack name as committed at `refname` (the pin's display identity).
/// `load_at_ref` deliberately returns only the standards manifest; the name
/// is audit metadata, re-read here from the same tracked `pack.toml` rather
/// than by widening the KRZ-341 loader's signature. `pub(crate)` for the
/// KRZ-345 planning projection, which names the same source identity in the
/// planner-facing seed header.
pub(crate) fn pack_name_at_ref(
    repo: &GitRepo,
    refname: &str,
    pack_rel_dir: &str,
) -> Result<Option<String>, String> {
    let manifest_rel = if pack_rel_dir.is_empty() {
        super::PACK_MANIFEST.to_string()
    } else {
        format!("{pack_rel_dir}/{}", super::PACK_MANIFEST)
    };
    let Some(bytes) = repo
        .show_file(refname, &manifest_rel)
        .map_err(|e| format!("cannot read {manifest_rel} at `{refname}`: {e}"))?
    else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{manifest_rel} at `{refname}` is not valid UTF-8"))?;
    let doc =
        super::toml::parse(&text).map_err(|e| format!("{manifest_rel} at `{refname}`: {e}"))?;
    let (name, _schema) =
        super::manifest_header(&doc).map_err(|e| format!("{manifest_rel} at `{refname}`: {e}"))?;
    Ok(Some(name))
}

// ---------------------------------------------------------------------------
// Final validation: the approved-envelope check (D-E)
// ---------------------------------------------------------------------------

/// The final-validation envelope check: re-read the pinned source snapshot
/// (the mission's pinned `base_sha` — immutable, so exactly the bytes
/// approval read) and resolve it against the ACTUAL changed paths. Any
/// effectively ENFORCED rule that is applicable now but was not pinned is a
/// newly applicable enforced rule: the mission escaped its approved policy
/// envelope and must be revised/reapproved, not silently judged against a
/// moving set. Returns the offending rules as pin-shaped snapshots.
///
/// External pins need no re-read: the pinned bytes are the only authority,
/// and an external corpus can never carry enforced rules (the loader
/// refuses them), so nothing can newly block.
pub fn newly_applicable_enforced(
    repo: &GitRepo,
    base_sha: &str,
    pin: &StandardsPin,
    actual_paths: &[String],
) -> Result<Vec<PinnedRule>, String> {
    if pin.source == StandardsPinSource::ExternalPinned {
        return Ok(Vec::new());
    }
    let manifest = load_at_ref(repo, base_sha, &pin.pack_dir)?.ok_or_else(|| {
        format!(
            "the pinned base {base_sha} no longer yields the approved standards pack `{}` \
                 (pinned digest sha256:{}) — the approval snapshot is inconsistent; re-approve \
                 the mission",
            pin.pack_dir, pin.digest
        )
    })?;
    if manifest.digest != pin.digest {
        // Same immutable ref must give same bytes; a mismatch means the ref
        // was rewritten — fail closed rather than compare against policy the
        // operator never saw.
        return Err(format!(
            "the standards pack `{}` at the pinned base {base_sha} digests to sha256:{} but \
             approval pinned sha256:{} — the base history moved under the mission; re-approve \
             against the current policy",
            pin.pack_dir, manifest.digest, pin.digest
        ));
    }
    let task_class = pin.task_class.as_deref();
    let now = resolve_mission_set(&manifest, task_class, &TouchInput::Actual(actual_paths));
    Ok(now
        .iter()
        .filter(|rule| manifest.effective_status(rule) == RfcStatus::Enforced)
        .filter(|rule| !pin.rules.iter().any(|pinned| pinned.id == rule.id))
        .map(|rule| pin_rule(&manifest, rule))
        .collect())
}

// ---------------------------------------------------------------------------
// Merge: live-base policy drift (D-E)
// ---------------------------------------------------------------------------

/// The merge-time drift verdict: the applicable ENFORCED set resolved from
/// the live base differs from the approved pin's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftReport {
    /// The digest pinned at approval.
    pub approved_digest: String,
    /// The digest resolved from the live base — `None` when the live base
    /// no longer yields a readable standards manifest (removed or malformed:
    /// the ultimate drift, failed closed).
    pub current_digest: Option<String>,
    /// Id-level change lines (added / removed / changed), stable-sorted.
    pub changed_rules: Vec<String>,
}

/// The merge-time policy-drift check (D-E): re-resolve the LIVE base policy
/// against the exact scratch integration diff and compare the applicable
/// ENFORCED set against the approved pin's, both sides resolved with the
/// same inputs (merge stage, the pin's task class, the integration paths) so
/// an unchanged base can never false-positive. `Ok(None)` — no drift — lets
/// the merge proceed. External pins skip entirely: advisory-only, and the
/// pinned bytes remain the authority (a later filesystem edit cannot change
/// a run).
pub fn merge_drift(
    repo: &GitRepo,
    live_base_ref: &str,
    pin: &StandardsPin,
    integration_paths: &[String],
) -> Result<Option<DriftReport>, String> {
    if pin.source == StandardsPinSource::ExternalPinned {
        return Ok(None);
    }
    let approved_rules = resolve_pin(
        pin,
        RuleStage::Merge,
        &TouchInput::Actual(integration_paths),
    );
    let approved = enforced_snapshot(&approved_rules);
    let approved_bindings = pinned_enforced_gate_snapshot(&approved_rules, &pin.gates);
    let (current, current_bindings, current_digest) =
        match load_at_ref(repo, live_base_ref, &pin.pack_dir) {
            Ok(Some(manifest)) => {
                let resolved = resolve(
                    &manifest,
                    RuleStage::Merge,
                    pin.task_class.as_deref(),
                    &TouchInput::Actual(integration_paths),
                );
                let pinned: Vec<PinnedRule> = resolved
                    .iter()
                    .map(|rule| pin_rule(&manifest, rule))
                    .collect();
                let bindings = manifest_enforced_gate_snapshot(&pinned, &manifest.pack_gates);
                (
                    enforced_snapshot(&pinned),
                    bindings,
                    Some(manifest.digest.clone()),
                )
            }
            Ok(None) => (
                std::collections::BTreeMap::new(),
                std::collections::BTreeMap::new(),
                None,
            ),
            Err(error) => {
                // A live base whose policy cannot be read must fail closed:
                // merging under an unknowable enforced set is not an option.
                return Ok(Some(DriftReport {
                    approved_digest: pin.digest.clone(),
                    current_digest: None,
                    changed_rules: vec![format!(
                        "live base standards pack `{}` failed to load: {error}",
                        pin.pack_dir
                    )],
                }));
            }
        };
    let mut changed = drift_lines(&approved, &current);
    for (rule_id, approved_gate) in &approved_bindings {
        match current_bindings.get(rule_id) {
            Some(current_gate) if current_gate == approved_gate => {}
            Some(_) => changed.push(format!(
                "{rule_id} checker gate declaration changed on the live base since approval"
            )),
            None => changed.push(format!(
                "{rule_id} checker gate declaration is missing on the live base"
            )),
        }
    }
    for rule_id in current_bindings.keys() {
        if !approved_bindings.contains_key(rule_id) {
            changed.push(format!(
                "{rule_id} checker gate declaration is newly applicable on the live base"
            ));
        }
    }
    changed.sort();
    changed.dedup();
    if changed.is_empty() {
        return Ok(None);
    }
    Ok(Some(DriftReport {
        approved_digest: pin.digest.clone(),
        current_digest,
        changed_rules: changed,
    }))
}

fn pinned_enforced_gate_snapshot(
    rules: &[PinnedRule],
    gates: &[crate::types::PinnedGate],
) -> std::collections::BTreeMap<String, crate::types::PinnedGate> {
    rules
        .iter()
        .filter(|rule| rule.effective_status == RfcStatus::Enforced.as_str())
        .filter_map(|rule| {
            let id = rule.checker.as_deref()?.strip_prefix("gate:")?;
            gates
                .iter()
                .find(|gate| gate.id == id)
                .cloned()
                .map(|gate| (rule.id.clone(), gate))
        })
        .collect()
}

fn manifest_enforced_gate_snapshot(
    rules: &[PinnedRule],
    gates: &[super::PackGateDecl],
) -> std::collections::BTreeMap<String, crate::types::PinnedGate> {
    rules
        .iter()
        .filter(|rule| rule.effective_status == RfcStatus::Enforced.as_str())
        .filter_map(|rule| {
            let id = rule.checker.as_deref()?.strip_prefix("gate:")?;
            gates.iter().find(|gate| gate.name == id).map(|gate| {
                (
                    rule.id.clone(),
                    crate::types::PinnedGate {
                        id: gate.name.clone(),
                        command: gate.command.clone(),
                        when_paths: gate.when_paths.clone(),
                    },
                )
            })
        })
        .collect()
}

/// The applicable ENFORCED snapshot, keyed by rule id: the drift comparison
/// unit. The full pinned rule is the value, so a revision bump, a statement
/// or scope edit, a checker rebind, or a waiver-posture change all count —
/// the merge must not rely on the lifecycle lint having run.
fn enforced_snapshot(rules: &[PinnedRule]) -> std::collections::BTreeMap<String, PinnedRule> {
    rules
        .iter()
        .filter(|rule| rule.effective_status == RfcStatus::Enforced.as_str())
        .map(|rule| (rule.id.clone(), rule.clone()))
        .collect()
}

/// Id-level change lines between two enforced snapshots, stable-sorted.
fn drift_lines(
    approved: &std::collections::BTreeMap<String, PinnedRule>,
    current: &std::collections::BTreeMap<String, PinnedRule>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for (id, rule) in current {
        match approved.get(id) {
            None => lines.push(format!(
                "{id} r{} (newly applicable enforced rule on the live base)",
                rule.revision
            )),
            Some(before) if *before != *rule => lines.push(format!(
                "{id} r{} -> r{} (changed on the live base since approval)",
                before.revision, rule.revision
            )),
            Some(_) => {}
        }
    }
    for id in approved.keys() {
        if !current.contains_key(id) {
            lines.push(format!(
                "{id} r{} (approved enforced rule absent from the live base policy)",
                approved[id].revision
            ));
        }
    }
    lines.sort();
    lines
}

// ---------------------------------------------------------------------------
// Review rendering (D-G: plan review sees the exact rules being accepted)
// ---------------------------------------------------------------------------

/// The plan.md section for a pin: source identity + digest, the selection
/// inputs, and every applicable rule's id, revision, effective status,
/// statement, scopes, and checker binding — the review surface D-G requires.
pub fn render_pin_section(pin: &StandardsPin) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "## Flight Rules standards (approved manifest pin)\n");
    let _ = writeln!(
        out,
        "Pack `{}` (`{}`, source {}) — standards root `{}`, digest `sha256:{}`.",
        pin.pack_name,
        pin.pack_dir,
        pin.source.as_str(),
        pin.standards_root,
        pin.digest
    );
    let task_class = pin.task_class.as_deref().unwrap_or("(none)");
    let touch_set = if pin.touch_set.is_empty() {
        "(empty)".to_string()
    } else {
        pin.touch_set.join(", ")
    };
    let _ = writeln!(
        out,
        "Resolved with task class `{task_class}` over touch set: {touch_set}. \
         This snapshot — not a later branch or filesystem read — governs every mission stage.\n"
    );
    if pin.rules.is_empty() {
        let _ = writeln!(out, "No rules apply to this mission's selection inputs.");
        return out;
    }
    for rule in &pin.rules {
        let checker = rule.checker.as_deref().unwrap_or("-");
        let _ = writeln!(
            out,
            "- **{} r{}** — {}, {}; checker `{}`; waivable: {}",
            rule.id, rule.revision, rule.level, rule.effective_status, checker, rule.waivable
        );
        let _ = writeln!(out, "  - statement: {}", rule.statement);
        let list = |items: &[String]| {
            if items.is_empty() {
                "-".to_string()
            } else {
                items.join(", ")
            }
        };
        let _ = writeln!(
            out,
            "  - stages: {}; when-paths: {}; task-classes: {}; domains: {}",
            list(&rule.stages),
            list(&rule.when_paths),
            list(&rule.task_classes),
            list(&rule.domains)
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Tests (ticket flight-rules-resolution-pin; anti-vacuity prefix
// `flight_rules_pin_` — grep-verified unique to this ticket's tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventKind;
    use crate::pack::standards::StandardsTrust;
    use std::path::PathBuf;

    // ---- fixtures ---------------------------------------------------------

    /// The schema-4 fixture manifest: one declared gate (checker target),
    /// one standards root.
    const PACK_TOML: &str = "[pack]\nname = \"zz-pin-pack\"\nschema = 4\n\n\
                             [standards]\nroot = \"standards\"\n\n\
                             [[gate]]\nname = \"zz-gate\"\ncommand = \"cd .\"\n";

    fn rfc_md(id: &str, status: &str) -> String {
        format!("---\nid: {id}\ntitle: zz fixture\nstatus: {status}\nowner: zz\n---\nprose\n")
    }

    #[allow(clippy::too_many_arguments)]
    fn rule_md(
        id: &str,
        rfc: &str,
        revision: u64,
        level: &str,
        status: &str,
        stages: &str,
        when_paths: Option<&str>,
        task_classes: Option<&str>,
        checker: Option<&str>,
    ) -> String {
        // `domains` is a required field (browsing labels only — selection
        // never reads it, which several tests prove by NOT varying it).
        let mut out = format!(
            "---\nid: {id}\nrevision: {revision}\nrfc: {rfc}\nlevel: {level}\nstatus: \
             {status}\nstatement: zz statement for {id}.\ndomains: [zz]\nstages: [{stages}]\n"
        );
        if let Some(paths) = when_paths {
            out.push_str(&format!("when-paths: [{paths}]\n"));
        }
        if let Some(classes) = task_classes {
            out.push_str(&format!("task-classes: [{classes}]\n"));
        }
        if let Some(checker) = checker {
            out.push_str(&format!("checker: {checker}\n"));
        }
        out.push_str("---\nprose\n");
        out
    }

    /// A pack dir with the given RFCs and rules; returns the TempDir (kept
    /// alive by the caller) and the pack dir.
    fn pack_dir_with(
        rfcs: &[(&str, &str)],
        rules: &[(&str, String)],
    ) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("pack");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(super::super::PACK_MANIFEST), PACK_TOML).unwrap();
        for (id, status) in rfcs {
            let path = dir.join(format!("standards/{id}-slug/rfc.md"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, rfc_md(id, status)).unwrap();
        }
        for (id, body) in rules {
            let rfc = body
                .lines()
                .find_map(|line| line.strip_prefix("rfc: "))
                .expect("rule fixture names its rfc")
                .to_string();
            let path = dir.join(format!("standards/{rfc}-slug/rules/{id}.md"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        (tmp, dir)
    }

    /// Load a fixture pack's standards manifest as repo-tracked (the trust
    /// level that lets enforced rules activate).
    fn manifest_of(dir: &Path) -> StandardsManifest {
        crate::pack::Pack::load_with_trust(dir, StandardsTrust::RepoTracked)
            .expect("load")
            .expect("a pack")
            .standards
            .expect("a standards manifest")
    }

    /// A temp git repo whose HEAD commits the given files; returns the
    /// TempDir, the root, and the opened repo (the standards.rs fixture
    /// idiom).
    fn git_repo_with_files(
        files: &[(String, String)],
    ) -> Option<(tempfile::TempDir, PathBuf, GitRepo)> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&root)
            .output()
            .ok()?;
        if !init.status.success() {
            eprintln!("skipping test: git is not on PATH");
            return None;
        }
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        for (rel, body) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "pack"]);
        let repo = GitRepo::open(&root).expect("git repo");
        Some((tmp, root, repo))
    }

    /// Commit the fixture pack (one approved RFC, one enforced RFC with a
    /// gated must rule) vendored under `vendor/pack`, plus a seed file.
    fn vendored_pack_files(enforced_rfc_status: &str) -> Vec<(String, String)> {
        vec![
            ("README.md".to_string(), "seed\n".to_string()),
            ("vendor/pack/pack.toml".to_string(), PACK_TOML.to_string()),
            (
                "vendor/pack/standards/RFC-001-slug/rfc.md".to_string(),
                rfc_md("RFC-001", "approved"),
            ),
            (
                "vendor/pack/standards/RFC-001-slug/rules/ZZ-ADV-001.md".to_string(),
                rule_md(
                    "ZZ-ADV-001",
                    "RFC-001",
                    1,
                    "should",
                    "active",
                    "planning, implementation, validation, merge",
                    None,
                    None,
                    Some("agent-judgement"),
                ),
            ),
            (
                "vendor/pack/standards/RFC-002-slug/rfc.md".to_string(),
                rfc_md("RFC-002", enforced_rfc_status),
            ),
            (
                "vendor/pack/standards/RFC-002-slug/rules/ZZ-MUST-001.md".to_string(),
                rule_md(
                    "ZZ-MUST-001",
                    "RFC-002",
                    1,
                    "must",
                    "active",
                    "implementation, validation, merge",
                    Some("crates/"),
                    None,
                    Some("gate:zz-gate"),
                ),
            ),
        ]
    }

    fn cfg_with_pack(pack_dir: Option<String>) -> MissionConfig {
        MissionConfig {
            pack_dir,
            ..MissionConfig::default()
        }
    }

    // ---- D-D: deterministic selection -------------------------------------

    #[test]
    fn flight_rules_pin_resolution_is_deterministic_and_stable_sorted() {
        let (_tmp, dir) = pack_dir_with(
            &[("RFC-001", "approved")],
            &[
                (
                    "ZZ-B-002",
                    rule_md(
                        "ZZ-B-002",
                        "RFC-001",
                        1,
                        "should",
                        "active",
                        "validation",
                        None,
                        None,
                        Some("agent-judgement"),
                    ),
                ),
                (
                    "ZZ-A-001",
                    rule_md(
                        "ZZ-A-001",
                        "RFC-001",
                        3,
                        "must",
                        "active",
                        "validation",
                        None,
                        None,
                        Some("agent-judgement"),
                    ),
                ),
            ],
        );
        let manifest = manifest_of(&dir);
        let touch = TouchInput::Actual(&["crates/x.rs".to_string()]);
        let first = resolve(
            &manifest,
            RuleStage::Validation,
            Some("implementation"),
            &touch,
        );
        let second = resolve(
            &manifest,
            RuleStage::Validation,
            Some("implementation"),
            &touch,
        );
        assert_eq!(first, second, "same inputs must select identically");
        let ids: Vec<&str> = first.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["ZZ-A-001", "ZZ-B-002"], "stable-sorted by id");

        // The mission-wide set agrees on membership and order regardless of
        // which single stage was asked first.
        let mission = resolve_mission_set(
            &manifest,
            Some("implementation"),
            &TouchInput::Declared(&["crates/**".to_string()]),
        );
        let mission_ids: Vec<&str> = mission.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(mission_ids, ["ZZ-A-001", "ZZ-B-002"]);
    }

    #[test]
    fn flight_rules_pin_domains_never_select() {
        // The fixture rules all carry `domains: [zz]` — a label any browsing
        // query would "match" — yet selection must consult only stage, task
        // class, and when-paths (D-D).
        let (_tmp, dir) = pack_dir_with(
            &[("RFC-001", "approved")],
            &[(
                "ZZ-SCOPED",
                rule_md(
                    "ZZ-SCOPED",
                    "RFC-001",
                    1,
                    "must",
                    "active",
                    "validation",
                    Some("crates/"),
                    None,
                    Some("agent-judgement"),
                ),
            )],
        );
        let manifest = manifest_of(&dir);
        // Domain-relevant by label, path-irrelevant by scope: not selected.
        let miss = resolve(
            &manifest,
            RuleStage::Validation,
            None,
            &TouchInput::Actual(&["docs/readme.md".to_string()]),
        );
        assert!(miss.is_empty(), "domains never invoke selection: {miss:?}");
        let hit = resolve(
            &manifest,
            RuleStage::Validation,
            None,
            &TouchInput::Actual(&["crates/lib.rs".to_string()]),
        );
        assert_eq!(hit.len(), 1);
    }

    #[test]
    fn flight_rules_pin_lifecycle_retired_and_draft_never_apply() {
        let (_tmp, dir) = pack_dir_with(
            &[
                ("RFC-001", "draft"),
                ("RFC-002", "enforced"),
                ("RFC-003", "retired"),
            ],
            &[
                (
                    "ZZ-DRAFT",
                    rule_md(
                        "ZZ-DRAFT",
                        "RFC-001",
                        1,
                        "must",
                        "active",
                        "validation",
                        None,
                        None,
                        None,
                    ),
                ),
                (
                    "ZZ-ENFORCED",
                    rule_md(
                        "ZZ-ENFORCED",
                        "RFC-002",
                        1,
                        "must",
                        "active",
                        "validation",
                        None,
                        None,
                        Some("gate:zz-gate"),
                    ),
                ),
                (
                    "ZZ-TOMBSTONE",
                    rule_md(
                        "ZZ-TOMBSTONE",
                        "RFC-002",
                        1,
                        "must",
                        "retired",
                        "validation",
                        None,
                        None,
                        None,
                    ),
                ),
                (
                    "ZZ-RETIRED",
                    rule_md(
                        "ZZ-RETIRED",
                        "RFC-003",
                        1,
                        "must",
                        "active",
                        "validation",
                        None,
                        None,
                        None,
                    ),
                ),
            ],
        );
        let manifest = manifest_of(&dir);
        let selected = resolve(
            &manifest,
            RuleStage::Validation,
            None,
            &TouchInput::Actual(&["crates/x.rs".to_string()]),
        );
        let ids: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            ["ZZ-ENFORCED"],
            "draft-RFC rules, tombstones, and retired-RFC rules never apply on mission surfaces"
        );
        assert_eq!(manifest.effective_status(&selected[0]), RfcStatus::Enforced);
    }

    #[test]
    fn flight_rules_pin_task_class_and_stage_scoping() {
        let (_tmp, dir) = pack_dir_with(
            &[("RFC-001", "approved")],
            &[
                (
                    "ZZ-IMPL",
                    rule_md(
                        "ZZ-IMPL",
                        "RFC-001",
                        1,
                        "should",
                        "active",
                        "implementation",
                        None,
                        Some("implementation"),
                        Some("agent-judgement"),
                    ),
                ),
                (
                    "ZZ-ANY",
                    rule_md(
                        "ZZ-ANY",
                        "RFC-001",
                        1,
                        "should",
                        "active",
                        "implementation",
                        None,
                        None,
                        Some("agent-judgement"),
                    ),
                ),
            ],
        );
        let manifest = manifest_of(&dir);
        let touch = TouchInput::Actual(&["crates/x.rs".to_string()]);
        // Routing normalization: case/trim-insensitive match on the class.
        let hit = resolve(
            &manifest,
            RuleStage::Implementation,
            Some("  Implementation "),
            &touch,
        );
        assert_eq!(hit.len(), 2);
        // A different class: the class-scoped rule drops out.
        let docs = resolve(&manifest, RuleStage::Implementation, Some("docs"), &touch);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].id, "ZZ-ANY");
        // A classless mission matches no class-scoped rule.
        let classless = resolve(&manifest, RuleStage::Implementation, None, &touch);
        assert_eq!(classless.len(), 1);
        // Wrong stage: neither applies.
        assert!(resolve(&manifest, RuleStage::Merge, Some("implementation"), &touch).is_empty());
    }

    #[test]
    fn flight_rules_pin_declared_overlap_and_actual_prefix_matching() {
        let (_tmp, dir) = pack_dir_with(
            &[("RFC-001", "approved")],
            &[(
                "ZZ-ENG",
                rule_md(
                    "ZZ-ENG",
                    "RFC-001",
                    1,
                    "must",
                    "active",
                    "validation",
                    Some("crates/engine"),
                    None,
                    Some("gate:zz-gate"),
                ),
            )],
        );
        let manifest = manifest_of(&dir);

        // Declared (approval) semantics: conservative overlap.
        for (globs, expect) in [
            (vec!["crates/**"], true),                  // mission may reach the prefix
            (vec!["crates/engine/**"], true),           // stem sits below the prefix
            (vec!["crates/engine/src/types.rs"], true), // exact path below
            (vec!["**"], true),                         // root glob admits everything
            (vec!["docs/**"], false),                   // disjoint
            (vec!["!crates/**"], false),                // a negation-only set admits nothing
            (vec![], false),                            // empty touch set: no path-scoped rule
        ] {
            let selected = resolve(
                &manifest,
                RuleStage::Validation,
                None,
                &TouchInput::Declared(&globs.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
            );
            assert_eq!(
                !selected.is_empty(),
                expect,
                "declared touch set {globs:?} overlap must be {expect}"
            );
        }

        // Actual (validation/merge) semantics: exact at/below prefix, the
        // merge-gate idiom — a sibling crate does NOT match.
        for (paths, expect) in [
            (vec!["crates/engine"], true),
            (vec!["crates/engine/src/types.rs"], true),
            (vec!["crates/cli/main.rs"], false),
            (vec!["crates/engine-extra/x.rs"], false), // not a /-boundary match
        ] {
            let selected = resolve(
                &manifest,
                RuleStage::Validation,
                None,
                &TouchInput::Actual(&paths.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
            );
            assert_eq!(
                !selected.is_empty(),
                expect,
                "actual paths {paths:?} prefix match must be {expect}"
            );
        }

        // The superset property the envelope check relies on: a path admitted
        // by the declared glob AND below the prefix was selected at approval.
        let declared = resolve(
            &manifest,
            RuleStage::Validation,
            None,
            &TouchInput::Declared(&["crates/**".to_string()]),
        );
        let actual = resolve(
            &manifest,
            RuleStage::Validation,
            None,
            &TouchInput::Actual(&["crates/engine/src/types.rs".to_string()]),
        );
        assert!(declared.len() >= actual.len());
    }

    // ---- D-E: approval pinning --------------------------------------------

    #[test]
    fn flight_rules_pin_approval_pins_from_trusted_base_and_rejects_stale() {
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("enforced")) else {
            return;
        };
        let cfg = cfg_with_pack(Some("vendor/pack".to_string()));
        let touch_set = vec!["crates/**".to_string()];

        // Engine-authored pin: the plan carried nothing.
        let pin = approval_pin(
            &repo,
            &cfg,
            &root,
            "main",
            Some("implementation"),
            None,
            &touch_set,
        )
        .expect("pin")
        .expect("standards govern");
        assert_eq!(pin.pack_name, "zz-pin-pack");
        assert_eq!(pin.pack_dir, "vendor/pack");
        assert_eq!(pin.source, StandardsPinSource::RepoTracked);
        assert_eq!(pin.task_class.as_deref(), Some("implementation"));
        let ids: Vec<&str> = pin.rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["ZZ-ADV-001", "ZZ-MUST-001"]);
        let must = &pin.rules[1];
        assert_eq!(must.effective_status, "enforced");
        assert_eq!(must.checker.as_deref(), Some("gate:zz-gate"));
        assert_eq!(must.when_paths, vec!["crates".to_string()]);

        // A carried manifest identical to the fresh resolution approves.
        let again = approval_pin(
            &repo,
            &cfg,
            &root,
            "main",
            Some("implementation"),
            Some(&pin),
            &touch_set,
        )
        .expect("a carried manifest equal to the trusted resolution approves");
        assert_eq!(again.as_ref(), Some(&pin));

        // A stale or substituted carried manifest is rejected, naming both
        // digests.
        let mut stale = pin.clone();
        stale.digest = "0".repeat(64);
        let err = approval_pin(
            &repo,
            &cfg,
            &root,
            "main",
            Some("implementation"),
            Some(&stale),
            &touch_set,
        )
        .expect_err("a stale manifest must be rejected");
        assert!(err.contains("stale or substituted"), "{err}");
        assert!(err.contains(&pin.digest), "{err}");

        // A carried manifest with no pack configured is a substitution.
        let err = approval_pin(
            &repo,
            &cfg_with_pack(None),
            &root,
            "main",
            Some("implementation"),
            Some(&pin),
            &touch_set,
        )
        .expect_err("a substituted manifest must be rejected");
        assert!(err.contains("no packDir is configured"), "{err}");
    }

    #[test]
    fn flight_rules_pin_approval_ignores_mission_branch_pack_edit() {
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("enforced")) else {
            return;
        };
        let cfg = cfg_with_pack(Some("vendor/pack".to_string()));
        let pin = approval_pin(
            &repo,
            &cfg,
            &root,
            "main",
            None,
            None,
            &["crates/**".to_string()],
        )
        .expect("pin")
        .expect("standards govern");

        // A mission branch weakens the pack (retire the enforced RFC) — the
        // approval read at the base is structurally blind to it (D-A/D-E).
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["checkout", "-qb", "kranz/mission-x"]);
        std::fs::write(
            root.join("vendor/pack/standards/RFC-002-slug/rfc.md"),
            rfc_md("RFC-002", "retired"),
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "weaken policy"]);
        git(&["checkout", "-q", "main"]);

        let repin = approval_pin(
            &repo,
            &cfg,
            &root,
            "main",
            None,
            None,
            &["crates/**".to_string()],
        )
        .expect("pin")
        .expect("standards govern");
        assert_eq!(
            pin, repin,
            "the mission branch's pack edit can never reshape the approval read"
        );
        assert!(repin.rules.iter().any(|r| r.effective_status == "enforced"));
    }

    #[test]
    fn flight_rules_pin_approval_refuses_untracked_repo_pack_corpus() {
        // The pack exists ONLY in the worktree (never committed to the base):
        // an untracked repo-relative corpus has no base history, so approval
        // refuses it naming the remedy (D-A/D-J) instead of silently running
        // standards-free.
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("approved")) else {
            return;
        };
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["rm", "-rq", "vendor/pack"]);
        git(&["commit", "-qm", "drop the pack from the base"]);
        // The worktree still holds a pack (restored, untracked relative to HEAD).
        // Restore an ENFORCED corpus. External-trust loading rejects it;
        // approval must propagate that refusal rather than swallowing it as
        // `None` and silently running standards-free.
        for (rel, body) in vendored_pack_files("enforced") {
            if rel == "README.md" {
                continue;
            }
            let path = root.join(&rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let cfg = cfg_with_pack(Some("vendor/pack".to_string()));
        let err = approval_pin(&repo, &cfg, &root, "main", None, None, &[])
            .expect_err("an untracked standards pack must refuse approval");
        assert!(err.contains("not tracked on base branch"), "{err}");
    }

    #[test]
    fn flight_rules_pin_external_enforced_refused_at_approval() {
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("enforced")) else {
            return;
        };
        // An ABSOLUTE packDir is external/untracked by construction: its
        // enforced rule is refused at approval with the trust remedy (D-A).
        let external = root.join("vendor/pack");
        let cfg = cfg_with_pack(Some(external.to_string_lossy().into_owned()));
        let err = approval_pin(
            &repo,
            &cfg,
            &root,
            "main",
            None,
            None,
            &["crates/**".to_string()],
        )
        .expect_err("an enforced rule from an external pack must refuse approval");
        assert!(
            err.contains("repo-tracked") || err.contains("vendor"),
            "the refusal names the trust remedy: {err}"
        );
    }

    #[test]
    fn flight_rules_pin_external_advisory_pin_survives_later_edits() {
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("approved")) else {
            return;
        };
        // All rules advisory (approved RFC): the external pack pins fine.
        let external = root.join("vendor/pack");
        let cfg = cfg_with_pack(Some(external.to_string_lossy().into_owned()));
        let pin = approval_pin(
            &repo,
            &cfg,
            &root,
            "main",
            None,
            None,
            &["crates/**".to_string()],
        )
        .expect("pin")
        .expect("advisory standards pin");
        assert_eq!(pin.source, StandardsPinSource::ExternalPinned);
        assert!(pin.rules.iter().all(|r| r.effective_status == "approved"));

        // An external pack edit after approval cannot change the run: the
        // final-validation check needs no re-read, and merge never re-reads
        // an external pack at all.
        std::fs::write(
            root.join("vendor/pack/standards/RFC-001-slug/rules/ZZ-ADV-001.md"),
            rule_md(
                "ZZ-ADV-001",
                "RFC-001",
                9,
                "must",
                "active",
                "merge",
                None,
                None,
                None,
            ),
        )
        .unwrap();
        let newly = newly_applicable_enforced(&repo, "main", &pin, &["crates/x.rs".to_string()])
            .expect("external pins never re-read");
        assert!(newly.is_empty());
        let drift = merge_drift(&repo, "main", &pin, &["crates/x.rs".to_string()])
            .expect("external pins skip the drift check");
        assert!(drift.is_none(), "an external pack edit cannot change a run");
    }

    // ---- D-E: final validation + merge drift ------------------------------

    #[test]
    fn flight_rules_pin_final_validation_flags_newly_applicable_enforced() {
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("enforced")) else {
            return;
        };
        let cfg = cfg_with_pack(Some("vendor/pack".to_string()));
        // Approved with a touch set that never reaches `crates/` — the
        // path-scoped enforced rule is NOT in the approved set.
        let pin = approval_pin(
            &repo,
            &cfg,
            &root,
            "main",
            None,
            None,
            &["docs/**".to_string()],
        )
        .expect("pin")
        .expect("standards govern");
        assert!(pin.rules.iter().all(|r| r.id != "ZZ-MUST-001"));

        let base_sha = repo.rev_parse("main").expect("base sha");
        // The actual diff escaped the envelope into crates/: the enforced
        // rule is newly applicable — park for revision/reapproval.
        let newly = newly_applicable_enforced(
            &repo,
            &base_sha,
            &pin,
            &["crates/engine/src/lib.rs".to_string()],
        )
        .expect("check");
        let ids: Vec<&str> = newly.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["ZZ-MUST-001"]);
        assert_eq!(newly[0].effective_status, "enforced");

        // A diff INSIDE the approved envelope can never newly apply anything
        // (the declared overlap is a superset of anything it admits).
        let clean =
            newly_applicable_enforced(&repo, &base_sha, &pin, &["docs/guide.md".to_string()])
                .expect("check");
        assert!(clean.is_empty(), "{clean:?}");
    }

    #[test]
    fn flight_rules_pin_merge_drift_refuses_enforced_set_change() {
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("approved")) else {
            return;
        };
        let cfg = cfg_with_pack(Some("vendor/pack".to_string()));
        let touch = vec!["crates/**".to_string()];
        let pin = approval_pin(&repo, &cfg, &root, "main", None, None, &touch)
            .expect("pin")
            .expect("standards govern");
        // The approved RFC-002 fixture is `approved` here, so the approved
        // enforced set is EMPTY — nothing to drift yet.
        let paths = vec!["crates/engine/src/lib.rs".to_string()];
        assert!(
            merge_drift(&repo, "main", &pin, &paths)
                .expect("check")
                .is_none(),
            "an unchanged base can never drift"
        );

        // The live base promotes RFC-002 to enforced (through its approved
        // state — a legitimate transition) and bumps nothing else.
        std::fs::write(
            root.join("vendor/pack/standards/RFC-002-slug/rfc.md"),
            rfc_md("RFC-002", "enforced"),
        )
        .unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["add", "."]);
        git(&["commit", "-qm", "promote RFC-002 to enforced"]);

        let report = merge_drift(&repo, "main", &pin, &paths)
            .expect("check")
            .expect("the enforced set changed: drift must refuse");
        assert_eq!(report.approved_digest, pin.digest);
        assert!(report.current_digest.is_some());
        assert_ne!(report.current_digest.as_deref(), Some(pin.digest.as_str()));
        assert!(
            report
                .changed_rules
                .iter()
                .any(|line| line.contains("ZZ-MUST-001") && line.contains("newly applicable")),
            "{:?}",
            report.changed_rules
        );

        // And a REMOVED pack on the live base is drift too (policy removal):
        // flip the base pack's enforced RFC back via a base WITHOUT standards.
        git(&["rm", "-rq", "vendor/pack"]);
        git(&["commit", "-qm", "remove the pack"]);
        // Approve a pin that HAS an enforced rule: rebuild the pack enforced
        // at an earlier commit and pin against it.
        let enforced_files = vendored_pack_files("enforced");
        for (rel, body) in &enforced_files {
            if rel == "README.md" {
                continue;
            }
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "restore the pack enforced"]);
        let enforced_pin = approval_pin(&repo, &cfg, &root, "main", None, None, &touch)
            .expect("pin")
            .expect("standards govern");
        assert!(enforced_pin
            .rules
            .iter()
            .any(|r| r.effective_status == "enforced"));
        git(&["rm", "-rq", "vendor/pack"]);
        git(&["commit", "-qm", "remove the pack again"]);
        let report = merge_drift(&repo, "main", &enforced_pin, &paths)
            .expect("check")
            .expect("a vanished pack is drift");
        assert!(report.current_digest.is_none());
        assert!(
            report
                .changed_rules
                .iter()
                .any(|line| line.contains("ZZ-MUST-001") && line.contains("absent")),
            "{:?}",
            report.changed_rules
        );
    }

    #[test]
    fn flight_rules_pin_merge_ignores_mission_branch_pack_edit() {
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("enforced")) else {
            return;
        };
        let cfg = cfg_with_pack(Some("vendor/pack".to_string()));
        let touch = vec!["crates/**".to_string()];
        let pin = approval_pin(&repo, &cfg, &root, "main", None, None, &touch)
            .expect("pin")
            .expect("standards govern");

        // The mission branch edits its pack; the LIVE BASE does not move.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["checkout", "-qb", "kranz/mission-x"]);
        std::fs::write(
            root.join("vendor/pack/standards/RFC-002-slug/rfc.md"),
            rfc_md("RFC-002", "retired"),
        )
        .unwrap();
        std::fs::write(root.join("crates/engine/src/lib.rs"), "pub fn x() {}\n").unwrap_or_else(
            |_| {
                std::fs::create_dir_all(root.join("crates/engine/src")).unwrap();
                std::fs::write(root.join("crates/engine/src/lib.rs"), "pub fn x() {}\n").unwrap();
            },
        );
        git(&["add", "."]);
        git(&["commit", "-qm", "mission work plus a pack edit"]);
        git(&["checkout", "-q", "main"]);

        // Merge resolves the LIVE base policy: the mission's own edit is
        // invisible, so no drift — the mission is judged by the OLD base
        // version, and its edit can govern only future missions (D-E).
        let paths = vec!["crates/engine/src/lib.rs".to_string()];
        assert!(
            merge_drift(&repo, "main", &pin, &paths)
                .expect("check")
                .is_none(),
            "a mission-branch pack edit is not policy drift"
        );
    }

    // ---- the no-pack / old-log regressions ---------------------------------

    #[test]
    fn flight_rules_pin_no_pack_and_old_logs_are_byte_identical() {
        let Some((_tmp, root, repo)) = git_repo_with_files(&vendored_pack_files("approved")) else {
            return;
        };
        // No packDir configured: no pin, no error.
        assert!(
            approval_pin(&repo, &cfg_with_pack(None), &root, "main", None, None, &[])
                .expect("ok")
                .is_none()
        );
        // A schema-2/3 pack (no [standards]): no pin either.
        let schema3 = vec![(
            "vendor/pack/pack.toml".to_string(),
            "[pack]\nname = \"plain\"\nschema = 3\n".to_string(),
        )];
        let Some((_t2, root2, repo2)) = git_repo_with_files(&schema3) else {
            return;
        };
        assert!(
            approval_pin(
                &repo2,
                &cfg_with_pack(Some("vendor/pack".to_string())),
                &root2,
                "main",
                None,
                None,
                &[]
            )
            .expect("ok")
            .is_none(),
            "a schema-3 pack governs no standards"
        );

        // A pin-less Plan serializes WITHOUT the key (old plans stay
        // byte-identical), and an old plan.json without the key folds back
        // with None.
        let plan = crate::types::Plan {
            goal: "g".into(),
            validation_contract: vec![],
            milestones: vec![],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
        };
        let json = serde_json::to_string(&plan).expect("serialize");
        assert!(!json.contains("standardsManifest"), "{json}");
        let old: crate::types::Plan =
            serde_json::from_str(r#"{"goal":"g","validationContract":[],"milestones":[]}"#)
                .expect("an old plan folds");
        assert_eq!(old.standards_manifest, None);
    }

    #[test]
    fn flight_rules_pin_events_round_trip_and_fold() {
        // Both new kinds serialize under their dotted names with the
        // documented payload keys, and fold as audit-only no-ops.
        let resolved = EventKind::StandardsResolved {
            source: "repo-tracked".to_string(),
            pack_name: "zz-pin-pack".to_string(),
            standards_root: "standards".to_string(),
            digest: "ab".repeat(32),
            stage: APPROVAL_SURFACE.to_string(),
            task_class: Some("implementation".to_string()),
            touch_set: vec!["crates/**".to_string()],
            rules: vec![crate::types::StandardsRuleRef {
                id: "ZZ-MUST-001".to_string(),
                revision: 1,
                effective_status: "enforced".to_string(),
            }],
            approval_seq: 7,
        };
        assert_eq!(resolved.type_name(), "standards.resolved");
        let value = serde_json::to_value(&resolved).expect("serialize");
        assert_eq!(value["type"], "standards.resolved");
        assert_eq!(value["payload"]["approvalSeq"], 7);
        let back: EventKind = serde_json::from_value(value).expect("round trip");
        assert_eq!(back.type_name(), "standards.resolved");

        let drifted = EventKind::StandardsDrifted {
            approved_digest: "ab".repeat(32),
            current_digest: None,
            surface: "merge".to_string(),
            changed_rules: vec![
                "ZZ-MUST-001 r1 (newly applicable enforced rule on the live base)".to_string(),
            ],
        };
        assert_eq!(drifted.type_name(), "standards.drifted");
        let value = serde_json::to_value(&drifted).expect("serialize");
        assert_eq!(value["type"], "standards.drifted");
        // currentDigest skipped when None — old consumers see no new key.
        assert!(value["payload"].get("currentDigest").is_none());
        let back: EventKind = serde_json::from_value(value).expect("round trip");
        let EventKind::StandardsDrifted { current_digest, .. } = back else {
            panic!("round trip preserves the variant");
        };
        assert_eq!(current_digest, None);
    }
}
