//! The Flight Rules standards corpus (ticket
//! `.kranz/tickets/flight-rules-pack-contract.md`, KRZ-341; design
//! `docs/scoping/flight-rules-engineering-standards.md`, decisions D-A through
//! D-C, D-F, and D-J): an ADDITIVE schema-4 extension of the pack contract.
//! A pack declaring `[standards] root = "..."` carries a corpus of RFC and
//! rule Markdown files that this module loads into ONE normalized,
//! stable-sorted manifest plus a sha256 content digest, and whose lifecycle
//! transitions `kranz standards lint --against <ref>` checks against the
//! trusted base.
//!
//! Corpus shape (D-A, "Canonical pack shape"):
//!
//! ```text
//! <root>/RFC-014-slug/rfc.md            — one directory per RFC
//! <root>/RFC-014-slug/rules/RULE-ID.md  — flat rule files, one rule each
//! ```
//!
//! WHY a hand-written frontmatter subset and not YAML: the engine's
//! dependency tree has no YAML crate and AGENTS.md prefers existing
//! utilities over new dependencies — the same call `super::toml` made for
//! the manifest. The subset is line-oriented and fully accountable: `key:
//! value` scalars, `key: [a, b]` inline lists, double-quoted strings with
//! `\\`/`\"` escapes, and `#` comments. Everything else — anchors, aliases,
//! tags, block scalars, single-quoted strings, nested/block values, tabs —
//! is REFUSED naming the file and field, never guessed at. Implicit typing
//! is refused field-by-field: integers are digit strings, booleans are
//! exactly `true`/`false`, enums name their vocabulary.
//!
//! WHY the traversal is capability-relative and no-follow (D-J): house
//! standards are prompt input and policy input. The pack dir is the
//! operator-chosen anchor (the same trust basis as [`crate::pack::Pack::load`]'s
//! textFile reads); every parent component is opened `open_dir_nofollow`,
//! every leaf is stat-checked regular and read `FollowSymlinks::No` with a
//! byte cap — a symlinked parent/leaf, a FIFO, a device, an oversized file,
//! or a bloated corpus fails promptly naming the file, and is never
//! followed or read unboundedly. The git-ref source applies the same
//! posture to tracked blobs: `git ls-tree` modes `120000` (symlink) and
//! `160000` (submodule) inside the corpus are load errors.
//!
//! WHY the digest covers exactly these bytes (D-C/D-F): identity comes from
//! stable frontmatter IDs, never paths — renaming a file preserves the
//! digest. The digest hashes the NORMALIZED metadata (every frontmatter
//! field of every RFC/rule, sorted by id, lists sorted and deduplicated)
//! PLUS the declarations of referenced pack gates, so changing a referenced
//! checker changes the standards digest. Markdown prose bodies are NOT
//! hashed: prose is rationale, not a second machine authority — a rationale
//! typo must not churn the digest.
//!
//! WHY the trust boundary (D-A/D-J): an external/untracked pack may supply
//! approved ADVISORY rules but cannot activate ENFORCED rules in this slice
//! — kranz has no base history with which to prove an external pack's
//! lifecycle transitions. [`crate::pack::standards::StandardsTrust::External`] plus an effectively
//! enforced rule is a load error naming the remedy (vendor the pack into
//! the repo as a tracked, repo-relative `packDir`). Repo-relative packs are
//! read from tracked blobs in the pinned base tree where the engine already
//! does that ([`crate::pack::standards::load_at_ref`], mirroring the merge-gate base-read idiom);
//! mission-time approval pinning is the next slice (KRZ-342).
//!
//! Loading and linting parse bytes only — no checker command or source text
//! is ever executed by this module.

use super::{PackGateDecl, PACK_MANIFEST};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Per-file byte cap for one corpus file (`rfc.md` or a rule file). The
/// frontmatter is small by construction; the allowance is for rationale
/// prose. A larger file fails the load naming the file.
pub const MAX_STANDARDS_FILE_BYTES: u64 = 256 * 1024;

/// Cap on the number of corpus files (RFC + rule files together).
pub const MAX_STANDARDS_FILES: usize = 512;

/// Cap on the number of rules in one pack's corpus.
pub const MAX_STANDARDS_RULES: usize = 256;

/// Cap on the total NORMALIZED manifest bytes (the canonical text the
/// digest hashes) — the bound later resolution/prompt budgets rely on.
pub const MAX_STANDARDS_NORMALIZED_BYTES: usize = 1024 * 1024;

/// The first line of the canonical manifest text. Bumping the format is a
/// deliberate, reviewable digest change — old digests simply stop matching.
const CANONICAL_HEADER: &str = "kranz-standards-manifest v1";

/// Whether the pack's bytes carry provable repo history (D-A/D-J). The
/// decision is made by the caller ([`trust_for_dir`] for the lint surfaces,
/// `packDir` absoluteness for mission loads); the loader only enforces the
/// consequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardsTrust {
    /// A tracked, repo-relative pack: enforced rules may activate (their
    /// lifecycle is provable against base history).
    RepoTracked,
    /// An external or untracked pack: approved advisory rules load; an
    /// effectively ENFORCED rule is a load error naming the trust remedy.
    External,
}

/// RFC lifecycle (D-B). Every active contained rule inherits the RFC's
/// status; a rule may only narrow it to `retired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfcStatus {
    Draft,
    Approved,
    Enforced,
    Retired,
}

impl RfcStatus {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "approved" => Some(Self::Approved),
            "enforced" => Some(Self::Enforced),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Approved => "approved",
            Self::Enforced => "enforced",
            Self::Retired => "retired",
        }
    }
}

/// RFC-2119 level of a rule's normative statement. The design's vocabulary
/// is SHOULD/MUST only (D-B's behavior table has no `may` row) — `may` is a
/// load error naming the supported levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleLevel {
    Must,
    Should,
}

impl RuleLevel {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "must" => Some(Self::Must),
            "should" => Some(Self::Should),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Must => "must",
            Self::Should => "should",
        }
    }
}

/// A rule's own declared status (D-B): `active` inherits the parent RFC's
/// lifecycle; `retired` is the immutable one-way tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleStatus {
    Active,
    Retired,
}

impl RuleStatus {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "active" => Some(Self::Active),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
        }
    }
}

/// The workflow stage vocabulary a rule scopes itself to (D-G's projection
/// stages).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleStage {
    Planning,
    Implementation,
    Validation,
    Merge,
}

impl RuleStage {
    /// Parse the canonical stage spelling (`planning`, `implementation`,
    /// `validation`, `merge`). `pub(crate)` for the KRZ-342 resolver, which
    /// re-resolves pinned rules whose stages ride the plan contract as
    /// strings.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "planning" => Some(Self::Planning),
            "implementation" => Some(Self::Implementation),
            "validation" => Some(Self::Validation),
            "merge" => Some(Self::Merge),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Implementation => "implementation",
            Self::Validation => "validation",
            Self::Merge => "merge",
        }
    }
}

/// A rule's typed checker binding (D-F): a registered pack gate, the
/// engine-owned contextual reviewer, or an explicit human decision — never
/// arbitrary executable prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checker {
    /// `gate:<stable-gate-id>` — must name a `[[gate]]` declared by the same
    /// pack; the referenced declaration joins the digest so a checker edit
    /// is byte-visible (D-J's drift row).
    Gate(String),
    /// `agent-judgement` — the engine-owned contextual standards reviewer.
    AgentJudgement,
    /// `manual-attestation` — an explicit authorized human decision.
    ManualAttestation,
}

impl Checker {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "agent-judgement" => Ok(Self::AgentJudgement),
            "manual-attestation" => Ok(Self::ManualAttestation),
            _ => match raw.strip_prefix("gate:") {
                Some(id) if !id.is_empty() => Ok(Self::Gate(id.to_string())),
                _ => Err(format!(
                    "supported checker forms are `gate:<id>` (a declared pack gate), \
                     `agent-judgement`, and `manual-attestation`, got `{raw}`"
                )),
            },
        }
    }

    /// The canonical spelling used in the manifest and reports.
    pub fn render(&self) -> String {
        match self {
            Self::Gate(id) => format!("gate:{id}"),
            Self::AgentJudgement => "agent-judgement".to_string(),
            Self::ManualAttestation => "manual-attestation".to_string(),
        }
    }
}

/// One RFC's normalized governance metadata (`rfc.md` frontmatter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RfcMeta {
    pub id: String,
    pub title: String,
    pub owner: String,
    pub status: RfcStatus,
    /// RFC3339 promotion instant, normalized to UTC seconds (`Z`). Before
    /// this instant a freshly-enforced RFC remains approved in effect (D-B's
    /// absorption window); this slice parses and carries it, later slices
    /// evaluate it.
    pub effective_at: Option<String>,
    /// RFC ids this RFC supersedes (sorted, deduplicated).
    pub supersedes: Vec<String>,
}

/// One rule's normalized metadata (a `rules/*.md` frontmatter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMeta {
    pub id: String,
    /// Positive, monotonic per id (D-C): a semantic change without an
    /// increment is a transition-lint refusal.
    pub revision: u64,
    /// Parent RFC id — validated to exist (no orphan rules).
    pub rfc: String,
    pub level: RuleLevel,
    pub status: RuleStatus,
    /// The one-line normative statement — the canonical machine/human text.
    pub statement: String,
    /// Browsing/reporting labels (D-D: never an LLM-selected policy switch).
    /// Sorted and deduplicated.
    pub domains: Vec<String>,
    /// The stages the rule applies to (sorted by name). Never empty.
    pub stages: Vec<RuleStage>,
    /// Repo-relative path prefixes (normalized); empty means unscoped.
    pub when_paths: Vec<String>,
    /// Mission task classes (free-form, matched by config/routing
    /// normalization); empty means unscoped.
    pub task_classes: Vec<String>,
    /// The typed checker binding. Draft rules may omit it (D-F); promotion
    /// to an approved/enforced EFFECTIVE status requires it (fail-closed).
    pub checker: Option<Checker>,
    /// Waiver posture (D-I): `false` when omitted — fail-closed.
    pub waivable: bool,
}

/// The normalized, stable-sorted product of a standards corpus load: the
/// RFCs and rules (each sorted by id), the declarations of pack gates the
/// rules reference (sorted by name), the canonical text, and its sha256.
/// This is the object later slices pin into the approved mission contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardsManifest {
    /// The normalized pack-relative root from `[standards] root`.
    pub root: String,
    pub rfcs: Vec<RfcMeta>,
    pub rules: Vec<RuleMeta>,
    /// Declarations of the pack gates referenced by `gate:` checkers —
    /// governing bytes, hashed into the digest (D-F).
    pub gate_bindings: Vec<PackGateDecl>,
    /// All pack gates from the same trusted source. Only referenced bindings
    /// participate in the standards digest, but approval pins the full list
    /// so ordinary advisory pack gates also avoid a mission-worktree re-read.
    pub pack_gates: Vec<PackGateDecl>,
    /// Lowercase hex sha256 over [`Self::canonical_text`].
    pub digest: String,
    canonical: String,
}

impl StandardsManifest {
    /// The normalized governing bytes the digest hashes: every frontmatter
    /// field of every RFC/rule (sorted by id, lists sorted/deduplicated),
    /// plus the referenced pack gate declarations. Prose bodies and file
    /// paths are deliberately absent (D-C).
    pub fn canonical_text(&self) -> &str {
        &self.canonical
    }

    /// The rule with this id, if present.
    pub fn rule(&self, id: &str) -> Option<&RuleMeta> {
        self.rules.iter().find(|r| r.id == id)
    }

    /// The RFC with this id, if present.
    pub fn rfc(&self, id: &str) -> Option<&RfcMeta> {
        self.rfcs.iter().find(|r| r.id == id)
    }

    /// A rule's EFFECTIVE lifecycle (D-B): its own tombstone wins; an active
    /// rule inherits its parent RFC's status. The parent always exists in a
    /// loaded manifest (orphans are load errors); the defensive fallback is
    /// `Retired`, the status that can never block anything.
    pub fn effective_status(&self, rule: &RuleMeta) -> RfcStatus {
        if rule.status == RuleStatus::Retired {
            return RfcStatus::Retired;
        }
        self.rfc(&rule.rfc)
            .map(|rfc| rfc.status)
            .unwrap_or(RfcStatus::Retired)
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Load the standards corpus of a filesystem pack: a capability-relative,
/// no-follow walk of `<pack_dir>/<root>`. Called by
/// [`super::Pack::from_document`] when the manifest declares `[standards]`,
/// so every consuming surface sees the same fully-accounted pack.
pub(crate) fn load_from_pack_dir(
    pack_dir: &Path,
    root: &str,
    gates: &[PackGateDecl],
    trust: StandardsTrust,
) -> Result<StandardsManifest, String> {
    use cap_fs_ext::DirExt as _;

    let display_root = pack_dir.join(root);
    let mut dir = cap_std::fs::Dir::open_ambient_dir(pack_dir, cap_std::ambient_authority())
        .map_err(|e| format!("cannot open pack dir {}: {e}", pack_dir.display()))?;
    // `root` reaches here normalized (plain Normal components), so splitting
    // on '/' yields plain names — the same idiom as the textFile reader.
    for name in root.split('/') {
        dir = dir.open_dir_nofollow(name).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "[standards] root `{root}` does not exist in the pack ({})",
                    display_root.display()
                )
            } else {
                format!(
                    "[standards] root `{root}` resolves through a symlinked or non-directory \
                     component ({}) — the standards corpus loads no-follow",
                    display_root.display()
                )
            }
        })?;
    }
    let source = FsSource {
        root_dir: dir,
        display_root,
    };
    load_from_source(&source, root, gates, trust)
}

