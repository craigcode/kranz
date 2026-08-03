//! The pack contract (ticket `.kranz/tickets/pack-contract-gates-prompts.md`,
//! KRZ-313 series): a pack — a directory with a `pack.toml` — declares
//! DETERMINISTIC gates, role prompts, checklists, and artefact-store adapters
//! that kranz validates at load and wires into the mission surfaces.
//!
//! This module carries the repo's IP boundary: kranz core stays domain-free,
//! and domain knowledge (house standards, review lenses, evidence stores)
//! ships in private packs. The contract EXTENDS the existing pack concept
//! (`packaging/gascity/pack.toml`, schema 2, docs/gascity-citizenship.md)
//! rather than adding a second mechanism: the schema-2 base manifest
//! (`[pack]` name + schema) is a valid pack that simply registers nothing,
//! and schema 3 adds the declaration sections below.
//!
//! WHY validation fails closed: every consuming surface (the final gate, the
//! role-prompt builders, `kranz pack lint`) loads through the same strict
//! path, and ANY violation — an unknown field, a wrong type, a missing
//! required key, an empty gate command, a duplicate name, a model-judged
//! gate kind, an engine-reserved gate name — is a load error naming the
//! offending field, never a silently-skipped section. A pack the operator
//! configured but kranz cannot fully account for must not quietly degrade
//! back to pack-less behavior: the operator believes its gates run.
//!
//! WHY gates compose AFTER the engine floor: a pack can add to the floor,
//! never lower, reorder, or replace it. At the final gate the orchestrator
//! registers the engine floor gates ([`crate::contract_gates`]) into ONE
//! [`GatePipeline`] FIRST and pack gates after — registration order IS the
//! evaluation order within the deterministic section (gate.rs), so the
//! composition is the guarantee, not a convention. This module closes the
//! one remaining hole: a pack gate NAMED like an engine floor gate (which
//! would be indistinguishable in reports) is refused at load against
//! [`RESERVED_GATE_NAMES`]. Model-judged gates stay engine-only in this
//! slice — a pack declaring `kind = "model-judged"` is refused at load.
//!
//! WHY checklists and artefact stores are declaration-only: this slice
//! validates their declarations at load and reports them (lint, run-start
//! decision) but never EXECUTES them — no checklist is checked and no
//! artefact adapter is invoked. Declaring the shapes now means the future
//! slices that consume them need no schema rework; executing them is
//! deliberately out of scope.
//!
//! WHY [`PackGate`] carries a pre-computed outcome: [`Gate::evaluate`] is
//! synchronous by design (gates capture everything they need at
//! construction), while the engine's bounded shell runner
//! ([`crate::command_exec::run_shell_command`]) is async. The orchestrator
//! therefore runs each pack gate's command at REGISTRATION time — same
//! cleared contract env, same active root as the contract assertions — and
//! the gate captures the outcome; the pipeline still owns ordering and
//! reporting, so a pack gate flows through it exactly like a live
//! evaluation. This mirrors [`crate::merge_gate::MergeSuiteGate`]'s
//! capture-at-construction contract.
//!
//! No pack configured ⇒ `load_for_config` returns `Ok(None)` and every
//! surface behaves byte-identically to a pack-less engine.

mod toml;

use crate::gate::{ArtefactRef, Gate, GateKind, GateOutcome};
use crate::types::{MissionConfig, Role};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// The manifest file name inside a pack directory.
pub const PACK_MANIFEST: &str = "pack.toml";

/// The pre-existing base manifest version (`packaging/gascity`): `[pack]`
/// name + schema only. A valid pack that registers nothing by convention
/// (declaration sections are honored uniformly if present).
pub const SCHEMA_BASE: u32 = 2;

/// The current contract version: the base manifest plus the `[[gate]]`,
/// `[[prompt]]`, `[[checklist]]`, and `[[artefact_store]]` sections.
pub const SCHEMA_CONTRACT: u32 = 3;

/// Engine gate names a pack gate may never claim. The first four are the
/// contract-defect floor gates ([`crate::contract_gates`]); `merge-gate-suite`
/// is the repo-owned merge gate ([`crate::merge_gate::MergeSuiteGate`]).
/// Sharing a name would make a pack verdict indistinguishable from a floor
/// verdict in every report — the one way a pack could appear to displace the
/// floor — so it fails closed at load.
pub const RESERVED_GATE_NAMES: &[&str] = &[
    crate::contract_gates::VACUOUS_FILTER,
    crate::contract_gates::WRONG_POLARITY,
    crate::contract_gates::PASSES_ON_BASE,
    crate::contract_gates::ENV_SENSITIVE,
    "merge-gate-suite",
];

/// A validated pack: the manifest's declarations, load-resolved (prompt
/// `textFile`s already read) and ready for the consuming surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub name: String,
    pub schema: u32,
    /// The directory the manifest was loaded from (textFile resolution root,
    /// reported by lint and the run-start decision).
    pub dir: PathBuf,
    pub gates: Vec<PackGateDecl>,
    pub prompts: Vec<PackPrompt>,
    pub checklists: Vec<PackChecklist>,
    pub artefact_stores: Vec<PackArtefactStore>,
}

/// One declared deterministic gate. Runs at the final gate (advisory, like
/// the engine floor gates) against the active tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackGateDecl {
    pub name: String,
    pub command: String,
    /// Merge-gate-idiom scoping: empty runs unconditionally; otherwise the
    /// gate runs when at least one changed path equals or sits below a
    /// prefix. Normalized (`.` components stripped) at load.
    pub when_paths: Vec<String>,
}