/// Load the standards corpus as of a base git ref: `pack.toml` and every
/// governing byte come from TRACKED BLOBS at `<ref>` (`git show` /
/// `git ls-tree`), never the filesystem worktree (D-A) — a mission branch
/// edit cannot reshape the policy judging it. `pack_rel_dir` is the pack's
/// repo-relative slash path (`""` when the pack sits at the repo root).
/// `Ok(None)` means the ref has no pack manifest or the pack declares no
/// standards root — the base simply had no standards.
pub fn load_at_ref(
    repo: &crate::git_ops::GitRepo,
    refname: &str,
    pack_rel_dir: &str,
) -> Result<Option<StandardsManifest>, String> {
    let oid = repo
        .rev_parse(refname)
        .map_err(|e| format!("cannot resolve ref `{refname}`: {e}"))?;
    let manifest_rel = join_rel(pack_rel_dir, PACK_MANIFEST);
    let Some(bytes) = repo
        .show_file(&oid, &manifest_rel)
        .map_err(|e| format!("cannot read {manifest_rel} at `{refname}`: {e}"))?
    else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{manifest_rel} at `{refname}` is not valid UTF-8"))?;
    let doc =
        super::toml::parse(&text).map_err(|e| format!("{manifest_rel} at `{refname}`: {e}"))?;
    // The base manifest is validated through the SAME strict helpers as a
    // worktree load — a base the engine cannot fully account for fails
    // closed rather than silently comparing against a partial read.
    let (_name, schema) =
        super::manifest_header(&doc).map_err(|e| format!("{manifest_rel} at `{refname}`: {e}"))?;
    let Some(root) = super::standards_root_of(&doc, schema)
        .map_err(|e| format!("{manifest_rel} at `{refname}`: {e}"))?
    else {
        return Ok(None);
    };
    // Checker bindings are base-owned too: the gate declarations come from
    // the same tracked pack.toml, never the worktree.
    let mut gates = Vec::new();
    for (idx, item) in doc.array("gate").iter().enumerate() {
        gates.push(
            super::load_gate(item, idx)
                .map_err(|e| format!("{manifest_rel} at `{refname}`: {e}"))?,
        );
    }
    super::reject_duplicate_names("gate", gates.iter().map(|g| g.name.as_str()))
        .map_err(|e| format!("{manifest_rel} at `{refname}`: {e}"))?;
    let prefix = join_rel(pack_rel_dir, &root);
    let source = GitSource {
        repo,
        oid: &oid,
        refname,
        prefix,
    };
    // Base bytes are tracked by construction — RepoTracked is descriptive,
    // not a decision.
    load_from_source(&source, &root, &gates, StandardsTrust::RepoTracked).map(Some)
}

/// The trust level a pack directory earns from its relationship to the repo
/// (D-A/D-J): inside the repo with a TRACKED `pack.toml` ⇒
/// [`StandardsTrust::RepoTracked`]; anything else — outside the repo,
/// inside but untracked, not a git repo, any lookup failure — fails closed
/// to [`StandardsTrust::External`] (advisory-only).
pub fn trust_for_dir(repo_root: &Path, pack_dir: &Path) -> StandardsTrust {
    let Some(rel) = repo_relative_dir(repo_root, pack_dir) else {
        return StandardsTrust::External;
    };
    let manifest_rel = join_rel(&rel, PACK_MANIFEST);
    let Ok(repo) = crate::git_ops::GitRepo::open(repo_root) else {
        return StandardsTrust::External;
    };
    match repo.is_tracked(&manifest_rel) {
        Ok(true) => StandardsTrust::RepoTracked,
        _ => StandardsTrust::External,
    }
}

/// The repo-relative slash path of `dir` when it sits inside `repo_root`
/// (`""` for the repo root itself), `None` when outside. Both sides are
/// canonicalized first so symlinked prefixes (macOS `/tmp` → `/private/...`)
/// cannot fake or defeat the containment check.
pub fn repo_relative_dir(repo_root: &Path, dir: &Path) -> Option<String> {
    let repo_c = std::fs::canonicalize(repo_root).ok()?;
    let dir_c = std::fs::canonicalize(dir).ok()?;
    let rel = dir_c.strip_prefix(&repo_c).ok()?;
    let mut out = String::new();
    for part in rel.components() {
        let std::path::Component::Normal(name) = part else {
            return None;
        };
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(name.to_str()?);
    }
    Some(out)
}

/// Join a repo-relative directory (possibly empty) and a file name into a
/// slash path git pathspecs understand.
fn join_rel(dir: &str, leaf: &str) -> String {
    if dir.is_empty() {
        leaf.to_string()
    } else {
        format!("{dir}/{leaf}")
    }
}

// ---------------------------------------------------------------------------
// Lifecycle transition lint (D-B/D-C)
// ---------------------------------------------------------------------------

/// The lifecycle transition check behind `kranz standards lint --against
/// <ref>`: compare the PROPOSED manifest against the trusted BASE (`None` —
/// the base ref had no standards) and return every violation, each naming
/// the rule/RFC and the refused transition. An empty vec is "transitions
/// clean". The four refused classes are D-B/D-C's:
///
/// - absent/draft → enforced (RFC or rule): blocking policy must absorb an
///   approved advisory period first;
/// - a semantic rule change (statement, level, stages, when-paths,
///   task-classes, checker, waivable — D-C's list) without a revision
///   increment; revisions also never move backwards;
/// - disappearance of a rule ID known at the base (retire, never delete);
/// - tombstone reactivation (a rule retired at the base stays retired).
pub fn check_transitions(
    base: Option<&StandardsManifest>,
    proposed: &StandardsManifest,
) -> Vec<String> {
    let mut errors = Vec::new();

    for rfc in proposed
        .rfcs
        .iter()
        .filter(|r| r.status == RfcStatus::Enforced)
    {
        match base.and_then(|b| b.rfc(&rfc.id)) {
            None => errors.push(format!(
                "RFC `{}` is enforced but absent at the base — an RFC may not move \
                 absent/draft → enforced; land it approved first so the advisory period \
                 produces real evidence (D-B)",
                rfc.id
            )),
            Some(base_rfc) if base_rfc.status == RfcStatus::Draft => errors.push(format!(
                "RFC `{}` is enforced but was draft at the base — absent/draft → enforced \
                 is refused; promote through approved first (D-B)",
                rfc.id
            )),
            Some(base_rfc) if base_rfc.status == RfcStatus::Retired => errors.push(format!(
                "RFC `{}` is enforced but was retired at the base — a tombstone is one-way \
                 (D-B/D-C)",
                rfc.id
            )),
            Some(_) => {}
        }
    }

    for rule in &proposed.rules {
        if proposed.effective_status(rule) != RfcStatus::Enforced {
            continue;
        }
        let base_effective = match base {
            Some(base) => base
                .rule(&rule.id)
                .map(|base_rule| base.effective_status(base_rule)),
            None => None,
        };
        match base_effective {
            Some(RfcStatus::Approved | RfcStatus::Enforced) => {}
            // A reactivated tombstone is already reported below — one error
            // per violation class is enough.
            Some(RfcStatus::Retired) => {}
            Some(RfcStatus::Draft) => errors.push(format!(
                "rule `{}` is enforced but was draft at the base — absent/draft → enforced \
                 is refused; an approved advisory period comes first (D-B)",
                rule.id
            )),
            None => errors.push(format!(
                "rule `{}` is enforced but absent at the base — new rules enter as draft or \
                 approved, never directly enforced (D-B)",
                rule.id
            )),
        }
    }

    let Some(base) = base else {
        return errors;
    };
    for base_rule in &base.rules {
        let Some(proposed_rule) = proposed.rule(&base_rule.id) else {
            errors.push(format!(
                "rule `{}` (base revision {}) is gone — known rule IDs cannot disappear; \
                 retire the rule as a one-way tombstone instead of deleting it (D-C)",
                base_rule.id, base_rule.revision
            ));
            continue;
        };
        if base_rule.status == RuleStatus::Retired && proposed_rule.status == RuleStatus::Active {
            errors.push(format!(
                "rule `{}` was retired at the base and cannot be reactivated — retirement \
                 is a one-way tombstone; a successor rule needs a new ID (D-B/D-C)",
                base_rule.id
            ));
            continue;
        }
        if base_rule.status == RuleStatus::Active && proposed_rule.status == RuleStatus::Active {
            if proposed_rule.revision < base_rule.revision {
                errors.push(format!(
                    "rule `{}` revision moved backwards ({} → {}) — revisions are monotonic \
                     (D-C)",
                    base_rule.id, base_rule.revision, proposed_rule.revision
                ));
            } else if proposed_rule.revision == base_rule.revision {
                if let Some(field) = semantic_change(base_rule, proposed_rule) {
                    errors.push(format!(
                        "rule `{}` changed `{field}` without a revision increment (still \
                         {}) — a semantic change to statement, level, scope, checker, or \
                         waiver posture requires a bump (D-C)",
                        base_rule.id, base_rule.revision
                    ));
                }
            }
        }
    }
    errors
}

/// The first differing semantic field between two revisions of one rule, or
/// None. The field list is D-C's: statement, level, scope (stages,
/// when-paths, task-classes), checker, waiver posture. Domains are browsing
/// labels (D-D) and the parent-RFC link is lifecycle, not rule semantics —
/// neither forces a bump here.
fn semantic_change(base: &RuleMeta, proposed: &RuleMeta) -> Option<&'static str> {
    if base.statement != proposed.statement {
        Some("statement")
    } else if base.level != proposed.level {
        Some("level")
    } else if base.stages != proposed.stages {
        Some("stages")
    } else if base.when_paths != proposed.when_paths {
        Some("when-paths")
    } else if base.task_classes != proposed.task_classes {
        Some("task-classes")
    } else if base.checker != proposed.checker {
        Some("checker")
    } else if base.waivable != proposed.waivable {
        Some("waivable")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

/// The standards block of `kranz pack lint`: the registration summary —
/// root, digest, and lifecycle tallies — for a schema-4 pack.
pub fn render_registration(manifest: &StandardsManifest) -> String {
    let tally = |status: RfcStatus| manifest.rfcs.iter().filter(|r| r.status == status).count();
    let active_rules = manifest
        .rules
        .iter()
        .filter(|r| r.status == RuleStatus::Active)
        .count();
    format!(
        "standards (schema {} root `{}`):\n  digest: sha256:{}\n  RFCs: {} (draft {}, approved \
         {}, enforced {}, retired {}); rules: {} (active {}, retired {}); gate bindings: {}\n",
        super::SCHEMA_STANDARDS,
        manifest.root,
        manifest.digest,
        manifest.rfcs.len(),
        tally(RfcStatus::Draft),
        tally(RfcStatus::Approved),
        tally(RfcStatus::Enforced),
        tally(RfcStatus::Retired),
        manifest.rules.len(),
        active_rules,
        manifest.rules.len() - active_rules,
        manifest.gate_bindings.len(),
    )
}

/// The headline of `kranz standards lint`: the full normalized manifest —
/// every RFC and rule with its effective status, checker binding, and
/// scopes — plus the digest and the trust posture the loader applied.
pub fn render_manifest(manifest: &StandardsManifest, trust: StandardsTrust) -> String {
    let mut out = format!(
        "standards root `{}` — {} RFC(s), {} rule(s)\ndigest: sha256:{}\n",
        manifest.root,
        manifest.rfcs.len(),
        manifest.rules.len(),
        manifest.digest
    );
    out.push_str(match trust {
        StandardsTrust::RepoTracked => "trust: repo-tracked — enforced rules may activate\n",
        StandardsTrust::External => {
            "trust: external/untracked — advisory only; enforced rules are refused at load \
             (D-A/D-J)\n"
        }
    });
    out.push_str("RFCs:\n");
    if manifest.rfcs.is_empty() {
        out.push_str("  (none)\n");
    }
    for rfc in &manifest.rfcs {
        let effective = rfc
            .effective_at
            .as_deref()
            .map(|ts| format!(", effective {ts}"))
            .unwrap_or_default();
        let supersedes = if rfc.supersedes.is_empty() {
            String::new()
        } else {
            format!(", supersedes {}", rfc.supersedes.join(", "))
        };
        out.push_str(&format!(
            "  - {} \"{}\" — {}, owner {}{}{}\n",
            rfc.id,
            rfc.title,
            rfc.status.as_str(),
            rfc.owner,
            effective,
            supersedes
        ));
    }
    out.push_str("rules:\n");
    if manifest.rules.is_empty() {
        out.push_str("  (none)\n");
    }
    for rule in &manifest.rules {
        let checker = rule
            .checker
            .as_ref()
            .map(Checker::render)
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "  - {} r{} — {}, {}; checker {}; waivable: {}\n      statement: {}\n",
            rule.id,
            rule.revision,
            rule.level.as_str(),
            manifest.effective_status(rule).as_str(),
            checker,
            rule.waivable,
            rule.statement
        ));
        let list = |items: &[String]| {
            if items.is_empty() {
                "-".to_string()
            } else {
                items.join(", ")
            }
        };
        let stages = rule
            .stages
            .iter()
            .map(RuleStage::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "      stages: {}; domains: {}; when-paths: {}; task-classes: {}\n",
            stages,
            list(&rule.domains),
            list(&rule.when_paths),
            list(&rule.task_classes)
        ));
    }
    out
}

/// The `--against <ref>` section of `kranz standards lint`: what the base
/// held and every refused transition (empty ⇒ clean).
pub fn render_transition_report(
    refname: &str,
    base: Option<&StandardsManifest>,
    errors: &[String],
) -> String {
    let base_desc = match base {
        Some(base) => format!(
            "base digest sha256:{}, {} RFC(s), {} rule(s)",
            base.digest,
            base.rfcs.len(),
            base.rules.len()
        ),
        None => "no standards at the base ref".to_string(),
    };
    let mut out = format!("transition check against `{refname}` ({base_desc}):\n");
    if errors.is_empty() {
        out.push_str("  ok — no lifecycle violations\n");
    } else {
        for error in errors {
            out.push_str(&format!("  REFUSED: {error}\n"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Corpus sources: the two D-A byte origins, one validation path
// ---------------------------------------------------------------------------

/// One listed corpus file: root-relative slash path, byte size, and the
/// display name errors quote (a filesystem path or `<ref>:<path>`).
struct SourceFile {
    rel: String,
    size: u64,
    display: String,
}

/// A governing-bytes source. Both shapes apply the same fail-closed posture:
/// regular files only, hostile shapes named and refused at LISTING time,
/// per-file caps checked before (and while) reading.
trait CorpusSource {
    /// Every regular file under the standards root, sorted by rel path.
    fn list_files(&self) -> Result<Vec<SourceFile>, String>;
    /// The bytes of one listed file (cap re-checked while reading).
    fn read_bytes(&self, file: &SourceFile) -> Result<Vec<u8>, String>;
}

/// The worktree/external source: a capability-relative, no-follow walk
/// under the pack dir anchor (D-J).
struct FsSource {
    /// The standards root directory, opened no-follow from the anchor.
    root_dir: cap_std::fs::Dir,
    display_root: PathBuf,
}

impl CorpusSource for FsSource {
    fn list_files(&self) -> Result<Vec<SourceFile>, String> {
        use cap_fs_ext::DirExt as _;

        let mut out = Vec::new();
        for (name, ftype) in sorted_entries(&self.root_dir, &self.display_root)? {
            let display = self.display_root.join(&name);
            check_entry_name(&name, &display)?;
            if ftype.is_symlink() {
                return Err(format!(
                    "{} resolves through a symlink — the standards corpus never follows \
                     symlinks (D-J)",
                    display.display()
                ));
            }
            if !ftype.is_dir() {
                return Err(format!(
                    "{} is not an RFC directory — the standards root holds one directory \
                     per RFC, nothing else",
                    display.display()
                ));
            }
            let rfc_dir = self.root_dir.open_dir_nofollow(&name).map_err(|_| {
                format!(
                    "{} resolves through a symlinked or non-directory component — the \
                     standards corpus never follows symlinks (D-J)",
                    display.display()
                )
            })?;
            list_rfc_dir(&rfc_dir, &name, &display, &mut out)?;
        }
        Ok(out)
    }

    fn read_bytes(&self, file: &SourceFile) -> Result<Vec<u8>, String> {
        use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
        use std::io::Read as _;

        let mut dir = self
            .root_dir
            .try_clone()
            .map_err(|e| format!("{} cannot be opened: {e}", file.display))?;
        let mut names = file.rel.split('/').peekable();
        while let Some(name) = names.next() {
            if names.peek().is_some() {
                dir = dir.open_dir_nofollow(name).map_err(|_| {
                    format!(
                        "{} resolves through a symlinked or non-directory component — the \
                         standards corpus never follows symlinks (D-J)",
                        file.display
                    )
                })?;
                continue;
            }
            // Stat BEFORE opening: a FIFO would block an O_RDONLY open
            // forever waiting for a writer — the refusal must be prompt.
            let meta = dir
                .symlink_metadata(name)
                .map_err(|e| format!("{} cannot be stat'ed: {e}", file.display))?;
            let ftype = meta.file_type();
            if ftype.is_symlink() {
                return Err(format!(
                    "{} is a symlink — the standards corpus never follows symlinks (D-J)",
                    file.display
                ));
            }
            if !ftype.is_file() {
                return Err(format!(
                    "{} is not a regular file (FIFO/device/socket) — the standards corpus \
                     accepts regular files only (D-J)",
                    file.display
                ));
            }
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let opened = dir
                .open_with(name, &options)
                .map_err(|e| format!("{} cannot be read: {e}", file.display))?;
            let mut bytes = Vec::new();
            opened
                .take(MAX_STANDARDS_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("{} cannot be read: {e}", file.display))?;
            if bytes.len() as u64 > MAX_STANDARDS_FILE_BYTES {
                return Err(format!(
                    "{} is {} bytes, over the {}-byte per-file cap",
                    file.display,
                    bytes.len(),
                    MAX_STANDARDS_FILE_BYTES
                ));
            }
            return Ok(bytes);
        }
        // Unreachable: rel paths are non-empty by construction — fail closed
        // rather than panic if that ever changes.
        Err(format!("{} resolves to no file", file.display))
    }
}

/// One RFC directory's listing: its `rfc.md` plus its rule files.
fn list_rfc_dir(
    dir: &cap_std::fs::Dir,
    rel_prefix: &str,
    display: &Path,
    out: &mut Vec<SourceFile>,
) -> Result<(), String> {
    use cap_fs_ext::DirExt as _;

    for (name, ftype) in sorted_entries(dir, display)? {
        let entry_display = display.join(&name);
        if ftype.is_symlink() {
            return Err(format!(
                "{} resolves through a symlink — the standards corpus never follows \
                 symlinks (D-J)",
                entry_display.display()
            ));
        }
        if name == "rfc.md" {
            if !ftype.is_file() {
                return Err(format!(
                    "{} must be a regular file",
                    entry_display.display()
                ));
            }
            out.push(SourceFile {
                rel: format!("{rel_prefix}/rfc.md"),
                size: dir.symlink_metadata(&name).map(|m| m.len()).unwrap_or(0),
                display: entry_display.display().to_string(),
            });
            continue;
        }
        if name == "rules" {
            if !ftype.is_dir() {
                return Err(format!(
                    "{} must be a directory holding rule files",
                    entry_display.display()
                ));
            }
            let rules_dir = dir.open_dir_nofollow(&name).map_err(|_| {
                format!(
                    "{} resolves through a symlinked or non-directory component — the \
                     standards corpus never follows symlinks (D-J)",
                    entry_display.display()
                )
            })?;
            for (rule_name, rule_ftype) in sorted_entries(&rules_dir, &entry_display)? {
                let rule_display = entry_display.join(&rule_name);
                if rule_ftype.is_symlink() {
                    return Err(format!(
                        "{} resolves through a symlink — the standards corpus never \
                         follows symlinks (D-J)",
                        rule_display.display()
                    ));
                }
                if rule_ftype.is_dir() {
                    return Err(format!(
                        "{} is a directory — rules/ holds rule Markdown files only, no \
                         nested directories",
                        rule_display.display()
                    ));
                }
                if !rule_ftype.is_file() {
                    return Err(format!(
                        "{} is not a regular file (FIFO/device/socket) — the standards \
                         corpus accepts regular files only (D-J)",
                        rule_display.display()
                    ));
                }
                if !rule_name.ends_with(".md") {
                    return Err(format!(
                        "{} is not a `.md` rule file — rules/ holds rule Markdown files \
                         only",
                        rule_display.display()
                    ));
                }
                check_entry_name(&rule_name, &rule_display)?;
                out.push(SourceFile {
                    rel: format!("{rel_prefix}/rules/{rule_name}"),
                    size: rules_dir
                        .symlink_metadata(&rule_name)
                        .map(|m| m.len())
                        .unwrap_or(0),
                    display: rule_display.display().to_string(),
                });
            }
            continue;
        }
        return Err(format!(
            "{} is unexpected — an RFC directory holds `rfc.md` and `rules/`, nothing else",
            entry_display.display()
        ));
    }
    Ok(())
}

/// The pinned-base source: tracked blobs at one resolved git OID (D-A).
struct GitSource<'a> {
    repo: &'a crate::git_ops::GitRepo,
    oid: &'a str,
    /// The user-facing ref name, for error text.
    refname: &'a str,
    /// Repo-relative slash path of the standards root.
    prefix: String,
}

impl CorpusSource for GitSource<'_> {
    fn list_files(&self) -> Result<Vec<SourceFile>, String> {
        let entries = self
            .repo
            .ls_tree_recursive(self.oid, &self.prefix)
            .map_err(|e| format!("cannot list the standards root at `{}`: {e}", self.refname))?;
        if entries.is_empty() {
            return Err(format!(
                "[standards] root is declared but no tracked files exist under `{}` at \
                 `{}`",
                self.prefix, self.refname
            ));
        }
        let mut out = Vec::new();
        for entry in entries {
            let display = format!("{}:{}", self.refname, entry.path);
            if entry.path == self.prefix {
                return Err(format!(
                    "{display} is a file, not a directory tree — the standards root must \
                     be a directory"
                ));
            }
            let Some(rel) = entry.path.strip_prefix(&format!("{}/", self.prefix)) else {
                return Err(format!(
                    "git ls-tree reported {display} outside the standards root `{}`",
                    self.prefix
                ));
            };
            if rel.starts_with('"') {
                return Err(format!(
                    "{display} needed git quoting — corpus names stay inside ASCII \
                     alphanumerics, `.`, `_`, `-`"
                ));
            }
            // The no-follow posture applied to tracked bytes (D-J): a git
            // tree can carry symlinks (mode 120000) and submodules (mode
            // 160000); both are refused, never followed.
            if entry.mode == "120000" {
                return Err(format!(
                    "{display} is a tracked symlink — the standards corpus never follows \
                     symlinks (D-J)"
                ));
            }
            if entry.kind != "blob" {
                return Err(format!(
                    "{display} is a {} (mode {}) — the standards corpus accepts regular \
                     files only (D-J)",
                    entry.kind, entry.mode
                ));
            }
            let size = entry.size.unwrap_or(0);
            validate_corpus_rel(rel, &display)?;
            out.push(SourceFile {
                rel: rel.to_string(),
                size,
                display,
            });
        }
        Ok(out)
    }

    fn read_bytes(&self, file: &SourceFile) -> Result<Vec<u8>, String> {
        let path = format!("{}/{}", self.prefix, file.rel);
        let bytes = self
            .repo
            .show_file(self.oid, &path)
            .map_err(|e| format!("{} cannot be read: {e}", file.display))?
            .ok_or_else(|| {
                format!(
                    "{} vanished between listing and read — refusing to continue",
                    file.display
                )
            })?;
        if bytes.len() as u64 > MAX_STANDARDS_FILE_BYTES {
            return Err(format!(
                "{} is {} bytes, over the {MAX_STANDARDS_FILE_BYTES}-byte per-file cap",
                file.display,
                bytes.len()
            ));
        }
        Ok(bytes)
    }
}

/// The entries of one capability dir as (name, no-follow file type) pairs,
/// sorted by name for deterministic error and load order.
fn sorted_entries(
    dir: &cap_std::fs::Dir,
    display: &Path,
) -> Result<Vec<(String, cap_std::fs::FileType)>, String> {
    let mut out = Vec::new();
    let entries = dir
        .entries()
        .map_err(|e| format!("cannot list {}: {e}", display.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list {}: {e}", display.display()))?;
        let name = entry.file_name().into_string().map_err(|_| {
            format!(
                "{} holds a non-UTF-8 file name — the standards corpus requires UTF-8 names",
                display.display()
            )
        })?;
        // symlink_metadata: the type of the ENTRY ITSELF, never its target.
        let meta = dir
            .symlink_metadata(&name)
            .map_err(|e| format!("{} cannot be stat'ed: {e}", display.join(&name).display()))?;
        out.push((name, meta.file_type()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The corpus name charset: ASCII alphanumerics plus `.`, `_`, `-`, never
/// dot-leading (hidden files have no place in a policy corpus — a stray
/// `.DS_Store` fails the load rather than being quietly skipped).
fn check_entry_name(name: &str, display: &Path) -> Result<(), String> {
    if name.starts_with('.')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "{}: corpus entry names stay inside ASCII alphanumerics, `.`, `_`, `-` and \
             never start with `.`",
            display.display()
        ));
    }
    Ok(())
}

/// The git-source shape check for one root-relative path: `<dir>/rfc.md` or
/// `<dir>/rules/<RULE>.md` — anything else is named and refused, exactly
/// like the filesystem walk refuses unexpected shapes.
fn validate_corpus_rel(rel: &str, display: &str) -> Result<(), String> {
    let parts: Vec<&str> = rel.split('/').collect();
    for part in &parts {
        if part.starts_with('.')
            || part.is_empty()
            || !part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(format!(
                "{display}: corpus entry names stay inside ASCII alphanumerics, `.`, `_`, \
                 `-` and never start with `.`"
            ));
        }
    }
    let well_formed = match parts.as_slice() {
        [_dir, file] => *file == "rfc.md",
        [_dir, rules, file] => *rules == "rules" && file.ends_with(".md"),
        _ => false,
    };
    if !well_formed {
        return Err(format!(
            "{display}: unexpected path shape — the corpus holds `<RFC-dir>/rfc.md` and \
             `<RFC-dir>/rules/<rule>.md` only"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The load pipeline: list → shape → parse → validate → normalize → digest
// ---------------------------------------------------------------------------

/// Load one corpus from either source into the normalized manifest. Every
/// failure names the offending file/field; every cap fails promptly.
fn load_from_source<S: CorpusSource>(
    source: &S,
    root: &str,
    gates: &[PackGateDecl],
    trust: StandardsTrust,
) -> Result<StandardsManifest, String> {
    let mut listing = source.list_files()?;
    listing.sort_by(|a, b| a.rel.cmp(&b.rel));
    if listing.len() > MAX_STANDARDS_FILES {
        return Err(format!(
            "[standards] root `{root}` holds {} files, over the {}-file cap",
            listing.len(),
            MAX_STANDARDS_FILES
        ));
    }
    // The per-file cap fails at LISTING time (before any byte is read);
    // `read_bytes` re-checks while reading, closing the grow-after-stat race.
    for file in &listing {
        if file.size > MAX_STANDARDS_FILE_BYTES {
            return Err(format!(
                "{} is {} bytes, over the {}-byte per-file cap",
                file.display, file.size, MAX_STANDARDS_FILE_BYTES
            ));
        }
    }
    // Group by RFC directory (sorted iteration via BTreeMap): each must
    // carry exactly one rfc.md; rules/ files group under it.
    let mut groups: BTreeMap<String, (Option<&SourceFile>, Vec<&SourceFile>)> = BTreeMap::new();
    for file in &listing {
        let parts: Vec<&str> = file.rel.split('/').collect();
        let (dir, is_rfc) = match parts.as_slice() {
            [dir, name] if *name == "rfc.md" => ((*dir).to_string(), true),
            [dir, rules, name] if *rules == "rules" && name.ends_with(".md") => {
                ((*dir).to_string(), false)
            }
            _ => {
                return Err(format!(
                    "{}: unexpected path shape — the corpus holds `<RFC-dir>/rfc.md` and \
                     `<RFC-dir>/rules/<rule>.md` only",
                    file.display
                ))
            }
        };
        let group = groups.entry(dir).or_insert_with(|| (None, Vec::new()));
        if is_rfc {
            if group.0.is_some() {
                return Err(format!(
                    "{}: duplicate rfc.md in one RFC directory",
                    file.display
                ));
            }
            group.0 = Some(file);
        } else {
            group.1.push(file);
        }
    }

    let mut rfcs = Vec::new();
    let mut rules = Vec::new();
    for (dir, (rfc_file, rule_files)) in &groups {
        let Some(rfc_file) = rfc_file else {
            return Err(format!(
                "standards RFC directory `{dir}` has rules but no rfc.md ({})",
                rule_files[0].display
            ));
        };
        let text = read_text(source, rfc_file)?;
        let fm = parse_frontmatter(&text, &rfc_file.display)?;
        rfcs.push(load_rfc(&fm, &rfc_file.display)?);
        for rule_file in rule_files {
            if rules.len() >= MAX_STANDARDS_RULES {
                return Err(format!(
                    "{}: the rule count exceeds the {}-rule cap",
                    rule_file.display, MAX_STANDARDS_RULES
                ));
            }
            let text = read_text(source, rule_file)?;
            let fm = parse_frontmatter(&text, &rule_file.display)?;
            let rule = load_rule(&fm, &rule_file.display)?;
            // A declared checker resolves against this pack's gates HERE,
            // where the file is known, so the refusal names the file (D-F).
            if let Some(Checker::Gate(id)) = &rule.checker {
                if !gates.iter().any(|g| &g.name == id) {
                    return Err(format!(
                        "{}: field `checker`: rule `{}` checker `gate:{id}` names no \
                         declared [[gate]] in this pack — the checker must resolve to a \
                         pack gate at load (D-F)",
                        rule_file.display, rule.id
                    ));
                }
            }
            rules.push(rule);
        }
    }
    assemble(root, rfcs, rules, gates, trust)
}

/// Read one file as UTF-8 text (per-file cap re-checked inside the source).
fn read_text<S: CorpusSource>(source: &S, file: &SourceFile) -> Result<String, String> {
    let bytes = source.read_bytes(file)?;
    String::from_utf8(bytes).map_err(|_| {
        format!(
            "{} is not valid UTF-8 — corpus files are UTF-8 text",
            file.display
        )
    })
}

/// Cross-file validation, normalization, and the digest (D-C/D-F).
fn assemble(
    root: &str,
    mut rfcs: Vec<RfcMeta>,
    mut rules: Vec<RuleMeta>,
    gates: &[PackGateDecl],
    trust: StandardsTrust,
) -> Result<StandardsManifest, String> {
    // One pack-wide ID namespace (D-C): an RFC and a rule sharing an ID is
    // as much a collision as two rules sharing one.
    let mut ids = HashSet::new();
    for rfc in &rfcs {
        if !ids.insert(rfc.id.as_str()) {
            return Err(format!(
                "duplicate standards id `{}` — RFC and rule IDs are pack-wide unique (D-C)",
                rfc.id
            ));
        }
    }
    for rule in &rules {
        if !ids.insert(rule.id.as_str()) {
            return Err(format!(
                "duplicate standards id `{}` — RFC and rule IDs are pack-wide unique (D-C)",
                rule.id
            ));
        }
    }

    let status_of = |id: &str| rfcs.iter().find(|r| r.id == id).map(|r| r.status);
    let mut gate_bindings: BTreeMap<&str, &PackGateDecl> = BTreeMap::new();
    for rule in &rules {
        let Some(rfc_status) = status_of(&rule.rfc) else {
            return Err(format!(
                "rule `{}` names parent RFC `{}`, which does not exist in this pack — \
                 orphan rules fail the load (D-C)",
                rule.id, rule.rfc
            ));
        };
        let effective = if rule.status == RuleStatus::Retired {
            RfcStatus::Retired
        } else {
            rfc_status
        };
        // D-F: a checker that is DECLARED must resolve; a rule whose
        // effective status is approved/enforced must declare one (the
        // advisory period is mechanically evaluable). Drafts may omit it.
        if let Some(Checker::Gate(id)) = &rule.checker {
            let Some(gate) = gates.iter().find(|g| &g.name == id) else {
                return Err(format!(
                    "rule `{}` checker `gate:{id}` names no declared [[gate]] in this pack \
                     — the checker must resolve to a pack gate at load (D-F)",
                    rule.id
                ));
            };
            gate_bindings.insert(gate.name.as_str(), gate);
        }
        if matches!(effective, RfcStatus::Approved | RfcStatus::Enforced) && rule.checker.is_none()
        {
            return Err(format!(
                "rule `{}` is effectively {} (RFC `{}` is {}) but declares no checker — \
                 promotion to approved requires a valid typed binding; only draft rules \
                 may omit one (D-F)",
                rule.id,
                effective.as_str(),
                rule.rfc,
                rfc_status.as_str()
            ));
        }
        // D-A/D-J: blocking policy requires provable base history.
        if trust == StandardsTrust::External && effective == RfcStatus::Enforced {
            return Err(format!(
                "rule `{}` is effectively enforced but this pack is external/untracked — \
                 an external pack may supply approved advisory rules, never enforced ones, \
                 in this slice (D-A/D-J). Remedy: vendor the pack into the repo as a \
                 tracked, repo-relative packDir so its lifecycle is provable from base \
                 history",
                rule.id
            ));
        }
    }

    rfcs.sort_by(|a, b| a.id.cmp(&b.id));
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    let gate_bindings: Vec<PackGateDecl> =
        gate_bindings.values().map(|gate| (*gate).clone()).collect();
    let canonical = canonical_text(&rfcs, &rules, &gate_bindings);
    if canonical.len() > MAX_STANDARDS_NORMALIZED_BYTES {
        return Err(format!(
            "the normalized standards manifest is {} bytes, over the {}-byte cap",
            canonical.len(),
            MAX_STANDARDS_NORMALIZED_BYTES
        ));
    }
    let digest = Sha256::digest(canonical.as_bytes());
    let digest = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Ok(StandardsManifest {
        root: root.to_string(),
        rfcs,
        rules,
        gate_bindings,
        pack_gates: gates.to_vec(),
        digest,
        canonical,
    })
}

/// The canonical normalized bytes (D-C): fixed field order, one value per
/// line, lists sorted and deduplicated with one element per line, `-` for
/// an absent optional scalar. Paths and prose never appear — identity is
/// frontmatter IDs only.
fn canonical_text(rfcs: &[RfcMeta], rules: &[RuleMeta], gates: &[PackGateDecl]) -> String {
    fn list_lines(out: &mut String, indent: &str, items: &[String]) {
        for item in items {
            out.push_str(&format!("{indent}- {item}\n"));
        }
    }

    let mut out = format!("{CANONICAL_HEADER}\n");
    for rfc in rfcs {
        out.push_str(&format!("rfc {}\n", rfc.id));
        out.push_str(&format!("  title: {}\n", rfc.title));
        out.push_str(&format!("  owner: {}\n", rfc.owner));
        out.push_str(&format!("  status: {}\n", rfc.status.as_str()));
        out.push_str(&format!(
            "  effective-at: {}\n",
            rfc.effective_at.as_deref().unwrap_or("-")
        ));
        out.push_str("  supersedes:\n");
        list_lines(&mut out, "    ", &rfc.supersedes);
    }
    for rule in rules {
        out.push_str(&format!("rule {}\n", rule.id));
        out.push_str(&format!("  revision: {}\n", rule.revision));
        out.push_str(&format!("  rfc: {}\n", rule.rfc));
        out.push_str(&format!("  level: {}\n", rule.level.as_str()));
        out.push_str(&format!("  status: {}\n", rule.status.as_str()));
        out.push_str(&format!("  statement: {}\n", rule.statement));
        out.push_str("  domains:\n");
        list_lines(&mut out, "    ", &rule.domains);
        out.push_str("  stages:\n");
        let stages: Vec<String> = rule.stages.iter().map(|s| s.as_str().to_string()).collect();
        list_lines(&mut out, "    ", &stages);
        out.push_str("  when-paths:\n");
        list_lines(&mut out, "    ", &rule.when_paths);
        out.push_str("  task-classes:\n");
        list_lines(&mut out, "    ", &rule.task_classes);
        out.push_str(&format!(
            "  checker: {}\n",
            rule.checker
                .as_ref()
                .map(Checker::render)
                .unwrap_or_else(|| "-".to_string())
        ));
        out.push_str(&format!("  waivable: {}\n", rule.waivable));
    }
    for gate in gates {
        out.push_str(&format!("gate {}\n", gate.name));
        out.push_str(&format!("  command: {}\n", gate.command));
        out.push_str("  when-paths:\n");
        list_lines(&mut out, "    ", &gate.when_paths);
    }
    out
}

// ---------------------------------------------------------------------------
// The strict frontmatter subset (no YAML — see the module docs)
// ---------------------------------------------------------------------------

/// One parsed field value: a scalar or an inline list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldValue {
    Scalar(String),
    List(Vec<String>),
}

/// A parsed frontmatter block: fields in declared order (duplicates were
/// refused at parse time).
struct Frontmatter {
    fields: Vec<(String, FieldValue)>,
}

impl Frontmatter {
    fn get(&self, key: &str) -> Option<&FieldValue> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Refuse any field the document type does not declare, naming file and
    /// field (the unknown-field failure class).
    fn check_unknown(&self, display: &str, known: &[&str]) -> Result<(), String> {
        for (key, _) in &self.fields {
            if !known.contains(&key.as_str()) {
                return Err(format!(
                    "{display}: unknown frontmatter field `{key}` (declared fields: {})",
                    known.join(", ")
                ));
            }
        }
        Ok(())
    }

    fn scalar(&self, key: &str, display: &str) -> Result<Option<&str>, String> {
        match self.get(key) {
            Some(FieldValue::Scalar(value)) => Ok(Some(value.as_str())),
            Some(FieldValue::List(_)) => Err(format!(
                "{display}: field `{key}` must be a scalar, got a list"
            )),
            None => Ok(None),
        }
    }

    fn required_scalar(&self, key: &str, display: &str) -> Result<String, String> {
        self.scalar(key, display)?
            .map(str::to_string)
            .ok_or_else(|| format!("{display}: missing required field `{key}`"))
    }

    fn list(&self, key: &str, display: &str) -> Result<Option<&[String]>, String> {
        match self.get(key) {
            Some(FieldValue::List(items)) => Ok(Some(items.as_slice())),
            Some(FieldValue::Scalar(_)) => Err(format!(
                "{display}: field `{key}` must be a list (`{key}: [a, b]`), got a scalar"
            )),
            None => Ok(None),
        }
    }

    fn required_list(&self, key: &str, display: &str) -> Result<Vec<String>, String> {
        self.list(key, display)?
            .map(<[String]>::to_vec)
            .ok_or_else(|| format!("{display}: missing required field `{key}`"))
    }
}

/// Split a corpus file into its frontmatter fields, refusing everything the
/// strict subset does not declare. The Markdown body after the closing
/// fence is never parsed and never hashed (rationale, not authority).
fn parse_frontmatter(text: &str, display: &str) -> Result<Frontmatter, String> {
    // A UTF-8 BOM is normalized away rather than refused: Windows-authored
    // Markdown carries one and it changes no semantics.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines().enumerate();
    let Some((_, first)) = lines.next() else {
        return Err(format!(
            "{display}: empty file — expected a `---` frontmatter fence"
        ));
    };
    if first != "---" {
        return Err(format!(
            "{display}: line 1 must be the `---` frontmatter fence, got `{first}`"
        ));
    }
    let mut fields: Vec<(String, FieldValue)> = Vec::new();
    for (idx, line) in lines {
        let line_no = idx + 1;
        if line == "---" {
            return Ok(Frontmatter { fields });
        }
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            return Err(format!(
                "{display}: line {line_no}: unexpected indentation — the frontmatter \
                 subset has no nested or block values"
            ));
        }
        if line.contains('\t') {
            return Err(format!(
                "{display}: line {line_no}: tab characters are not in the frontmatter \
                 subset"
            ));
        }
        let Some(colon) = line.find(':') else {
            return Err(format!(
                "{display}: line {line_no}: expected `key: value`, got `{line}`"
            ));
        };
        let key = &line[..colon];
        if !is_kebab_key(key) {
            return Err(format!(
                "{display}: line {line_no}: unsupported field name `{key}` (lowercase \
                 kebab-case keys only)"
            ));
        }
        if fields.iter().any(|(k, _)| k == key) {
            return Err(format!(
                "{display}: line {line_no}: duplicate field `{key}`"
            ));
        }
        let raw = line[colon + 1..].trim();
        if raw.is_empty() {
            return Err(format!(
                "{display}: line {line_no}: field `{key}` has an empty value — omit \
                 optional fields instead (implicit null is not in the subset)"
            ));
        }
        let value = parse_value(raw, display, line_no, key)?;
        fields.push((key.to_string(), value));
    }
    Err(format!(
        "{display}: missing the closing `---` frontmatter fence"
    ))
}

/// Kebab-case field names (`effective-at`, `when-paths`): lowercase ASCII
/// letters and digits, dash-separated, starting with a letter.
fn is_kebab_key(key: &str) -> bool {
    let mut parts = key.split('-');
    let valid_part = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    match parts.next() {
        Some(first) if first.chars().next().is_some_and(|c| c.is_ascii_lowercase()) => {
            valid_part(first) && parts.all(valid_part)
        }
        _ => false,
    }
}

/// Parse one field's raw value text into a scalar or list, refusing the
/// YAML constructs the subset does not support (each error names the
/// construct and the field).
fn parse_value(raw: &str, display: &str, line_no: usize, key: &str) -> Result<FieldValue, String> {
    let refusal = |what: &str| {
        format!(
            "{display}: line {line_no}: field `{key}`: {what} is not in the frontmatter \
             subset"
        )
    };
    match raw.chars().next() {
        Some('[') => parse_inline_list(raw, display, line_no, key).map(FieldValue::List),
        Some('"') => {
            let (value, rest) = parse_quoted(&raw[1..], display, line_no, key)?;
            check_trailing(rest, display, line_no, key)?;
            Ok(FieldValue::Scalar(value))
        }
        Some('\'') => Err(refusal("single-quoted strings (use double quotes)")),
        Some('&') => Err(refusal("anchors")),
        Some('*') => Err(refusal("aliases")),
        Some('!') => Err(refusal("tags")),
        Some('|' | '>') => Err(refusal(
            "block scalars (values are single-line; the statement is one line)",
        )),
        Some('{') => Err(refusal("flow mappings")),
        _ => {
            // A bare scalar runs to end-of-line; a ` #` starts a comment
            // (YAML-consistent) so a hash inside a value needs quotes.
            let cut = raw.find(" #").unwrap_or(raw.len());
            let value = raw[..cut].trim();
            if value.is_empty() {
                return Err(format!(
                    "{display}: line {line_no}: field `{key}` has an empty value — omit \
                     optional fields instead"
                ));
            }
            Ok(FieldValue::Scalar(value.to_string()))
        }
    }
}

/// Whatever follows a quoted value or list: whitespace plus an optional `#`
/// comment, nothing else.
fn check_trailing(rest: &str, display: &str, line_no: usize, key: &str) -> Result<(), String> {
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(format!(
            "{display}: line {line_no}: field `{key}` has trailing text after the value"
        ))
    }
}

/// A double-quoted string with exactly two escapes (`\"` and `\\`) —
/// everything else is refused as an unsupported escape.
fn parse_quoted<'a>(
    text: &'a str,
    display: &str,
    line_no: usize,
    key: &str,
) -> Result<(String, &'a str), String> {
    let mut out = String::new();
    let mut chars = text.char_indices();
    while let Some((idx, c)) = chars.next() {
        match c {
            '"' => return Ok((out, &text[idx + 1..])),
            '\\' => match chars.next() {
                Some((_, '"')) => out.push('"'),
                Some((_, '\\')) => out.push('\\'),
                Some((_, other)) => {
                    return Err(format!(
                        "{display}: line {line_no}: field `{key}`: unsupported escape \
                         `\\{other}` (only `\\\"` and `\\\\` are in the subset)"
                    ))
                }
                None => break,
            },
            c => out.push(c),
        }
    }
    Err(format!(
        "{display}: line {line_no}: field `{key}`: unterminated quoted string"
    ))
}

/// An inline list `[a, b, "c, d"]`: comma-separated bare or double-quoted
/// elements, optional trailing comma, no comments inside the brackets.
fn parse_inline_list(
    raw: &str,
    display: &str,
    line_no: usize,
    key: &str,
) -> Result<Vec<String>, String> {
    let mut items = Vec::new();
    let mut rest = &raw[1..];
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix(']') {
            check_trailing(after, display, line_no, key)?;
            return Ok(items);
        }
        if rest.is_empty() {
            return Err(format!(
                "{display}: line {line_no}: field `{key}`: unterminated `[` in list value"
            ));
        }
        if let Some(after) = rest.strip_prefix('"') {
            let (value, after) = parse_quoted(after, display, line_no, key)?;
            items.push(value);
            rest = after;
        } else {
            let end = rest.find([',', ']']).ok_or_else(|| {
                format!(
                    "{display}: line {line_no}: field `{key}`: unterminated `[` in \
                         list value"
                )
            })?;
            let element = rest[..end].trim();
            if element.is_empty() {
                return Err(format!(
                    "{display}: line {line_no}: field `{key}`: empty list element"
                ));
            }
            if element.contains(['#', '"', '[', '\'']) {
                return Err(format!(
                    "{display}: line {line_no}: field `{key}`: bare list element \
                     `{element}` contains a character the subset does not allow (quote \
                     the element)"
                ));
            }
            items.push(element.to_string());
            rest = &rest[end..];
        }
        rest = rest.trim_start();
        match rest.strip_prefix(',') {
            Some(after) => rest = after,
            None => {
                // Must now be `]` (handled at the loop top).
                if !rest.starts_with(']') {
                    return Err(format!(
                        "{display}: line {line_no}: field `{key}`: expected `,` or `]` in \
                         list value"
                    ));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-document semantic validation
// ---------------------------------------------------------------------------

/// An RFC/rule ID: non-empty, ASCII alphanumerics plus `.`, `_`, `-`. IDs
/// are join keys for findings and trends (D-C), so their charset is tight.
fn is_valid_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn required_id(fm: &Frontmatter, key: &str, display: &str) -> Result<String, String> {
    let id = fm.required_scalar(key, display)?;
    if !is_valid_id(&id) {
        return Err(format!(
            "{display}: field `{key}` is `{id}` — IDs use ASCII alphanumerics, `.`, `_`, \
             `-` only"
        ));
    }
    Ok(id)
}

/// A `revision`: digit characters only (no sign, no YAML implicit-typing
/// surprises), value ≥ 1.
fn required_revision(fm: &Frontmatter, display: &str) -> Result<u64, String> {
    let raw = fm.required_scalar("revision", display)?;
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "{display}: field `revision` must be a positive integer, got `{raw}`"
        ));
    }
    match raw.parse::<u64>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "{display}: field `revision` must be a positive integer, got `{raw}`"
        )),
    }
}

/// `waivable`: exactly `true`/`false` — implicit boolean typing (`yes`,
/// `True`) is refused, and the DEFAULT IS FALSE (D-I fails closed).
fn optional_bool(fm: &Frontmatter, key: &str, display: &str) -> Result<bool, String> {
    match fm.scalar(key, display)? {
        Some("true") => Ok(true),
        Some("false") | None => Ok(false),
        Some(other) => Err(format!(
            "{display}: field `{key}` must be exactly `true` or `false`, got `{other}`"
        )),
    }
}

/// Sorted, deduplicated list normalization — authored order and duplicates
/// never reach the canonical bytes.
fn normalized_list(mut items: Vec<String>) -> Vec<String> {
    items.sort();
    items.dedup();
    items
}

/// Validate and normalize one `rfc.md` frontmatter.
fn load_rfc(fm: &Frontmatter, display: &str) -> Result<RfcMeta, String> {
    fm.check_unknown(
        display,
        &[
            "id",
            "title",
            "owner",
            "status",
            "effective-at",
            "supersedes",
        ],
    )?;
    let id = required_id(fm, "id", display)?;
    let title = fm.required_scalar("title", display)?;
    let owner = fm.required_scalar("owner", display)?;
    let status_raw = fm.required_scalar("status", display)?;
    let status = RfcStatus::parse(&status_raw).ok_or_else(|| {
        format!(
            "{display}: field `status` is `{status_raw}` — RFC statuses are draft, \
             approved, enforced, retired (D-B)"
        )
    })?;
    let effective_at = match fm.scalar("effective-at", display)? {
        Some(raw) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(raw).map_err(|_| {
                format!(
                    "{display}: field `effective-at` must be an RFC3339 timestamp, got \
                     `{raw}`"
                )
            })?;
            // Normalized to UTC seconds so byte-identical instants digest
            // identically however they were authored (`+00:00` vs `Z`).
            Some(
                parsed
                    .with_timezone(&chrono::Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )
        }
        None => None,
    };
    let mut supersedes = Vec::new();
    if let Some(items) = fm.list("supersedes", display)? {
        for item in items {
            if !is_valid_id(item) {
                return Err(format!(
                    "{display}: field `supersedes` element `{item}` is not a valid RFC id"
                ));
            }
            supersedes.push(item.clone());
        }
    }
    Ok(RfcMeta {
        id,
        title,
        owner,
        status,
        effective_at,
        supersedes: normalized_list(supersedes),
    })
}

/// Validate and normalize one rule file's frontmatter.
fn load_rule(fm: &Frontmatter, display: &str) -> Result<RuleMeta, String> {
    fm.check_unknown(
        display,
        &[
            "id",
            "revision",
            "rfc",
            "level",
            "status",
            "statement",
            "domains",
            "stages",
            "when-paths",
            "task-classes",
            "checker",
            "waivable",
        ],
    )?;
    let id = required_id(fm, "id", display)?;
    let revision = required_revision(fm, display)?;
    let rfc = required_id(fm, "rfc", display)?;
    let level_raw = fm.required_scalar("level", display)?;
    let level = RuleLevel::parse(&level_raw).ok_or_else(|| {
        format!(
            "{display}: field `level` is `{level_raw}` — RFC-2119 levels are must and \
             should (D-B has no `may` row)"
        )
    })?;
    let status_raw = fm.required_scalar("status", display)?;
    let status = RuleStatus::parse(&status_raw).ok_or_else(|| {
        format!(
            "{display}: field `status` is `{status_raw}` — rule statuses are active and \
             retired (D-B)"
        )
    })?;
    let statement = fm.required_scalar("statement", display)?;
    let domains = normalized_list(fm.required_list("domains", display)?);
    let mut stages = Vec::new();
    for raw in fm.required_list("stages", display)? {
        let Some(stage) = RuleStage::parse(&raw) else {
            return Err(format!(
                "{display}: field `stages` element `{raw}` — stages are planning, \
                 implementation, validation, merge"
            ));
        };
        stages.push(stage);
    }
    if stages.is_empty() {
        return Err(format!(
            "{display}: field `stages` must list at least one stage — a rule applying \
             nowhere is not a rule"
        ));
    }
    // Sorted by NAME so authored order never reaches the canonical bytes.
    stages.sort_by_key(|s| s.as_str());
    stages.dedup();
    let mut when_paths = Vec::new();
    if let Some(items) = fm.list("when-paths", display)? {
        for item in items {
            let path = Path::new(item);
            if item.trim().is_empty()
                || path.is_absolute()
                || path.components().any(|part| {
                    !matches!(
                        part,
                        std::path::Component::CurDir | std::path::Component::Normal(_)
                    )
                })
            {
                return Err(format!(
                    "{display}: field `when-paths` entries must be repo-relative paths \
                     without parent components: {item:?}"
                ));
            }
            let normalized = crate::merge_gate::normalize_relative_path(item, false);
            if normalized.is_empty() || normalized == "." {
                return Err(format!(
                    "{display}: field `when-paths` entries must name a repo path — omit \
                     the field to leave the rule unscoped"
                ));
            }
            when_paths.push(normalized);
        }
    }
    let mut task_classes = Vec::new();
    if let Some(items) = fm.list("task-classes", display)? {
        task_classes.extend(items.iter().cloned());
    }
    let checker = match fm.scalar("checker", display)? {
        Some(raw) => {
            Some(Checker::parse(raw).map_err(|e| format!("{display}: field `checker`: {e}"))?)
        }
        None => None,
    };
    let waivable = optional_bool(fm, "waivable", display)?;
    Ok(RuleMeta {
        id,
        revision,
        rfc,
        level,
        status,
        statement,
        domains,
        stages,
        when_paths: normalized_list(when_paths),
        task_classes: normalized_list(task_classes),
        checker,
        waivable,
    })
}

// ---------------------------------------------------------------------------
// Tests (KRZ-341 acceptance) — the `flight_rules_contract_` prefix is unique
// in the workspace (verified by grep before landing), so the contract filter
// `cargo test --workspace flight_rules_contract_` can never pass vacuously
// on a pre-existing test.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{SCHEMA_BASE, SCHEMA_CONTRACT, SCHEMA_STANDARDS};

    /// A pack directory in a tempdir with the given pack.toml + corpus files.
    fn pack_with(manifest: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("pack");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PACK_MANIFEST), manifest).unwrap();
        for (rel, body) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        (tmp, dir)
    }

    /// The schema-4 manifest of the synthetic fixture pack: one declared
    /// gate (rule one's checker target), one standards root.
    const PACK_TOML: &str = r#"