/// One declared prompt: text appended to the target role's rendered prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackPrompt {
    pub name: String,
    pub role: Role,
    /// The resolved text (inline `text` verbatim, or `textFile` read at load).
    pub text: String,
    /// Where the text came from, for the lint surface.
    pub source: PromptSource,
}

/// How a prompt's text was declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptSource {
    Inline,
    File(String),
}

/// One declared checklist. DECLARATION-ONLY in this slice: validated at
/// load, never executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackChecklist {
    pub name: String,
    pub items: Vec<String>,
}

/// One declared artefact-store adapter. DECLARATION-ONLY in this slice:
/// validated at load, never invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackArtefactStore {
    pub name: String,
    pub kind: String,
}

impl Pack {
    /// Load and validate the pack at `dir`. `Ok(None)` means the directory
    /// is not a pack (no `pack.toml`) — the lint surface says so plainly;
    /// config-pointed loads ([`load_for_config`]) turn it into an error.
    /// Any contract violation is an `Err` naming the offending field.
    pub fn load(dir: &Path) -> Result<Option<Pack>, String> {
        let manifest_path = dir.join(PACK_MANIFEST);
        let source = match std::fs::read_to_string(&manifest_path) {
            Ok(source) => source,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", manifest_path.display())),
        };
        Self::parse(dir, &source).map(Some)
    }

    /// Parse and validate a manifest's text (the load path minus the file
    /// read, so tests exercise the identical validation).
    pub fn parse(dir: &Path, source: &str) -> Result<Pack, String> {
        let doc = toml::parse(source).map_err(|e| format!("{PACK_MANIFEST}: {e}"))?;
        Self::from_document(dir, &doc)
    }

    /// Semantic validation over the parsed document: strict per-section
    /// fields, types, uniqueness, and the engine-name reservation.
    fn from_document(dir: &Path, doc: &toml::Document) -> Result<Pack, String> {
        // Unknown SECTIONS fail closed too — a mistyped `[[gates]]` must not
        // silently register nothing.
        for section in &doc.sections {
            let name = match section {
                toml::Section::Single(t) => t.name.as_str(),
                toml::Section::Array { name, .. } => name.as_str(),
            };
            if !["pack", "gate", "prompt", "checklist", "artefact_store"].contains(&name) {
                return Err(format!(
                    "{PACK_MANIFEST}: unknown section `{name}` (declared sections: [pack], \
                     [[gate]], [[prompt]], [[checklist]], [[artefact_store]])"
                ));
            }
        }

        let header = doc
            .single("pack")
            .ok_or_else(|| format!("{PACK_MANIFEST}: missing required table `[pack]`"))?;
        check_unknown(header, "[pack]", &["name", "schema"])?;
        let name = required_string(header, "[pack]", "name")?;
        let schema = match header.get("schema") {
            Some(toml::Value::Integer(n)) => {
                let n = *n;
                if n == i64::from(SCHEMA_BASE) || n == i64::from(SCHEMA_CONTRACT) {
                    n as u32
                } else {
                    return Err(format!(
                        "[pack] field `schema` is {n}: supported versions are {SCHEMA_BASE} \
                         (base manifest) and {SCHEMA_CONTRACT} (contract)"
                    ));
                }
            }
            Some(v) => {
                return Err(format!(
                    "[pack] field `schema` must be an integer, got {}",
                    v.type_name()
                ))
            }
            None => return Err("[pack] is missing required field `schema`".to_string()),
        };

        let mut gates = Vec::new();
        for (idx, item) in doc.array("gate").iter().enumerate() {
            gates.push(load_gate(item, idx)?);
        }
        reject_duplicate_names("gate", gates.iter().map(|g| g.name.as_str()))?;

        let mut prompts = Vec::new();
        for (idx, item) in doc.array("prompt").iter().enumerate() {
            prompts.push(load_prompt(item, idx, dir)?);
        }
        reject_duplicate_names("prompt", prompts.iter().map(|p| p.name.as_str()))?;

        let mut checklists = Vec::new();
        for (idx, item) in doc.array("checklist").iter().enumerate() {
            checklists.push(load_checklist(item, idx)?);
        }
        reject_duplicate_names("checklist", checklists.iter().map(|c| c.name.as_str()))?;

        let mut artefact_stores = Vec::new();
        for (idx, item) in doc.array("artefact_store").iter().enumerate() {
            artefact_stores.push(load_artefact_store(item, idx)?);
        }
        reject_duplicate_names(
            "artefact_store",
            artefact_stores.iter().map(|s| s.name.as_str()),
        )?;

        Ok(Pack {
            name,
            schema,
            dir: dir.to_path_buf(),
            gates,
            prompts,
            checklists,
            artefact_stores,
        })
    }

    /// The gates applicable to a diff: unconditional gates plus those whose
    /// `whenPaths` match at least one changed path (the merge-gate idiom,
    /// [`crate::merge_gate::when_paths_match`]).
    pub fn gates_for_paths(&self, changed_paths: &[String]) -> Vec<&PackGateDecl> {
        self.gates
            .iter()
            .filter(|g| crate::merge_gate::when_paths_match(&g.when_paths, changed_paths))
            .collect()
    }