[pack]
name = "zz-standards-pack"
schema = 4

[standards]
root = "standards"

[[gate]]
name = "zz-gate-one"
command = "cd ."
"#;

    /// One approved RFC (effective-at authored non-UTC to pin the UTC
    /// normalization) — prose below the fence is never hashed.
    const RFC_MD: &str = "\
---
id: RFC-001
title: zz synthetic safety standard
status: approved
owner: zz-platform
effective-at: 2026-09-01T02:00:00+02:00
---
Prose rationale — never hashed.
";

    const RULE_ONE: &str = "\
---
id: ZZ-RULE-001
revision: 1
rfc: RFC-001
level: must
status: active
statement: zz synthetic must statement one.
domains: [zz-domain]
stages: [implementation, validation]
checker: gate:zz-gate-one
waivable: false
---
Rule one prose.
";

    /// Domains authored UNSORTED to pin the normalized (sorted, deduplicated)
    /// ordering; rule two also exercises every optional field.
    const RULE_TWO: &str = "\
---
id: ZZ-RULE-002
revision: 2
rfc: RFC-001
level: should
status: active
statement: zz synthetic should statement two.
domains: [zz-other, zz-domain]
stages: [planning, implementation, validation, merge]
when-paths: [crates/]
task-classes: [implementation]
checker: agent-judgement
---
Rule two prose.
";

    const CORPUS: &[(&str, &str)] = &[
        ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
        ("standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md", RULE_ONE),
        ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
    ];

    fn synthetic_pack() -> (tempfile::TempDir, PathBuf) {
        pack_with(PACK_TOML, CORPUS)
    }

    /// Load the fixture pack as a repo-tracked pack and return its standards
    /// manifest (the pack-level `standards` field is the eager-load proof).
    fn load_trusted(dir: &Path) -> StandardsManifest {
        crate::pack::Pack::load_with_trust(dir, StandardsTrust::RepoTracked)
            .expect("load")
            .expect("a pack")
            .standards
            .expect("a standards manifest")
    }

    /// The canonical bytes of the synthetic fixture, pinned verbatim: any
    /// change to the normalization format breaks this test DELIBERATELY (the
    /// digest is an audit surface — silent format drift is unacceptable).
    const EXPECTED_CANONICAL: &str = "\
kranz-standards-manifest v1
rfc RFC-001
  title: zz synthetic safety standard
  owner: zz-platform
  status: approved
  effective-at: 2026-09-01T00:00:00Z
  supersedes:
rule ZZ-RULE-001
  revision: 1
  rfc: RFC-001
  level: must
  status: active
  statement: zz synthetic must statement one.
  domains:
    - zz-domain
  stages:
    - implementation
    - validation
  when-paths:
  task-classes:
  checker: gate:zz-gate-one
  waivable: false
rule ZZ-RULE-002
  revision: 2
  rfc: RFC-001
  level: should
  status: active
  statement: zz synthetic should statement two.
  domains:
    - zz-domain
    - zz-other
  stages:
    - implementation
    - merge
    - planning
    - validation
  when-paths:
    - crates
  task-classes:
    - implementation
  checker: agent-judgement
  waivable: false
gate zz-gate-one
  command: cd .
  when-paths:
";

    /// sha256(EXPECTED_CANONICAL), pinned: the digest is an audit surface, so
    /// a normalization-format change must be deliberate and review-visible.
    const EXPECTED_DIGEST: &str =
        "8dfb505b203fea5f66286353defce0ef46118331fc45946b00b27c6bacd01df7";

    #[test]
    fn flight_rules_contract_synthetic_pack_loads_byte_stable_manifest_and_digest() {
        let (_tmp, dir) = synthetic_pack();
        let manifest = load_trusted(&dir);
        assert_eq!(manifest.root, "standards");
        assert_eq!(manifest.rfcs.len(), 1);
        assert_eq!(manifest.rules.len(), 2);
        assert_eq!(manifest.gate_bindings.len(), 1);
        assert_eq!(manifest.canonical_text(), EXPECTED_CANONICAL);
        // sha256(EXPECTED_CANONICAL) — pinned so any normalization drift is
        // a deliberate, review-visible change.
        assert_eq!(manifest.digest, EXPECTED_DIGEST);
        // Spot-check the parsed metadata end to end.
        let rfc = &manifest.rfcs[0];
        assert_eq!(rfc.id, "RFC-001");
        assert_eq!(rfc.status, RfcStatus::Approved);
        assert_eq!(
            rfc.effective_at.as_deref(),
            Some("2026-09-01T00:00:00Z"),
            "effective-at normalizes to UTC seconds"
        );
        let rule = manifest.rule("ZZ-RULE-002").expect("rule two");
        assert_eq!(rule.revision, 2);
        assert_eq!(rule.level, RuleLevel::Should);
        assert_eq!(rule.domains, vec!["zz-domain", "zz-other"], "sorted");
        assert_eq!(rule.when_paths, vec!["crates"], "normalized, slash-free");
        assert_eq!(rule.checker, Some(Checker::AgentJudgement));
        assert!(!rule.waivable);
        assert_eq!(
            manifest.effective_status(rule),
            RfcStatus::Approved,
            "an active rule inherits its RFC's lifecycle (D-B)"
        );
    }

    #[test]
    fn flight_rules_contract_renaming_files_and_dirs_preserves_identity_and_digest() {
        let (_tmp, dir) = synthetic_pack();
        let before = load_trusted(&dir);
        // Identity is frontmatter IDs, never paths (D-C): rename the rule
        // file AND the RFC directory — the digest must not move.
        let renamed = &[
            ("standards/RFC-001-renamed/rfc.md", RFC_MD),
            (
                "standards/RFC-001-renamed/rules/ZZ-RENAMED-001.md",
                RULE_ONE,
            ),
            ("standards/RFC-001-renamed/rules/ZZ-RULE-002.md", RULE_TWO),
        ];
        let (_tmp2, dir2) = pack_with(PACK_TOML, renamed);
        let after = load_trusted(&dir2);
        assert_eq!(before.digest, after.digest);
        assert_eq!(before, after, "paths are not identity (D-C)");
    }

    #[test]
    fn flight_rules_contract_prose_edits_do_not_churn_the_digest() {
        let (_tmp, dir) = synthetic_pack();
        let before = load_trusted(&dir);
        let edited_rfc = RFC_MD.replace("never hashed", "EDITED rationale");
        let edited_rule = RULE_ONE.replace("Rule one prose.", "COMPLETELY NEW PROSE.");
        let corpus = &[
            ("standards/RFC-001-zz-safety/rfc.md", edited_rfc.as_str()),
            (
                "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                edited_rule.as_str(),
            ),
            ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
        ];
        let (_tmp2, dir2) = pack_with(PACK_TOML, corpus);
        let after = load_trusted(&dir2);
        assert_eq!(
            before.digest, after.digest,
            "prose is rationale, not a second machine authority (D-C)"
        );
    }

    #[test]
    fn flight_rules_contract_changing_a_referenced_gate_declaration_changes_the_digest() {
        let (_tmp, dir) = synthetic_pack();
        let before = load_trusted(&dir);
        // The referenced checker's declaration is governing bytes (D-F):
        // editing its command must change the standards digest even though
        // no rule file moved.
        let manifest_toml = PACK_TOML.replace("command = \"cd .\"", "command = \"cd ..\"");
        let (_tmp2, dir2) = pack_with(&manifest_toml, CORPUS);
        let after = load_trusted(&dir2);
        assert_ne!(before.digest, after.digest);
        // An UNREFERENCED gate stays out of the digest: only the bindings
        // rules actually name are governing bytes.
        let with_extra_gate =
            format!("{PACK_TOML}\n[[gate]]\nname = \"zz-gate-two\"\ncommand = \"cd /\"\n");
        let (_tmp3, dir3) = pack_with(&with_extra_gate, CORPUS);
        let third = load_trusted(&dir3);
        assert_eq!(before.digest, third.digest);
    }

    #[test]
    fn flight_rules_contract_standards_section_requires_schema_four() {
        for schema in [SCHEMA_BASE, SCHEMA_CONTRACT] {
            let manifest = format!(
                "[pack]\nname = \"x\"\nschema = {schema}\n\n[standards]\nroot = \"standards\"\n"
            );
            let (_tmp, dir) = pack_with(&manifest, &[]);
            let err = crate::pack::Pack::load(&dir).expect_err("must fail");
            assert!(err.contains("[standards]"), "names the field: {err}");
            assert!(err.contains("schema"), "says why: {err}");
        }
    }

    #[test]
    fn flight_rules_contract_standards_section_unknown_key_fails_closed() {
        let manifest =
            "[pack]\nname = \"x\"\nschema = 4\n\n[standards]\nroot = \"standards\"\nbogus = 1\n";
        let (_tmp, dir) = pack_with(manifest, &[]);
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("unknown field `bogus`"), "{err}");
        assert!(err.contains("[standards]"), "{err}");

        // A declared root that does not exist is a misconfiguration, not an
        // empty corpus.
        let manifest = "[pack]\nname = \"x\"\nschema = 4\n\n[standards]\nroot = \"missing\"\n";
        let (_tmp2, dir2) = pack_with(manifest, &[]);
        let err = crate::pack::Pack::load(&dir2).expect_err("must fail");
        assert!(err.contains("root `missing` does not exist"), "{err}");

        // An escaping root is refused like every other pack path.
        let manifest = "[pack]\nname = \"x\"\nschema = 4\n\n[standards]\nroot = \"../outside\"\n";
        let (_tmp3, dir3) = pack_with(manifest, &[]);
        let err = crate::pack::Pack::load(&dir3).expect_err("must fail");
        assert!(err.contains("pack-relative path"), "{err}");
    }

    #[test]
    fn flight_rules_contract_missing_and_unknown_frontmatter_fields_fail() {
        // Missing required field (statement), naming file and field.
        let rule = RULE_ONE.replace("statement: zz synthetic must statement one.\n", "");
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    rule.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("missing required field `statement`"), "{err}");
        assert!(err.contains("ZZ-RULE-001.md"), "names the file: {err}");

        // Unknown field, naming file and field.
        let rule = RULE_ONE.replace("waivable: false", "waivable: false\nbogus: nope");
        let (_tmp2, dir2) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    rule.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir2).expect_err("must fail");
        assert!(err.contains("unknown frontmatter field `bogus`"), "{err}");
        assert!(err.contains("ZZ-RULE-001.md"), "names the file: {err}");

        // An RFC missing its owner.
        let rfc = RFC_MD.replace("owner: zz-platform\n", "");
        let (_tmp3, dir3) = pack_with(
            PACK_TOML,
            &[("standards/RFC-001-zz-safety/rfc.md", rfc.as_str())],
        );
        let err = crate::pack::Pack::load(&dir3).expect_err("must fail");
        assert!(err.contains("missing required field `owner`"), "{err}");
        assert!(err.contains("rfc.md"), "names the file: {err}");
    }

    #[test]
    fn flight_rules_contract_invalid_level_status_and_stage_fail() {
        for (from, to, needle) in [
            ("level: must", "level: may", "field `level` is `may`"),
            (
                "status: active",
                "status: limbo",
                "field `status` is `limbo`",
            ),
            (
                "stages: [implementation, validation]",
                "stages: [implementation, guessing]",
                "field `stages` element `guessing`",
            ),
            (
                "waivable: false",
                "waivable: yes",
                "field `waivable` must be exactly `true` or `false`",
            ),
        ] {
            let rule = RULE_ONE.replace(from, to);
            let (_tmp, dir) = pack_with(
                PACK_TOML,
                &[
                    ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                    (
                        "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                        rule.as_str(),
                    ),
                ],
            );
            let err = crate::pack::Pack::load(&dir).expect_err("must fail");
            assert!(err.contains(needle), "{from} → {to}: {err}");
            assert!(err.contains("ZZ-RULE-001.md"), "names the file: {err}");
        }
        // An invalid RFC status fails the same way.
        let rfc = RFC_MD.replace("status: approved", "status: wishful");
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[("standards/RFC-001-zz-safety/rfc.md", rfc.as_str())],
        );
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("field `status` is `wishful`"), "{err}");
    }

    #[test]
    fn flight_rules_contract_duplicate_ids_fail() {
        // Two rules sharing an ID.
        let dupe = RULE_TWO.replace("ZZ-RULE-002", "ZZ-RULE-001");
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md", RULE_ONE),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md",
                    dupe.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(
            err.contains("duplicate standards id `ZZ-RULE-001`"),
            "{err}"
        );

        // A rule ID colliding with an RFC ID: ONE pack-wide namespace (D-C).
        let dupe = RULE_ONE.replace("id: ZZ-RULE-001", "id: RFC-001");
        let (_tmp2, dir2) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    dupe.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir2).expect_err("must fail");
        assert!(err.contains("duplicate standards id `RFC-001`"), "{err}");
    }

    #[test]
    fn flight_rules_contract_orphan_rule_fails() {
        let orphan = RULE_ONE.replace("rfc: RFC-001", "rfc: RFC-999");
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    orphan.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("rule `ZZ-RULE-001`"), "{err}");
        assert!(err.contains("RFC-999"), "names the missing parent: {err}");
        assert!(err.contains("orphan"), "{err}");
    }

    #[test]
    fn flight_rules_contract_bad_revision_and_checker_fail() {
        for (from, to, needle) in [
            (
                "revision: 1",
                "revision: 0",
                "field `revision` must be a positive integer",
            ),
            (
                "revision: 1",
                "revision: -2",
                "field `revision` must be a positive integer",
            ),
            (
                "revision: 1",
                "revision: two",
                "field `revision` must be a positive integer",
            ),
            (
                "checker: gate:zz-gate-one",
                "checker: gate:zz-undeclared",
                "names no declared [[gate]]",
            ),
            (
                "checker: gate:zz-gate-one",
                "checker: run-the-script",
                "supported checker forms",
            ),
            (
                "checker: gate:zz-gate-one",
                "checker: gate:",
                "supported checker forms",
            ),
        ] {
            let rule = RULE_ONE.replace(from, to);
            let (_tmp, dir) = pack_with(
                PACK_TOML,
                &[
                    ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                    (
                        "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                        rule.as_str(),
                    ),
                ],
            );
            let err = crate::pack::Pack::load(&dir).expect_err("must fail");
            assert!(err.contains(needle), "{from} → {to}: {err}");
            assert!(err.contains("ZZ-RULE-001.md"), "names the file: {err}");
        }
    }

    #[test]
    fn flight_rules_contract_approved_rule_requires_a_checker_draft_may_omit() {
        // RFC approved + rule without a checker: the advisory period must be
        // mechanically evaluable (D-F) — refused, naming the rule.
        let no_checker = RULE_ONE.replace("checker: gate:zz-gate-one\n", "");
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    no_checker.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("rule `ZZ-RULE-001`"), "{err}");
        assert!(err.contains("declares no checker"), "{err}");

        // The SAME rule under a DRAFT RFC loads: drafts may omit the binding.
        let draft_rfc = RFC_MD.replace("status: approved", "status: draft");
        let (_tmp2, dir2) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", draft_rfc.as_str()),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    no_checker.as_str(),
                ),
            ],
        );
        let manifest = load_trusted(&dir2);
        assert_eq!(manifest.rules.len(), 1);
        assert_eq!(manifest.rules[0].checker, None);
        assert_eq!(
            manifest.effective_status(&manifest.rules[0]),
            RfcStatus::Draft
        );
    }

    #[test]
    fn flight_rules_contract_retired_rule_may_omit_its_checker() {
        // A tombstone keeps its identity without a binding (D-B): retired
        // under an enforced RFC is still retired, never enforced.
        let enforced_rfc = RFC_MD.replace("status: approved", "status: enforced");
        let retired = RULE_ONE
            .replace("status: active", "status: retired")
            .replace("checker: gate:zz-gate-one\n", "");
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", enforced_rfc.as_str()),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    retired.as_str(),
                ),
            ],
        );
        let manifest = load_trusted(&dir);
        assert_eq!(
            manifest.effective_status(&manifest.rules[0]),
            RfcStatus::Retired,
            "the rule tombstone narrows an enforced RFC (D-B)"
        );
    }

    #[test]
    fn flight_rules_contract_caps_fail_promptly() {
        // Per-file bytes: one oversized rule file trips the cap naming it.
        let big = format!(
            "{RULE_ONE}{}",
            "x".repeat(MAX_STANDARDS_FILE_BYTES as usize + 1)
        );
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    big.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("per-file cap"), "{err}");
        assert!(err.contains("ZZ-RULE-001.md"), "names the file: {err}");

        // File count: more corpus files than the cap (RFC-only dirs, so the
        // rule cap cannot fire first).
        let mut files: Vec<(String, String)> = Vec::new();
        for i in 0..=(MAX_STANDARDS_FILES) {
            files.push((
                format!("standards/RFC-D{i:04}/rfc.md"),
                RFC_MD.replace("RFC-001", &format!("RFC-D{i:04}")),
            ));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let (_tmp2, dir2) = pack_with(PACK_TOML, &refs);
        let err = crate::pack::Pack::load(&dir2).expect_err("must fail");
        assert!(err.contains("file cap"), "{err}");

        // Rule count: one RFC with more rule files than the rule cap.
        let mut files: Vec<(String, String)> = vec![(
            "standards/RFC-001-zz-safety/rfc.md".to_string(),
            RFC_MD.to_string(),
        )];
        for i in 0..=(MAX_STANDARDS_RULES) {
            let rule = RULE_ONE.replace("ZZ-RULE-001", &format!("ZZ-RULE-C{i:04}"));
            files.push((
                format!("standards/RFC-001-zz-safety/rules/ZZ-RULE-C{i:04}.md"),
                rule,
            ));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let (_tmp3, dir3) = pack_with(PACK_TOML, &refs);
        let err = crate::pack::Pack::load(&dir3).expect_err("must fail");
        assert!(err.contains("rule cap"), "{err}");

        // Total normalized bytes: many rules with long single-line
        // statements — under the per-file and count caps, over the total.
        let mut files: Vec<(String, String)> = vec![(
            "standards/RFC-001-zz-safety/rfc.md".to_string(),
            RFC_MD.to_string(),
        )];
        for i in 0..250usize {
            let rule = RULE_ONE
                .replace("ZZ-RULE-001", &format!("ZZ-RULE-N{i:04}"))
                .replace("zz synthetic must statement one.", &"s".repeat(4600));
            files.push((
                format!("standards/RFC-001-zz-safety/rules/ZZ-RULE-N{i:04}.md"),
                rule,
            ));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let (_tmp4, dir4) = pack_with(PACK_TOML, &refs);
        let err = crate::pack::Pack::load(&dir4).expect_err("must fail");
        assert!(err.contains("normalized standards manifest"), "{err}");
    }

    #[test]
    fn flight_rules_contract_invalid_utf8_fails() {
        let (_tmp, dir) = synthetic_pack();
        let path = dir.join("standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md");
        let mut bytes = RULE_ONE.as_bytes().to_vec();
        bytes.push(0xFF);
        bytes.push(0xFE);
        std::fs::write(&path, bytes).unwrap();
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("not valid UTF-8"), "{err}");
        assert!(err.contains("ZZ-RULE-001.md"), "names the file: {err}");
    }

    #[test]
    fn flight_rules_contract_frontmatter_subset_refuses_yaml_surprises() {
        for (line, needle) in [
            ("statement: &anchor text", "anchors"),
            ("statement: *alias", "aliases"),
            ("statement: !!str text", "tags"),
            ("statement: |", "block scalars"),
            ("statement: 'single'", "single-quoted strings"),
            ("statement: {flow: map}", "flow mappings"),
            ("statement:", "empty value"),
            ("domains: [zz-domain", "unterminated `[`"),
            ("domains: [zz-domain,, zz-other]", "empty list element"),
        ] {
            let rule = RULE_ONE.replace("statement: zz synthetic must statement one.", line);
            let rule = if line.starts_with("domains:") {
                RULE_ONE.replace("domains: [zz-domain]", line)
            } else {
                rule
            };
            let (_tmp, dir) = pack_with(
                PACK_TOML,
                &[
                    ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                    (
                        "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                        rule.as_str(),
                    ),
                ],
            );
            let err = crate::pack::Pack::load(&dir).expect_err("must fail");
            assert!(err.contains(needle), "{line}: {err}");
            assert!(err.contains("ZZ-RULE-001.md"), "names the file: {err}");
        }

        // A ` #` comment after a value is accepted (and never hashed): the
        // full fixture with only an added comment digests identically.
        let commented = RULE_ONE.replace(
            "statement: zz synthetic must statement one.",
            "statement: zz synthetic must statement one. # reviewed",
        );
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    commented.as_str(),
                ),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
            ],
        );
        let manifest = load_trusted(&dir);
        assert_eq!(manifest.digest, EXPECTED_DIGEST, "comments never govern");

        // Indented (nested-looking) and block-list lines are not the subset.
        for bad_line in ["  status: active", "- status: active"] {
            let rule = RULE_ONE.replace("status: active", bad_line);
            let (_tmp, dir) = pack_with(
                PACK_TOML,
                &[
                    ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                    (
                        "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                        rule.as_str(),
                    ),
                ],
            );
            let err = crate::pack::Pack::load(&dir).expect_err("must fail");
            assert!(
                err.contains("unexpected indentation") || err.contains("unsupported field name"),
                "{bad_line}: {err}"
            );
        }

        // Duplicate fields and a missing closing fence fail closed.
        let dupe = RULE_ONE.replace("level: must", "level: must\nlevel: should");
        let (_tmp, dir) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    dupe.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("duplicate field `level`"), "{err}");

        let unfenced = RULE_ONE.replace("---\nRule one prose.", "");
        let (_tmp2, dir2) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    unfenced.as_str(),
                ),
            ],
        );
        let err = crate::pack::Pack::load(&dir2).expect_err("must fail");
        assert!(err.contains("closing `---`"), "{err}");
    }

    #[test]
    fn flight_rules_contract_external_pack_cannot_activate_enforced_rules() {
        // Approved advisory rules load from an external pack…
        let (_tmp, dir) = synthetic_pack();
        let pack = crate::pack::Pack::load_with_trust(&dir, StandardsTrust::External)
            .expect("advisory loads")
            .expect("a pack");
        assert_eq!(pack.standards.as_ref().unwrap().rules.len(), 2);

        // …but an effectively ENFORCED rule is a load error naming the
        // trust remedy (D-A/D-J).
        let enforced_rfc = RFC_MD.replace("status: approved", "status: enforced");
        let (_tmp2, dir2) = pack_with(
            PACK_TOML,
            &[
                ("standards/RFC-001-zz-safety/rfc.md", enforced_rfc.as_str()),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md", RULE_ONE),
            ],
        );
        let err = crate::pack::Pack::load_with_trust(&dir2, StandardsTrust::External)
            .expect_err("must fail");
        assert!(err.contains("external/untracked"), "{err}");
        assert!(err.contains("rule `ZZ-RULE-001`"), "names the rule: {err}");
        assert!(
            err.contains("vendor the pack into the repo"),
            "names the remedy: {err}"
        );
        // The same bytes load as RepoTracked — the boundary is trust, not content.
        let pack = crate::pack::Pack::load_with_trust(&dir2, StandardsTrust::RepoTracked)
            .expect("tracked loads")
            .expect("a pack");
        assert_eq!(
            pack.standards.as_ref().unwrap().rfcs[0].status,
            RfcStatus::Enforced
        );
    }

    #[test]
    fn flight_rules_contract_schema_two_and_three_packs_carry_no_standards() {
        // Byte-identical pre-KRZ-341 behavior: no standards field, and the
        // run-start describe()/lint lines keep their pre-standards shape.
        for schema in [SCHEMA_BASE, SCHEMA_CONTRACT] {
            let manifest = format!("[pack]\nname = \"zz-plain\"\nschema = {schema}\n");
            let (_tmp, dir) = pack_with(&manifest, &[]);
            let pack = crate::pack::Pack::load(&dir)
                .expect("load")
                .expect("a pack");
            assert_eq!(pack.schema, schema);
            assert!(pack.standards.is_none());
            assert!(!pack.describe().contains("standards"), "byte-identical");
            assert!(!crate::pack::render_lint(&pack).contains("digest"));
        }
        // Schema 4 without [standards] is valid and registers no corpus.
        let (_tmp, dir) = pack_with("[pack]\nname = \"zz-bare-four\"\nschema = 4\n", &[]);
        let pack = crate::pack::Pack::load(&dir)
            .expect("load")
            .expect("a pack");
        assert_eq!(pack.schema, SCHEMA_STANDARDS);
        assert!(pack.standards.is_none());
    }

    #[test]
    fn flight_rules_contract_pack_lint_reports_the_standards_registration() {
        let (_tmp, dir) = synthetic_pack();
        let pack = crate::pack::Pack::load_with_trust(&dir, StandardsTrust::RepoTracked)
            .expect("load")
            .expect("a pack");
        let report = crate::pack::render_lint(&pack);
        assert!(
            report.contains("standards (schema 4 root `standards`)"),
            "{report}"
        );
        assert!(report.contains("digest: sha256:"), "{report}");
        assert!(
            report.contains("RFCs: 1 (draft 0, approved 1, enforced 0, retired 0)"),
            "{report}"
        );
        assert!(
            report.contains("rules: 2 (active 2, retired 0)"),
            "{report}"
        );

        // The standards-lint render names every rule with its effective
        // status and checker binding.
        let manifest = pack.standards.as_ref().unwrap();
        let rendered = render_manifest(manifest, StandardsTrust::RepoTracked);
        assert!(rendered.contains("trust: repo-tracked"), "{rendered}");
        assert!(
            rendered.contains("ZZ-RULE-001 r1 — must, approved; checker gate:zz-gate-one"),
            "{rendered}"
        );
        assert!(rendered.contains("digest: sha256:"), "{rendered}");
    }

    // ---- lifecycle transition lint --------------------------------------

    /// Two on-disk packs (base + proposed) through the same loader, then the
    /// pure comparison — the engine half of `standards lint --against`.
    fn transitions(
        base_files: Option<&[(&str, &str)]>,
        proposed_files: &[(&str, &str)],
    ) -> Vec<String> {
        let base = base_files.map(|files| {
            let (tmp, dir) = pack_with(PACK_TOML, files);
            let manifest = load_trusted(&dir);
            drop(tmp);
            manifest
        });
        let (_tmp, dir) = pack_with(PACK_TOML, proposed_files);
        let proposed = load_trusted(&dir);
        check_transitions(base.as_ref(), &proposed)
    }

    #[test]
    fn flight_rules_contract_transition_lint_refuses_absent_or_draft_to_enforced() {
        let enforced_rfc = RFC_MD.replace("status: approved", "status: enforced");
        let proposed = &[
            ("standards/RFC-001-zz-safety/rfc.md", enforced_rfc.as_str()),
            ("standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md", RULE_ONE),
        ];
        // Absent at the base (no standards at all).
        let errors = transitions(None, proposed);
        assert_eq!(errors.len(), 2, "RFC and rule both refuse: {errors:?}");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("RFC `RFC-001`") && e.contains("absent/draft")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("rule `ZZ-RULE-001`") && e.contains("absent")),
            "{errors:?}"
        );

        // Draft at the base.
        let draft_rfc = RFC_MD.replace("status: approved", "status: draft");
        let draft_rule = RULE_ONE.replace("checker: gate:zz-gate-one\n", "");
        let base = &[
            ("standards/RFC-001-zz-safety/rfc.md", draft_rfc.as_str()),
            (
                "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                draft_rule.as_str(),
            ),
        ];
        let errors = transitions(Some(base), proposed);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("RFC `RFC-001`") && e.contains("draft")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("rule `ZZ-RULE-001`") && e.contains("draft")),
            "{errors:?}"
        );
    }

    #[test]
    fn flight_rules_contract_transition_lint_refuses_semantic_change_without_revision_bump() {
        let changed = RULE_ONE.replace(
            "statement: zz synthetic must statement one.",
            "statement: zz REWRITTEN statement.",
        );
        let errors = transitions(
            Some(CORPUS),
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    changed.as_str(),
                ),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
            ],
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("rule `ZZ-RULE-001`"), "{errors:?}");
        assert!(
            errors[0].contains("statement"),
            "names the field: {errors:?}"
        );
        assert!(errors[0].contains("revision increment"), "{errors:?}");

        // The same change WITH a bump is the reviewed path — accepted.
        let bumped = changed.replace("revision: 1", "revision: 2");
        let errors = transitions(
            Some(CORPUS),
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    bumped.as_str(),
                ),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
            ],
        );
        assert!(errors.is_empty(), "{errors:?}");

        // A backwards revision is refused even with unchanged semantics.
        let base_bumped = RULE_ONE.replace("revision: 1", "revision: 5");
        let errors = transitions(
            Some(&[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    base_bumped.as_str(),
                ),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
            ]),
            CORPUS,
        );
        assert!(
            errors.iter().any(|e| e.contains("moved backwards")),
            "{errors:?}"
        );
    }

    #[test]
    fn flight_rules_contract_transition_lint_refuses_disappearing_ids_and_tombstone_reactivation() {
        // A rule present at the base is gone in the proposal.
        let errors = transitions(
            Some(CORPUS),
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md", RULE_ONE),
            ],
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("rule `ZZ-RULE-002`"), "{errors:?}");
        assert!(errors[0].contains("cannot disappear"), "{errors:?}");

        // A tombstone coming back active.
        let retired_base = RULE_ONE.replace("status: active", "status: retired");
        let errors = transitions(
            Some(&[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    retired_base.as_str(),
                ),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
            ]),
            CORPUS,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("rule `ZZ-RULE-001`") && e.contains("tombstone")),
            "{errors:?}"
        );
    }

    #[test]
    fn flight_rules_contract_transition_lint_accepts_reviewed_transitions() {
        // The designed path (D-B): approved at the base, enforced in the
        // proposal — the advisory period happened, so promotion is clean.
        let enforced_rfc = RFC_MD.replace("status: approved", "status: enforced");
        let errors = transitions(
            Some(CORPUS),
            &[
                ("standards/RFC-001-zz-safety/rfc.md", enforced_rfc.as_str()),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md", RULE_ONE),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
            ],
        );
        assert!(errors.is_empty(), "{errors:?}");

        // Retirement needs no revision bump; the tombstone stays.
        let retired = RULE_ONE.replace("status: active", "status: retired");
        let errors = transitions(
            Some(CORPUS),
            &[
                ("standards/RFC-001-zz-safety/rfc.md", RFC_MD),
                (
                    "standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md",
                    retired.as_str(),
                ),
                ("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md", RULE_TWO),
            ],
        );
        assert!(errors.is_empty(), "{errors:?}");

        // Unchanged corpus: no errors.
        let errors = transitions(Some(CORPUS), CORPUS);
        assert!(errors.is_empty(), "{errors:?}");
    }

    // ---- git-ref sourcing (the `--against` byte origin) ------------------

    /// A temp git repo holding the given files committed at HEAD; returns
    /// the TempDir, the repo root, and the opened GitRepo.
    fn git_repo_with_files(
        files: &[(String, String)],
    ) -> (tempfile::TempDir, PathBuf, crate::git_ops::GitRepo) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        for (rel, body) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "pack"]);
        let repo = crate::git_ops::GitRepo::open(&root).expect("git repo");
        (tmp, root, repo)
    }

    /// The fixture pack nested under `vendor/pack` inside the repo.
    fn vendored_files() -> Vec<(String, String)> {
        let mut files = vec![("vendor/pack/pack.toml".to_string(), PACK_TOML.to_string())];
        for (rel, body) in CORPUS {
            files.push((format!("vendor/pack/{rel}"), (*body).to_string()));
        }
        files
    }

    #[test]
    fn flight_rules_contract_load_at_ref_reads_tracked_blobs_not_the_worktree() {
        let (_tmp, root, repo) = git_repo_with_files(&vendored_files());

        // The base manifest comes from HEAD's tracked blobs; identical bytes
        // through the worktree source agree on the digest.
        let base = load_at_ref(&repo, "HEAD", "vendor/pack")
            .expect("base load")
            .expect("standards at HEAD");
        let worktree = crate::pack::Pack::load_with_trust(
            &root.join("vendor/pack"),
            StandardsTrust::RepoTracked,
        )
        .expect("worktree load")
        .expect("a pack");
        let worktree = worktree.standards.as_ref().unwrap();
        assert_eq!(base.digest, worktree.digest, "identical bytes agree");

        // …so an UNCOMMITTED worktree edit is invisible to the base read —
        // a mission branch cannot reshape the policy judging it (D-A).
        let edited = RULE_ONE.replace(
            "statement: zz synthetic must statement one.",
            "statement: zz WORKTREE-ONLY EDIT.",
        );
        std::fs::write(
            root.join("vendor/pack/standards/RFC-001-zz-safety/rules/ZZ-RULE-001.md"),
            &edited,
        )
        .unwrap();
        let base_after = load_at_ref(&repo, "HEAD", "vendor/pack")
            .expect("base load")
            .expect("standards at HEAD");
        assert_eq!(base.digest, base_after.digest, "the base is pinned blobs");
        let dirty = crate::pack::Pack::load_with_trust(
            &root.join("vendor/pack"),
            StandardsTrust::RepoTracked,
        )
        .expect("worktree load")
        .expect("a pack");
        let dirty = dirty.standards.as_ref().unwrap();
        assert_ne!(
            base_after.digest, dirty.digest,
            "the worktree moved; the base did not"
        );

        // The transition check composes: base from git, proposed from the
        // worktree, one refusal naming the rule and the missing bump.
        let errors = check_transitions(Some(&base_after), dirty);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("rule `ZZ-RULE-001`"), "{errors:?}");
        assert!(errors[0].contains("revision increment"), "{errors:?}");

        // A ref whose tree has no pack yields no base.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["rm", "-rqf", "vendor"]);
        git(&["commit", "-qm", "drop pack"]);
        assert_eq!(
            load_at_ref(&repo, "HEAD", "vendor/pack").expect("load"),
            None,
            "no pack at this ref"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flight_rules_contract_load_at_ref_refuses_a_tracked_symlink() {
        use std::os::unix::fs::symlink;
        let mut files = vendored_files();
        files.retain(|(p, _)| !p.ends_with("ZZ-RULE-002.md"));
        let (_tmp, root, repo) = git_repo_with_files(&files);
        // Commit a symlink where a rule file belongs (mode 120000 in the
        // tree) — the no-follow posture applies to tracked bytes too (D-J).
        symlink(
            "ZZ-RULE-001.md",
            root.join("vendor/pack/standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md"),
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
        git(&["commit", "-qm", "add symlink"]);
        let err = load_at_ref(&repo, "HEAD", "vendor/pack").expect_err("must fail");
        assert!(err.contains("symlink"), "{err}");
        assert!(err.contains("ZZ-RULE-002.md"), "names the path: {err}");
    }

    #[test]
    fn flight_rules_contract_trust_for_dir_distinguishes_repo_tracked_from_external() {
        let (_tmp, root, _repo) = git_repo_with_files(&vendored_files());
        // A tracked, in-repo pack earns RepoTracked…
        assert_eq!(
            trust_for_dir(&root, &root.join("vendor/pack")),
            StandardsTrust::RepoTracked
        );
        // …an in-repo but UNTRACKED pack does not (no base history)…
        std::fs::create_dir_all(root.join("scratch/pack")).unwrap();
        std::fs::write(root.join("scratch/pack/pack.toml"), PACK_TOML).unwrap();
        assert_eq!(
            trust_for_dir(&root, &root.join("scratch/pack")),
            StandardsTrust::External
        );
        // …and neither does a pack outside the repo entirely.
        let outside = tempfile::tempdir().unwrap();
        let pack = outside.path().join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join(PACK_MANIFEST), PACK_TOML).unwrap();
        assert_eq!(
            trust_for_dir(&root, &pack),
            StandardsTrust::External,
            "outside the repo is external"
        );
    }

    // ---- hostile filesystem shapes (unix: symlinks, FIFOs) ---------------

    #[cfg(unix)]
    #[test]
    fn flight_rules_contract_symlinked_parents_and_leaves_fail_no_follow() {
        use std::os::unix::fs::symlink;
        // A symlinked RULE FILE (leaf).
        let (_tmp, dir) = synthetic_pack();
        let target = dir.join("outside.md");
        std::fs::write(&target, RULE_ONE).unwrap();
        let link = dir.join("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md");
        std::fs::remove_file(&link).unwrap();
        symlink(&target, &link).unwrap();
        let err = crate::pack::Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("symlink"), "{err}");
        assert!(err.contains("never follows symlinks"), "{err}");

        // A symlinked RFC DIRECTORY (parent).
        let (_tmp2, dir2) = synthetic_pack();
        let real = dir2.join("real-rfc");
        std::fs::rename(dir2.join("standards/RFC-001-zz-safety"), &real).unwrap();
        symlink(&real, dir2.join("standards/RFC-001-zz-safety")).unwrap();
        let err = crate::pack::Pack::load(&dir2).expect_err("must fail");
        assert!(err.contains("symlink"), "{err}");

        // A FIFO in place of a rule file fails PROMPTLY (never blocks on open).
        let (_tmp3, dir3) = synthetic_pack();
        let fifo = dir3.join("standards/RFC-001-zz-safety/rules/ZZ-RULE-002.md");
        std::fs::remove_file(&fifo).unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("spawn mkfifo");
        assert!(status.success());
        let err = crate::pack::Pack::load(&dir3).expect_err("must fail");
        assert!(err.contains("not a regular file"), "{err}");
        assert!(err.contains("ZZ-RULE-002.md"), "names the file: {err}");

        // A stray file directly under the root, and a subdirectory under
        // rules/, are both refused naming the shape.
        let (_tmp4, dir4) = synthetic_pack();
        std::fs::write(dir4.join("standards/notes.txt"), "stray").unwrap();
        let err = crate::pack::Pack::load(&dir4).expect_err("must fail");
        assert!(err.contains("not an RFC directory"), "{err}");
        let (_tmp5, dir5) = synthetic_pack();
        std::fs::create_dir_all(dir5.join("standards/RFC-001-zz-safety/rules/nested")).unwrap();
        let err = crate::pack::Pack::load(&dir5).expect_err("must fail");
        assert!(err.contains("no nested directories"), "{err}");
    }
}