    /// The prompt block appended to `role`'s rendered prompt: one marked
    /// section per pack prompt targeting the role, in declared order. Empty
    /// when no prompt targets the role — the caller leaves the role prompt
    /// (and its recorded hash) byte-identical.
    pub fn prompt_section(&self, role: Role) -> String {
        let mut section = String::new();
        for prompt in self.prompts.iter().filter(|p| p.role == role) {
            section.push_str(&format!(
                "\n\n---\nPack guidance (pack `{}`, prompt `{}`):\n{}\n",
                self.name,
                prompt.name,
                prompt.text.trim_end()
            ));
        }
        section
    }

    /// One compact line naming everything the pack registered — the
    /// run-start audit decision and the lint headline share it.
    pub fn describe(&self) -> String {
        let gate_names = self
            .gates
            .iter()
            .map(|g| g.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let prompt_names = self
            .prompts
            .iter()
            .map(|p| format!("{}→{}", p.name, role_target_name(p.role)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "pack `{}` (schema {}) at {}: {} gate(s) [{}], {} prompt(s) [{}], \
             {} checklist(s), {} artefact store(s) (checklists/stores are \
             declaration-only: validated at load, never executed)",
            self.name,
            self.schema,
            self.dir.display(),
            self.gates.len(),
            gate_names,
            self.prompts.len(),
            prompt_names,
            self.checklists.len(),
            self.artefact_stores.len(),
        )
    }
}

/// Load the pack a mission config points at (`packDir`, repo-relative when
/// not absolute). No key ⇒ `Ok(None)` — the byte-identical pack-less path.
/// A key pointing at a non-directory or a non-pack is a misconfiguration
/// and fails closed, exactly like an invalid manifest.
pub fn load_for_config(cfg: &MissionConfig, repo_root: &Path) -> Result<Option<Pack>, String> {
    let Some(configured) = cfg.pack_dir.as_deref() else {
        return Ok(None);
    };
    let raw = Path::new(configured);
    let dir = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    };
    if !dir.is_dir() {
        return Err(format!(
            "packDir `{configured}` resolves to {}, which is not a directory",
            dir.display()
        ));
    }
    let Some(pack) = Pack::load(&dir)? else {
        return Err(format!(
            "packDir `{configured}` resolves to {}, which has no {PACK_MANIFEST} — \
             it is not a pack",
            dir.display()
        ));
    };
    Ok(Some(pack))
}

/// The multi-line lint report: what the pack registered, with the posture
/// of each section stated (advisory gates, declaration-only sections).
pub fn render_lint(pack: &Pack) -> String {
    let mut out = format!(
        "pack `{}` (schema {}) at {} — valid\n",
        pack.name,
        pack.schema,
        pack.dir.display()
    );
    out.push_str("gates (deterministic; final-gate, after the engine floor, advisory):\n");
    if pack.gates.is_empty() {
        out.push_str("  (none)\n");
    }
    for gate in &pack.gates {
        let scoping = if gate.when_paths.is_empty() {
            "unconditional".to_string()
        } else {
            format!("whenPaths: {}", gate.when_paths.join(", "))
        };
        out.push_str(&format!(
            "  - {}: `{}` ({scoping})\n",
            gate.name, gate.command
        ));
    }
    out.push_str("prompts (appended to the target role's prompt):\n");
    if pack.prompts.is_empty() {
        out.push_str("  (none)\n");
    }
    for prompt in &pack.prompts {
        let source = match &prompt.source {
            PromptSource::Inline => "inline text".to_string(),
            PromptSource::File(rel) => format!("file {rel}"),
        };
        out.push_str(&format!(
            "  - {} → {} ({source})\n",
            prompt.name,
            role_target_name(prompt.role)
        ));
    }
    out.push_str("checklists (declaration-only: validated at load, never executed):\n");
    if pack.checklists.is_empty() {
        out.push_str("  (none)\n");
    }
    for checklist in &pack.checklists {
        out.push_str(&format!(
            "  - {} ({} item(s))\n",
            checklist.name,
            checklist.items.len()
        ));
    }
    out.push_str("artefact stores (declaration-only: validated at load, never invoked):\n");
    if pack.artefact_stores.is_empty() {
        out.push_str("  (none)\n");
    }
    for store in &pack.artefact_stores {
        out.push_str(&format!("  - {} (kind `{}`)\n", store.name, store.kind));
    }
    out
}

/// The pack-facing name of a prompt role target (the manifest vocabulary).
pub fn role_target_name(role: Role) -> &'static str {
    match role {
        Role::Worker => "worker",
        Role::ValidatorScrutiny => "validator-scrutiny",
        Role::ValidatorFunctional => "validator-functional",
        Role::Orchestrator => "orchestrator",
    }
}

/// A pack-declared deterministic gate adapted to the first-class
/// [`crate::gate::Gate`] interface, registered into the final gate's shared
/// pipeline AFTER the engine floor gates. The outcome is captured at
/// construction (see the module docs for the async/sync bridge); the gate
/// is boolean-only — it reports no confidence score.
pub struct PackGate {
    name: String,
    outcome: GateOutcome,
}

impl PackGate {
    /// Build the gate from its command's already-completed bounded run:
    /// `ok`/`output` are the engine shell runner's result for `command`.
    /// The artefact mirrors [`crate::merge_gate::MergeSuiteGate`]: the
    /// command line is the reference, and a failure carries the output tail.
    pub fn from_run(name: &str, command: &str, ok: bool, output: String) -> Self {
        let artefact = ArtefactRef::new(command.to_string());
        let outcome = if ok {
            GateOutcome::pass(artefact)
        } else {
            GateOutcome::fail(artefact.with_detail(output))
        };
        Self {
            name: name.to_string(),
            outcome,
        }
    }
}

impl Gate for PackGate {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> GateKind {
        GateKind::Deterministic
    }

    fn evaluate(&self) -> GateOutcome {
        self.outcome.clone()
    }
}

// ---------------------------------------------------------------------------
// Per-section validation
// ---------------------------------------------------------------------------

/// A stable label for one `[[section]]` item, carrying its declared name
/// when readable so errors point at the entry AND the field.
fn entry_label(section: &str, index: usize, table: &toml::Table) -> String {
    match table.get("name") {
        Some(toml::Value::String(name)) => {
            format!("[[{section}]] entry {} (name `{name}`)", index + 1)
        }
        _ => format!("[[{section}]] entry {}", index + 1),
    }
}

/// Refuse any key the section's contract does not declare (the
/// unknown-field failure class).
fn check_unknown(table: &toml::Table, section: &str, known: &[&str]) -> Result<(), String> {
    let unknown = table.unknown_keys(known);
    if let Some(field) = unknown.first() {
        return Err(format!(
            "{section} has unknown field `{field}` (declared fields: {})",
            known.join(", ")
        ));
    }
    Ok(())
}

/// A required, non-empty string field.
fn required_string(table: &toml::Table, section: &str, key: &str) -> Result<String, String> {
    match table.get(key) {
        Some(toml::Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
        Some(toml::Value::String(_)) => Err(format!(
            "{section} field `{key}` must be a non-empty string"
        )),
        Some(v) => Err(format!(
            "{section} field `{key}` must be a string, got {}",
            v.type_name()
        )),
        None => Err(format!("{section} is missing required field `{key}`")),
    }
}

/// An optional string field (absent ⇒ None; present-but-wrong-type ⇒ Err).
fn optional_string(
    table: &toml::Table,
    section: &str,
    key: &str,
) -> Result<Option<String>, String> {
    match table.get(key) {
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(v) => Err(format!(
            "{section} field `{key}` must be a string, got {}",
            v.type_name()
        )),
        None => Ok(None),
    }
}

/// An optional array-of-non-empty-strings field (absent ⇒ empty vec).
fn optional_string_array(
    table: &toml::Table,
    section: &str,
    key: &str,
) -> Result<Vec<String>, String> {
    match table.get(key) {
        Some(toml::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                match item {
                    toml::Value::String(s) if !s.trim().is_empty() => out.push(s.clone()),
                    toml::Value::String(_) => {
                        return Err(format!(
                            "{section} field `{key}` element {} must be a non-empty string",
                            idx + 1
                        ))
                    }
                    v => {
                        return Err(format!(
                            "{section} field `{key}` element {} must be a string, got {}",
                            idx + 1,
                            v.type_name()
                        ))
                    }
                }
            }
            Ok(out)
        }
        Some(v) => Err(format!(
            "{section} field `{key}` must be an array of strings, got {}",
            v.type_name()
        )),
        None => Ok(Vec::new()),
    }
}

/// `[[gate]]`: name, command, optional kind (deterministic only) and
/// whenPaths (merge-gate idiom).
fn load_gate(table: &toml::Table, index: usize) -> Result<PackGateDecl, String> {
    let section = entry_label("gate", index, table);
    check_unknown(table, &section, &["name", "kind", "command", "whenPaths"])?;
    let name = required_string(table, &section, "name")?;
    if let Some(kind) = optional_string(table, &section, "kind")? {
        if kind != "deterministic" {
            return Err(format!(
                "{section} field `kind` is `{kind}`: packs may register only deterministic \
                 gates in this slice — model-judged gates are declared by the engine, \
                 never by a pack"
            ));
        }
    }
    if RESERVED_GATE_NAMES.contains(&name.as_str()) {
        return Err(format!(
            "{section} field `name` is `{name}`: reserved for an engine floor gate — \
             a pack can add gates after the floor, never impersonate it"
        ));
    }
    let command = required_string(table, &section, "command")?;
    if command.contains(['\n', '\r', '\0']) {
        return Err(format!(
            "{section} field `command` must be a single non-NUL line"
        ));
    }
    let mut when_paths = Vec::new();
    for raw in optional_string_array(table, &section, "whenPaths")? {
        validate_pack_relative_path(&raw, &section, "whenPaths")?;
        let normalized = crate::merge_gate::normalize_relative_path(&raw, false);
        if normalized.is_empty() || normalized == "." {
            return Err(format!(
                "{section} field `whenPaths` entries must name a repo path — \
                 omit whenPaths to run unconditionally"
            ));
        }
        when_paths.push(normalized);
    }
    Ok(PackGateDecl {
        name,
        command,
        when_paths,
    })
}

/// `[[prompt]]`: name, role target, exactly one of text / textFile. The
/// text is resolved AT LOAD (files read once, here) so every consuming
/// surface sees identical bytes or the load fails closed.
fn load_prompt(table: &toml::Table, index: usize, pack_dir: &Path) -> Result<PackPrompt, String> {
    let section = entry_label("prompt", index, table);
    check_unknown(table, &section, &["name", "role", "text", "textFile"])?;
    let name = required_string(table, &section, "name")?;
    let role_raw = required_string(table, &section, "role")?;
    let role = match role_raw.as_str() {
        "worker" => Role::Worker,
        "validator-scrutiny" => Role::ValidatorScrutiny,
        "validator-functional" => Role::ValidatorFunctional,
        other => {
            return Err(format!(
                "{section} field `role` is `{other}`: supported targets are `worker`, \
                 `validator-scrutiny`, `validator-functional` (the session roles whose \
                 prompts runner.rs builds)"
            ))
        }
    };
    let inline = optional_string(table, &section, "text")?;
    let file = optional_string(table, &section, "textFile")?;
    let (text, source) = match (inline, file) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "{section} declares both `text` and `textFile` — exactly one is required"
            ))
        }
        (None, None) => {
            return Err(format!(
                "{section} is missing required field `text` (or `textFile`)"
            ))
        }
        (Some(text), None) => (text, PromptSource::Inline),
        (None, Some(rel)) => {
            validate_pack_relative_path(&rel, &section, "textFile")?;
            let normalized = crate::merge_gate::normalize_relative_path(&rel, false);
            let text = read_pack_text_file_nofollow(pack_dir, &normalized, &section)?;
            (text, PromptSource::File(normalized))
        }
    };
    if text.trim().is_empty() {
        return Err(format!(
            "{section} field `text` resolves to empty prompt text"
        ));
    }
    Ok(PackPrompt {
        name,
        role,
        text,
        source,
    })
}

/// `[[checklist]]`: name + non-empty items. Declaration-only.
fn load_checklist(table: &toml::Table, index: usize) -> Result<PackChecklist, String> {
    let section = entry_label("checklist", index, table);
    check_unknown(table, &section, &["name", "items"])?;
    let name = required_string(table, &section, "name")?;
    if table.get("items").is_none() {
        return Err(format!("{section} is missing required field `items`"));
    }
    let items = optional_string_array(table, &section, "items")?;
    if items.is_empty() {
        return Err(format!(
            "{section} field `items` must list at least one item"
        ));
    }
    Ok(PackChecklist { name, items })
}

/// `[[artefact_store]]`: name + kind. Declaration-only.
fn load_artefact_store(table: &toml::Table, index: usize) -> Result<PackArtefactStore, String> {
    let section = entry_label("artefact_store", index, table);
    check_unknown(table, &section, &["name", "kind"])?;
    let name = required_string(table, &section, "name")?;
    let kind = required_string(table, &section, "kind")?;
    Ok(PackArtefactStore { name, kind })
}

/// Duplicate names within one section are a load error (the
/// duplicate-name failure class): reports and lint name entries, so a
/// collision would make two registrations indistinguishable.
fn reject_duplicate_names<'a>(
    section: &str,
    names: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(format!("duplicate [[{section}]] name `{name}`"));
        }
    }
    Ok(())
}

/// A pack-relative path (textFile, whenPaths entry): non-empty, not
/// absolute, no parent/root components — the same shape the merge-gate
/// suite demands of its paths, with pack-worded errors.
fn validate_pack_relative_path(raw: &str, section: &str, field: &str) -> Result<(), String> {
    let path = Path::new(raw);
    if raw.trim().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::CurDir | Component::Normal(_)))
    {
        return Err(format!(
            "{section} field `{field}` must be a pack-relative path without parent \
             components: {raw:?}"
        ));
    }
    Ok(())
}

/// Read a `[[prompt]]` `textFile` NO-FOLLOW from a pinned pack-directory
/// capability (12th-pass review): lexical validation
/// ([`validate_pack_relative_path`]) only sees path COMPONENTS, so a
/// textFile that is a symlink — or that resolves through a symlinked
/// parent directory — could load an engine-readable secret from outside
/// the pack and ship it to a remote model as prompt text. The pack dir is
/// the operator-chosen anchor (opened ambient — the same trust basis the
/// engine uses for the repo root in [`crate::paths`]); every parent
/// component is opened `open_dir_nofollow` and the leaf with
/// `FollowSymlinks::No`, so a symlink anywhere below the anchor is REFUSED
/// with an error naming the field, never followed. `rel` reaches here
/// already normalized ([`crate::merge_gate::normalize_relative_path`]
/// keeps only `Normal` components), so splitting on '/' yields plain
/// names.
fn read_pack_text_file_nofollow(
    pack_dir: &Path,
    rel: &str,
    section: &str,
) -> Result<String, String> {
    use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
    use std::io::Read as _;

    let display = pack_dir.join(rel);
    let field_error = |message: String| format!("{section} field `textFile` = {rel:?} {message}");
    let no_follow_refusal = |what: &str| {
        field_error(format!(
            "resolves through {what} ({}) — pack prompt files load no-follow so a pack \
             cannot read outside its own directory",
            display.display()
        ))
    };
    let mut dir = cap_std::fs::Dir::open_ambient_dir(pack_dir, cap_std::ambient_authority())
        .map_err(|e| field_error(format!("cannot open pack dir {}: {e}", pack_dir.display())))?;
    let mut names = rel.split('/').peekable();
    while let Some(name) = names.next() {
        if names.peek().is_some() {
            dir = dir
                .open_dir_nofollow(name)
                .map_err(|_| no_follow_refusal("a symlinked or non-directory component"))?;
        } else {
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let mut file = dir.open_with(name, &options).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    field_error(format!("cannot be read at {}: {e}", display.display()))
                } else {
                    no_follow_refusal("a symlink or other non-regular file")
                }
            })?;
            let mut text = String::new();
            file.read_to_string(&mut text).map_err(|e| {
                field_error(format!("cannot be read at {}: {e}", display.display()))
            })?;
            return Ok(text);
        }
    }
    // Unreachable: validation guarantees a non-empty path of Normal
    // components — but fail closed rather than panic if that ever changes.
    Err(field_error("resolves to no file".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::GatePipeline;

    /// A pack directory in a tempdir; returns the TempDir (kept alive by
    /// the caller) and the pack dir inside it.
    fn pack_dir_with(manifest: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
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

    const FULL_MANIFEST: &str = r#"
[pack]
name = "zz-synthetic-pack"
schema = 3

[[gate]]
name = "zz-gate-one"
command = "cd ."

[[gate]]
name = "zz-gate-two"
command = "cd ."
whenPaths = ["src/"]

[[prompt]]
name = "zz-prompt-worker"
role = "worker"
text = "zz inline worker guidance"

[[prompt]]
name = "zz-prompt-scrutiny"
role = "validator-scrutiny"
textFile = "prompts/scrutiny.md"

[[checklist]]
name = "zz-checklist"
items = ["first", "second"]

[[artefact_store]]
name = "zz-store"
kind = "local-dir"
"#;

    #[test]
    fn pack_contract_full_pack_loads_all_sections() {
        let (_tmp, dir) =
            pack_dir_with(FULL_MANIFEST, &[("prompts/scrutiny.md", "zz file text\n")]);
        let pack = Pack::load(&dir).expect("load").expect("a pack");
        assert_eq!(pack.name, "zz-synthetic-pack");
        assert_eq!(pack.schema, SCHEMA_CONTRACT);
        assert_eq!(pack.gates.len(), 2);
        assert_eq!(pack.gates[1].when_paths, vec!["src".to_string()]);
        assert_eq!(pack.prompts.len(), 2);
        assert_eq!(pack.prompts[1].text, "zz file text\n");
        assert_eq!(
            pack.prompts[1].source,
            PromptSource::File("prompts/scrutiny.md".to_string())
        );
        assert_eq!(pack.checklists[0].items.len(), 2);
        assert_eq!(pack.artefact_stores[0].kind, "local-dir");
    }

    /// The existing concept (schema 2, `[pack]` only) loads and registers
    /// nothing — extension, not a second mechanism.
    #[test]
    fn pack_contract_schema_two_base_manifest_registers_nothing() {
        let (_tmp, dir) = pack_dir_with("[pack]\nname = \"kranz\"\nschema = 2\n", &[]);
        let pack = Pack::load(&dir).expect("load").expect("a pack");
        assert_eq!(pack.schema, SCHEMA_BASE);
        assert!(pack.gates.is_empty());
        assert!(pack.prompts.is_empty());
        assert!(pack.checklists.is_empty());
        assert!(pack.artefact_stores.is_empty());
    }

    #[test]
    fn pack_contract_directory_without_manifest_is_not_a_pack() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(Pack::load(tmp.path()).expect("load"), None);
    }

    // ---- failure classes, one test each, each naming the field ---------

    #[test]
    fn pack_contract_unknown_field_fails_closed() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[gate]]\nname = \"g\"\ncommand = \"true\"\nbogus = 1\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("unknown field `bogus`"), "{err}");
        assert!(err.contains("[[gate]]"), "{err}");
    }

    #[test]
    fn pack_contract_unknown_section_fails_closed() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[gates]]\nname = \"g\"\ncommand = \"true\"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("unknown section `gates`"), "{err}");
    }

    #[test]
    fn pack_contract_missing_required_key_fails_closed() {
        // gate without command
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[gate]]\nname = \"g\"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("missing required field `command`"), "{err}");

        // [pack] without schema
        let (_tmp2, dir2) = pack_dir_with("[pack]\nname = \"x\"\n", &[]);
        let err = Pack::load(&dir2).expect_err("must fail");
        assert!(err.contains("missing required field `schema`"), "{err}");
    }

    #[test]
    fn pack_contract_wrong_type_fails_closed() {
        let (_tmp, dir) = pack_dir_with("[pack]\nname = \"x\"\nschema = \"3\"\n", &[]);
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("field `schema` must be an integer"), "{err}");

        let (_tmp2, dir2) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[gate]]\nname = \"g\"\ncommand = \"true\"\nwhenPaths = \"src\"\n",
            &[],
        );
        let err = Pack::load(&dir2).expect_err("must fail");
        assert!(
            err.contains("field `whenPaths` must be an array of strings"),
            "{err}"
        );
    }

    #[test]
    fn pack_contract_duplicate_name_fails_closed() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[gate]]\nname = \"g\"\ncommand = \"true\"\n\n[[gate]]\nname = \"g\"\ncommand = \"false\"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("duplicate [[gate]] name `g`"), "{err}");
    }

    #[test]
    fn pack_contract_model_judged_gate_kind_refused() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[gate]]\nname = \"g\"\ncommand = \"true\"\nkind = \"model-judged\"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("field `kind` is `model-judged`"), "{err}");
        assert!(err.contains("deterministic"), "{err}");
    }

    #[test]
    fn pack_contract_engine_floor_gate_names_are_reserved() {
        for reserved in RESERVED_GATE_NAMES {
            let manifest = format!(
                "[pack]\nname = \"x\"\nschema = 3\n\n[[gate]]\nname = \"{reserved}\"\ncommand = \"true\"\n"
            );
            let (_tmp, dir) = pack_dir_with(&manifest, &[]);
            let err = Pack::load(&dir).expect_err("must fail");
            assert!(err.contains("reserved for an engine floor gate"), "{err}");
        }
    }

    #[test]
    fn pack_contract_empty_gate_command_fails_closed() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[gate]]\nname = \"g\"\ncommand = \"  \"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(
            err.contains("field `command` must be a non-empty string"),
            "{err}"
        );
    }

    #[test]
    fn pack_contract_unsupported_schema_fails_closed() {
        let (_tmp, dir) = pack_dir_with("[pack]\nname = \"x\"\nschema = 4\n", &[]);
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("field `schema` is 4"), "{err}");
    }

    #[test]
    fn pack_contract_prompt_text_and_textfile_are_exclusive() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[prompt]]\nname = \"p\"\nrole = \"worker\"\ntext = \"t\"\ntextFile = \"p.md\"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("both `text` and `textFile`"), "{err}");

        let (_tmp2, dir2) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[prompt]]\nname = \"p\"\nrole = \"worker\"\n",
            &[],
        );
        let err = Pack::load(&dir2).expect_err("must fail");
        assert!(err.contains("missing required field `text`"), "{err}");
    }

    #[test]
    fn pack_contract_prompt_role_must_target_a_session_role() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[prompt]]\nname = \"p\"\nrole = \"orchestrator\"\ntext = \"t\"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("field `role` is `orchestrator`"), "{err}");
    }

    #[test]
    fn pack_contract_prompt_textfile_must_stay_inside_the_pack() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[prompt]]\nname = \"p\"\nrole = \"worker\"\ntextFile = \"../escape.md\"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(
            err.contains("field `textFile` must be a pack-relative path"),
            "{err}"
        );

        let (_tmp2, dir2) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[prompt]]\nname = \"p\"\nrole = \"worker\"\ntextFile = \"missing.md\"\n",
            &[],
        );
        let err = Pack::load(&dir2).expect_err("must fail");
        assert!(
            err.contains("field `textFile` = \"missing.md\" cannot be read"),
            "{err}"
        );
    }

    #[test]
    fn pack_contract_checklist_requires_items() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[checklist]]\nname = \"c\"\n",
            &[],
        );
        let err = Pack::load(&dir).expect_err("must fail");
        assert!(err.contains("missing required field `items`"), "{err}");
    }

    // ---- consuming-surface behavior ------------------------------------

    #[test]
    fn pack_contract_prompt_section_targets_only_the_named_role() {
        let (_tmp, dir) =
            pack_dir_with(FULL_MANIFEST, &[("prompts/scrutiny.md", "zz file text\n")]);
        let pack = Pack::load(&dir).unwrap().unwrap();
        let worker = pack.prompt_section(Role::Worker);
        assert!(worker.contains("zz-prompt-worker"), "{worker}");
        assert!(worker.contains("zz inline worker guidance"), "{worker}");
        assert!(!worker.contains("zz-prompt-scrutiny"), "{worker}");
        let scrutiny = pack.prompt_section(Role::ValidatorScrutiny);
        assert!(scrutiny.contains("zz file text"), "{scrutiny}");
        assert!(!scrutiny.contains("zz-prompt-worker"), "{scrutiny}");
        assert_eq!(pack.prompt_section(Role::ValidatorFunctional), "");
    }

    #[test]
    fn pack_contract_when_paths_scope_gates_like_the_merge_suite() {
        let (_tmp, dir) =
            pack_dir_with(FULL_MANIFEST, &[("prompts/scrutiny.md", "zz file text\n")]);
        let pack = Pack::load(&dir).unwrap().unwrap();
        let changed = vec!["crates/engine/src/lib.rs".to_string()];
        let applicable: Vec<&str> = pack
            .gates_for_paths(&changed)
            .iter()
            .map(|g| g.name.as_str())
            .collect();
        assert_eq!(applicable, vec!["zz-gate-one"], "scoped gate skipped");
        let changed = vec!["src/widget.ts".to_string()];
        let applicable: Vec<&str> = pack
            .gates_for_paths(&changed)
            .iter()
            .map(|g| g.name.as_str())
            .collect();
        assert_eq!(applicable, vec!["zz-gate-one", "zz-gate-two"]);
    }

    /// THE floor-composition guarantee: with floor gates registered FIRST
    /// and pack gates after, pipeline evaluation order is floor…floor, pack —
    /// a pack gate can never precede or displace an engine floor gate.
    #[test]
    fn pack_contract_gates_never_precede_or_displace_engine_floor_gates() {
        use crate::types::{Assertion, AssertionCheck};
        let contract = vec![Assertion {
            id: "a1".to_string(),
            statement: "s".to_string(),
            check: AssertionCheck::Command,
            command: Some("cargo test --workspace zz_pack_contract_floor 2>&1 | grep -qE 'test result: ok\\. [1-9]'".to_string()),
        }];
        let tree = tempfile::tempdir().unwrap();
        let mut pipeline = GatePipeline::new();
        // The orchestrator's composition: floor FIRST, pack after.
        crate::contract_gates::register_contract_gates(&mut pipeline, &contract, None, tree.path());
        let floor_len = pipeline.len();
        assert!(floor_len > 0, "floor gates registered");
        pipeline.register(Box::new(PackGate::from_run(
            "zz-pack-gate",
            "cd .",
            true,
            String::new(),
        )));
        let reports = pipeline.evaluate();
        let names: Vec<&str> = reports.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names.last(),
            Some(&"zz-pack-gate"),
            "the pack gate evaluates LAST: {names:?}"
        );
        let floor_names = &names[..floor_len];
        assert!(floor_names.contains(&crate::contract_gates::VACUOUS_FILTER));
        assert!(floor_names.contains(&crate::contract_gates::ENV_SENSITIVE));
        assert!(
            !floor_names.contains(&"zz-pack-gate"),
            "no pack gate inside the floor section"
        );
        assert_eq!(
            reports.len(),
            floor_len + 1,
            "the floor is intact — added to, never displaced"
        );
    }

    #[test]
    fn pack_contract_pack_gate_carries_the_run_outcome() {
        use crate::gate::Gate;
        let pass = PackGate::from_run("g", "cd .", true, String::new());
        assert_eq!(pass.kind(), GateKind::Deterministic);
        assert!(pass.evaluate().passed());
        assert_eq!(pass.evaluate().artefact.reference, "cd .");
        let fail = PackGate::from_run("g", "cd .", false, "boom".to_string());
        assert!(!fail.evaluate().passed());
        assert_eq!(fail.evaluate().artefact.detail.as_deref(), Some("boom"));
        assert_eq!(fail.evaluate().score, None, "boolean-only gate");
    }

    #[test]
    fn pack_contract_load_for_config_resolves_repo_relative_and_refuses_non_packs() {
        let repo = tempfile::tempdir().unwrap();
        // No key ⇒ None (the byte-identical pack-less path).
        let cfg = MissionConfig::default();
        assert_eq!(load_for_config(&cfg, repo.path()).unwrap(), None);

        // Relative resolution against the repo root.
        let (_tmp, pack_src) = pack_dir_with("[pack]\nname = \"x\"\nschema = 3\n", &[]);
        let rel = repo.path().join("my-pack");
        std::fs::create_dir_all(&rel).unwrap();
        std::fs::copy(pack_src.join(PACK_MANIFEST), rel.join(PACK_MANIFEST)).unwrap();
        let cfg = MissionConfig {
            pack_dir: Some("my-pack".to_string()),
            ..MissionConfig::default()
        };
        let pack = load_for_config(&cfg, repo.path()).unwrap().expect("a pack");
        assert_eq!(pack.name, "x");

        // A configured non-pack fails closed.
        std::fs::create_dir_all(repo.path().join("not-a-pack")).unwrap();
        let cfg = MissionConfig {
            pack_dir: Some("not-a-pack".to_string()),
            ..MissionConfig::default()
        };
        let err = load_for_config(&cfg, repo.path()).expect_err("must fail");
        assert!(err.contains("it is not a pack"), "{err}");

        let cfg = MissionConfig {
            pack_dir: Some("missing-dir".to_string()),
            ..MissionConfig::default()
        };
        let err = load_for_config(&cfg, repo.path()).expect_err("must fail");
        assert!(err.contains("is not a directory"), "{err}");
    }

    // ---- textFile no-follow containment (12th-pass review) --------------
    //
    // Symlink-creating tests are unix-only, exactly like the paths.rs guard
    // tests (`std::os::unix::fs::symlink`); Windows needs privileges to
    // create symlinks, so CI coverage there comes from the no-symlink case.

    /// A textFile that is a SYMLINK to a file outside the pack would load an
    /// engine-readable secret as prompt text and ship it to a remote model —
    /// refused at load, naming the field.
    #[cfg(unix)]
    #[test]
    fn pack_textfile_nofollow_refuses_a_symlinked_leaf() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let secret = tmp.path().join("engine-readable-secret.md");
        std::fs::write(&secret, "sk-live-secret-value").unwrap();
        let pack = tmp.path().join("pack");
        std::fs::create_dir_all(pack.join("prompts")).unwrap();
        std::fs::write(
            pack.join(PACK_MANIFEST),
            "[pack]\nname = \"x\"\nschema = 3\n\n[[prompt]]\nname = \"p\"\nrole = \"worker\"\ntextFile = \"prompts/scrutiny.md\"\n",
        )
        .unwrap();
        symlink(&secret, pack.join("prompts").join("scrutiny.md")).unwrap();

        let err = Pack::load(&pack).expect_err("a symlinked textFile must be refused");
        assert!(err.contains("field `textFile`"), "names the field: {err}");
        assert!(err.contains("no-follow"), "says why: {err}");
    }

    /// A symlinked PARENT directory escapes the pack just as surely as a
    /// symlinked leaf — same refusal, same named field.
    #[cfg(unix)]
    #[test]
    fn pack_textfile_nofollow_refuses_a_symlinked_parent_dir() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("scrutiny.md"), "exfiltrated prompt text").unwrap();
        let pack = tmp.path().join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join(PACK_MANIFEST),
            "[pack]\nname = \"x\"\nschema = 3\n\n[[prompt]]\nname = \"p\"\nrole = \"worker\"\ntextFile = \"prompts/scrutiny.md\"\n",
        )
        .unwrap();
        symlink(&outside, pack.join("prompts")).unwrap();

        let err = Pack::load(&pack).expect_err("a symlinked parent dir must be refused");
        assert!(err.contains("field `textFile`"), "names the field: {err}");
        assert!(err.contains("no-follow"), "says why: {err}");
    }

    /// The honest path: a plain in-pack textFile still loads (the existing
    /// `pack_contract_full_pack_loads_all_sections` pins the same behavior
    /// through the full manifest).
    #[test]
    fn pack_textfile_nofollow_plain_in_pack_file_loads() {
        let (_tmp, dir) = pack_dir_with(
            "[pack]\nname = \"x\"\nschema = 3\n\n[[prompt]]\nname = \"p\"\nrole = \"worker\"\ntextFile = \"prompts/scrutiny.md\"\n",
            &[("prompts/scrutiny.md", "zz plain in-pack text\n")],
        );
        let pack = Pack::load(&dir).expect("load").expect("a pack");
        assert_eq!(pack.prompts[0].text, "zz plain in-pack text\n");
        assert_eq!(
            pack.prompts[0].source,
            PromptSource::File("prompts/scrutiny.md".to_string())
        );
    }
}
