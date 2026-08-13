//! `kranz config` — inspect and edit the layered configuration.
//!
//! Configuration resolves from three layers, later layers winning (see
//! `kranz_engine::config`): compiled-in defaults ← `~/.kranz/config.json`
//! (the GLOBAL layer) ← `<repo>/.kranz/config.json` (the PROJECT layer).
//!
//! Four verbs:
//! - `show` — the effective merged config (or one layer with
//!   `--global`/`--project`).
//! - `set <path> <value>` — set ONE dotted key in a layer file. The dotted
//!   path is checked against the config SCHEMA first (a typo'd key would
//!   deep-merge, validate green — unknown keys are ignored on deserialize —
//!   and be a silent no-op). The edit then happens on the raw JSON tree
//!   (never a `MissionConfig` round-trip), so every other key already IN the
//!   file — including keys kranz doesn't know about — survives untouched.
//!   The candidate merge is validated BEFORE anything is written; an invalid
//!   value leaves the file byte-identical. A `--global` write is validated
//!   TWICE: standalone (defaults + candidate global — what every OTHER repo
//!   sees) and merged with this repo's project layer; either failure refuses
//!   the write, so a project override can never mask a global value that
//!   would poison other repos.
//! - `unset <path>` — remove one dotted key from a layer file, pruning parent
//!   objects the removal left empty. Removing the last key leaves an empty
//!   `{}` file (the file is never deleted). The dotted path gets the same
//!   schema check as `set` (a typo'd key should say "unknown key", not "not
//!   set"). Unset is NOT validity-gated: removal converges toward the
//!   defaults, and gating it could deadlock (an invalid VALUE in one layer
//!   would block its own removal). The schema-path gate cannot deadlock: an
//!   unknown key never affects validation in the first place.
//! - `role <role> <model> [effort]` — the MID-MISSION path: enqueue a
//!   `config-change` control command on a running mission (the CLI twin of
//!   Slack's `/kranz config`). File edits only shape FUTURE missions; `role`
//!   reshapes the one that is running, at its next spawn of that role.

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use kranz_engine::config;
use kranz_engine::control;
use kranz_engine::paths::{self, MissionPaths};
use kranz_engine::types::{ControlCommand, MissionConfig};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Subcommands under `kranz config` — the configuration surface.
#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Print the effective merged config (defaults <- global <- project).
    ///
    /// The merged layer files (and whether each exists) are listed on stderr,
    /// so stdout stays clean JSON for piping. --global / --project print just
    /// that one layer file instead ("{}" plus a stderr note when missing).
    Show {
        /// Print only ~/.kranz/config.json (the global layer)
        #[arg(long, conflicts_with = "project")]
        global: bool,

        /// Print only `<repo>/.kranz/config.json` (the project layer)
        #[arg(long)]
        project: bool,
    },

    /// Set one key in a config layer file (validated before writing).
    ///
    /// PATH is a dotted path into the config (worker.model,
    /// orchestrator.reasoningEffort, maxParallelWorkers, skipScrutiny, ...).
    /// VALUE parses as JSON first (2, true, ["x"]) and falls back to a plain
    /// string, so `kranz config set worker.model opus` works unquoted. Only
    /// that key changes — every other key in the file (known or not) is
    /// preserved. The merged result of all layers is validated first; on
    /// failure nothing is written.
    Set {
        /// Dotted path into the config (e.g. worker.model)
        path: String,

        /// The value (JSON if it parses, plain string otherwise)
        value: String,

        /// Write to `~/.kranz/config.json` instead of `<repo>/.kranz/config.json`
        #[arg(long)]
        global: bool,
    },

    /// Remove one key from a config layer file.
    ///
    /// Parent objects left empty by the removal are pruned; removing the last
    /// key leaves an empty "{}" file (the file itself is never deleted).
    Unset {
        /// Dotted path into the config (e.g. worker.model)
        path: String,

        /// Edit `~/.kranz/config.json` instead of `<repo>/.kranz/config.json`
        #[arg(long)]
        global: bool,
    },

    /// Change a role's model/effort on a RUNNING mission (mid-mission path).
    ///
    /// Enqueues a config-change control command on the target mission's
    /// control inbox — the CLI twin of Slack's `/kranz config`. The target is
    /// the global --mission id (which must be active), or, without one, the
    /// repo's single active mission; with several active this refuses and
    /// lists them. The change applies at the next spawn of that role; the
    /// layer files are not touched.
    Role {
        /// orchestrator | worker | scrutiny | functional
        role: String,

        /// Model alias or full id passed to `claude --model` (e.g. opus)
        model: String,

        /// low | medium | high | xhigh | max (unchanged when omitted)
        effort: Option<String>,
    },
}

/// Role names accepted by `kranz config role` — the same friendly spellings
/// (and the same case-insensitive matching) Slack's `/kranz config` accepts.
const ROLES: [&str; 4] = ["orchestrator", "worker", "scrutiny", "functional"];

/// Reasoning-effort values accepted by `claude --effort` (mirrors the engine).
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Dispatch a `kranz config …` subcommand. `mission` is the global --mission
/// flag, consumed only by `role`.
pub fn cmd_config(repo: &Path, command: ConfigCommand, mission: Option<&str>) -> Result<i32> {
    let layers = Layers::resolve(repo);
    match command {
        ConfigCommand::Show { global, project } => {
            let single = if global {
                Some(layers.target(true)?.to_path_buf())
            } else if project {
                Some(layers.project.clone())
            } else {
                None
            };
            match single {
                Some(path) => {
                    let (json, exists) = render_layer_file(&path)?;
                    if !exists {
                        eprintln!("# {} does not exist", path.display());
                    }
                    print!("{json}");
                }
                None => {
                    for path in layers.merge_order() {
                        let status = if path.is_file() { "merged" } else { "absent" };
                        eprintln!("# layer {status}: {}", path.display());
                    }
                    print!("{}", render_effective(&layers.merge_order())?);
                }
            }
            Ok(0)
        }
        ConfigCommand::Set {
            path,
            value,
            global,
        } => {
            let file = set_key(&layers, global, &path, &value)?;
            println!("set {path} in {}", file.display());
            Ok(0)
        }
        ConfigCommand::Unset { path, global } => {
            let file = unset_key(&layers, global, &path)?;
            println!("removed {path} from {}", file.display());
            Ok(0)
        }
        ConfigCommand::Role {
            role,
            model,
            effort,
        } => {
            let role = parse_role(&role)?;
            let effort = effort.as_deref().map(parse_effort).transpose()?;
            let applied_to = role_change(repo, mission, role, &model, effort)?;
            let effort_note = effort.map(|e| format!(", effort {e}")).unwrap_or_default();
            println!(
                "config change queued for mission {applied_to}: {role} -> model {model}\
                 {effort_note} (applies at the next {role} spawn; the layer files are unchanged)"
            );
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------------
// Layer files (the file-choice is explicit-path so tests never touch $HOME)
// ---------------------------------------------------------------------------

/// The two editable config layer files, resolved once so every verb — and
/// every test — operates on explicit paths instead of re-deriving `$HOME`.
pub struct Layers {
    /// `~/.kranz/config.json`; `None` when no home directory resolves.
    pub global: Option<PathBuf>,
    /// `<repo>/.kranz/config.json`.
    pub project: PathBuf,
}

impl Layers {
    /// The real layer paths for `repo` (global from the home directory).
    pub fn resolve(repo: &Path) -> Self {
        Layers {
            global: paths::global_config(),
            project: paths::project_config(repo),
        }
    }

    /// Layer paths in merge order: global first, project last (later wins) —
    /// the same order `config::load` uses.
    pub fn merge_order(&self) -> Vec<PathBuf> {
        let mut order = Vec::new();
        if let Some(global) = &self.global {
            order.push(global.clone());
        }
        order.push(self.project.clone());
        order
    }

    /// The file a `--global` / default (project) edit targets.
    pub fn target(&self, global: bool) -> Result<&Path> {
        if global {
            self.global.as_deref().ok_or_else(|| {
                anyhow!("cannot resolve the home directory for ~/.kranz/config.json")
            })
        } else {
            Ok(&self.project)
        }
    }
}

/// Pretty-print the effective config merged from explicit layer paths over
/// the compiled-in defaults (missing files are skipped, like `config::load`).
pub fn render_effective(layer_paths: &[PathBuf]) -> Result<String> {
    let cfg = config::load_layers(layer_paths)?;
    Ok(format!("{}\n", serde_json::to_string_pretty(&cfg)?))
}

/// Pretty-print one layer file. A missing file renders as `{}` with
/// `exists = false` so the caller can add a note without polluting stdout.
pub fn render_layer_file(path: &Path) -> Result<(String, bool)> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let v: Value = serde_json::from_str(&text)
                .with_context(|| format!("invalid JSON in {}", path.display()))?;
            Ok((format!("{}\n", serde_json::to_string_pretty(&v)?), true))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(("{}\n".to_string(), false)),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

// ---------------------------------------------------------------------------
// set / unset (raw JSON tree edits; never a MissionConfig round-trip)
// ---------------------------------------------------------------------------

/// Set `dotted` to `raw` (JSON-parsed, string fallback) in the chosen layer
/// file. The dotted path must exist in the config schema (a typo'd key would
/// otherwise merge, validate green, and be a silent no-op). The candidate
/// state is validated BEFORE writing — for `--global` both standalone and
/// merged with this repo's project layer; on any failure the file is left
/// byte-identical. Returns the file written.
pub fn set_key(layers: &Layers, global: bool, dotted: &str, raw: &str) -> Result<PathBuf> {
    check_schema_path(dotted)?;
    let target = layers.target(global)?.to_path_buf();
    let mut tree = read_layer(&target)?;
    set_dotted(&mut tree, dotted, parse_value(raw))?;
    validate_candidate(layers, &target, &tree)
        .with_context(|| format!("refusing to write {}", target.display()))?;
    write_layer(&target, &tree)?;
    Ok(target)
}

/// Remove `dotted` from the chosen layer file, pruning parents the removal
/// left empty. The dotted path gets the same schema check as `set` — a typo'd
/// key errors as "unknown key", not "not set". A schema-valid key that is not
/// set is an error too (and nothing is written). Removing the last key leaves
/// `{}` — the file is never deleted. Returns the file written.
pub fn unset_key(layers: &Layers, global: bool, dotted: &str) -> Result<PathBuf> {
    check_schema_path(dotted)?;
    let target = layers.target(global)?.to_path_buf();
    let mut tree = read_layer(&target)?;
    if !remove_dotted(&mut tree, dotted) {
        bail!("{dotted} is not set in {}", target.display());
    }
    write_layer(&target, &tree)?;
    Ok(target)
}

/// Parse a CLI value: JSON first (`2`, `true`, `["x"]`, `"quoted"`), falling
/// back to a plain string so `set worker.model opus` works unquoted.
fn parse_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Read a layer file as a top-level JSON object. Missing file = empty object
/// (creating a key in a not-yet-existing layer is fine); invalid JSON or a
/// non-object top level is an error naming the file.
fn read_layer(path: &Path) -> Result<serde_json::Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let v: Value = serde_json::from_str(&text)
                .with_context(|| format!("invalid JSON in {}", path.display()))?;
            match v {
                Value::Object(map) => Ok(map),
                _ => bail!(
                    "{} must contain a JSON object at the top level",
                    path.display()
                ),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Pretty-print the tree back to the layer file (creating parent dirs).
///
/// ATOMIC: written to a sibling tmp file (same directory, so the rename never
/// crosses a filesystem), flushed + synced, then renamed over the target —
/// the same pattern as `reducer::write_snapshot` / `control::enqueue`. A
/// concurrent `config::load` (serve create, the `kranz work` dispatcher, the
/// Slack bridge) therefore only ever sees the old bytes or the new bytes,
/// never a torn/empty file, and a crash mid-write cannot lose the config.
/// An existing target's permissions are carried over to the replacement.
fn write_layer(path: &Path, tree: &serde_json::Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("config path {} has no file name", path.display()))?;
    let tmp = path.with_file_name(format!("{}.tmp", file_name.to_string_lossy()));
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(tree.clone()))?
    );
    {
        use std::io::Write as _;
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.sync_data()
            .with_context(|| format!("syncing {}", tmp.display()))?;
    }
    // Keep the operator's permissions (e.g. a chmod 600 config) across the
    // replacement; best-effort — the rename below is the load-bearing part.
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} over {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Set `dotted` inside the object tree, creating intermediate objects as
/// needed. A segment that exists as a non-object is an error — `set` must
/// never silently clobber a scalar with an object on the way down.
fn set_dotted(root: &mut serde_json::Map<String, Value>, dotted: &str, value: Value) -> Result<()> {
    let segments: Vec<&str> = dotted.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        bail!("invalid config path {dotted:?} (empty segment)");
    }
    let mut cur = root;
    for seg in &segments[..segments.len() - 1] {
        let slot = cur
            .entry(seg.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        cur = match slot {
            Value::Object(map) => map,
            other => bail!(
                "config path {dotted:?}: {seg:?} holds {other} (not an object); \
                 unset it first if you mean to replace it"
            ),
        };
    }
    cur.insert(segments.last().expect("non-empty split").to_string(), value);
    Ok(())
}

/// Remove `dotted` from the tree; `true` iff it was present. Parent objects
/// the removal left empty are pruned on the way back up.
fn remove_dotted(root: &mut serde_json::Map<String, Value>, dotted: &str) -> bool {
    fn recurse(map: &mut serde_json::Map<String, Value>, segs: &[&str]) -> bool {
        match segs {
            [] => false,
            [leaf] => map.remove(*leaf).is_some(),
            [head, rest @ ..] => {
                let Some(Value::Object(child)) = map.get_mut(*head) else {
                    return false;
                };
                let removed = recurse(child, rest);
                if removed && child.is_empty() {
                    map.remove(*head);
                }
                removed
            }
        }
    }
    let segs: Vec<&str> = dotted.split('.').collect();
    recurse(root, &segs)
}

/// Validate the WOULD-BE state; an error here means `set` writes nothing.
///
/// A PROJECT write is validated as this repo's full merge (defaults <- global
/// <- candidate project) — exactly the load path the engine takes here.
///
/// A GLOBAL write is validated TWICE:
/// 1. standalone (defaults <- candidate global) — what every OTHER repo,
///    without this repo's project overrides, would load. Skipping this let a
///    project override mask an invalid global value: the write "succeeded"
///    here and poisoned every repo lacking the override.
/// 2. merged with this repo's project layer, like any other write.
///
/// Either failure refuses the write, with the failing combination named.
fn validate_candidate(
    layers: &Layers,
    target: &Path,
    candidate: &serde_json::Map<String, Value>,
) -> Result<()> {
    if layers.global.as_deref() == Some(target) {
        validate_merged(&[target.to_path_buf()], target, candidate).context(
            "the global config would be invalid on its own (defaults + global): every \
             repo without this repo's project overrides would fail to load it",
        )?;
    }
    validate_merged(&layers.merge_order(), target, candidate).context(
        "the merged config for this repo (defaults + global + project) would be invalid",
    )?;
    Ok(())
}

/// Deep-merge defaults <- the given layers (the target layer's on-disk
/// content replaced by `candidate`), then deserialize and run
/// `config::validate` — exactly the load path the engine takes.
fn validate_merged(
    layer_paths: &[PathBuf],
    target: &Path,
    candidate: &serde_json::Map<String, Value>,
) -> Result<()> {
    let mut merged = serde_json::to_value(MissionConfig::default())?;
    for path in layer_paths {
        let layer = if path.as_path() == target {
            Value::Object(candidate.clone())
        } else {
            Value::Object(read_layer(path)?)
        };
        config::deep_merge(&mut merged, &layer);
    }
    let cfg: MissionConfig = serde_json::from_value(merged)
        .map_err(|e| anyhow!("merged configuration does not deserialize: {e}"))?;
    config::validate(&cfg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Schema-path validation (a typo'd key must never be a silent no-op)
// ---------------------------------------------------------------------------

/// The config schema as a JSON tree: a FULLY-POPULATED `MissionConfig`
/// serialized out. A dotted path is legal iff every segment (the final one
/// too) exists in this tree — unknown keys are ignored on deserialization,
/// so without this gate `set worker.mdoel` merges, validates green, prints
/// success, and changes nothing.
///
/// Full population is load-bearing: `default()` leaves the
/// `skip_serializing_if` options out of its serialization (`claudeBinary`,
/// `orchestrator.maxTurns`, `maxBudgetUsd`, …) and those are LEGAL keys that
/// must not be false-rejected — so every `Option` is forced to `Some` first.
fn schema_tree() -> Value {
    let mut cfg = MissionConfig {
        claude_binary: Some(String::new()),
        ..MissionConfig::default()
    };
    for role in [
        &mut cfg.orchestrator,
        &mut cfg.worker,
        &mut cfg.validator_scrutiny,
        &mut cfg.validator_functional,
    ] {
        role.max_turns = Some(0);
        role.max_budget_usd = Some(0.0);
    }
    serde_json::to_value(cfg).expect("MissionConfig always serializes")
}

/// Refuse a dotted path that does not exist in the config schema.
///
/// - An unknown segment names the unknown part and lists the valid keys at
///   that level (so `max_parallel_workers` points at `maxParallelWorkers`).
/// - Array-valued keys (`denyPatterns`, `allowValidatorCommands`) are
///   settable only as a whole: a path THROUGH one (`denyPatterns.0`) is a
///   clean error, not an attempted merge.
/// - A path through a scalar (`worker.model.x`) is a clean error too.
fn check_schema_path(dotted: &str) -> Result<()> {
    let segments: Vec<&str> = dotted.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        bail!("invalid config path {dotted:?} (empty segment)");
    }
    let schema = schema_tree();
    let mut cur = &schema;
    let mut walked: Vec<&str> = Vec::with_capacity(segments.len());
    for seg in segments {
        match cur {
            Value::Object(map) => match map.get(seg) {
                Some(next) => {
                    walked.push(seg);
                    cur = next;
                }
                None => {
                    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
                    keys.sort_unstable();
                    let level = if walked.is_empty() {
                        "the top level".to_string()
                    } else {
                        format!("`{}`", walked.join("."))
                    };
                    bail!(
                        "unknown config key `{seg}` at {level}; valid keys here: {}",
                        keys.join(", ")
                    );
                }
            },
            Value::Array(_) => bail!(
                "config path `{dotted}`: `{walked}` is an array and can only be set as a \
                 whole (e.g. `kranz config set {walked} '[\"…\"]'`); its elements are not \
                 individually addressable",
                walked = walked.join(".")
            ),
            _ => bail!(
                "config path `{dotted}`: `{}` is a plain value; nothing nests beneath it",
                walked.join(".")
            ),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// role (mid-mission config change via the control inbox)
// ---------------------------------------------------------------------------

/// Parse a role token (case-insensitively) to its canonical spelling — the
/// same names (and aliases) Slack's `/kranz config` accepts.
fn parse_role(token: &str) -> Result<&'static str> {
    ROLES
        .iter()
        .copied()
        .find(|r| r.eq_ignore_ascii_case(token))
        .ok_or_else(|| {
            anyhow!(
                "unknown role {token:?}; expected one of {}",
                ROLES.join("|")
            )
        })
}

/// Parse an effort token (case-insensitively) to its canonical spelling.
fn parse_effort(token: &str) -> Result<&'static str> {
    EFFORTS
        .iter()
        .copied()
        .find(|e| e.eq_ignore_ascii_case(token))
        .ok_or_else(|| {
            anyhow!(
                "invalid effort {token:?}; expected one of {}",
                EFFORTS.join("|")
            )
        })
}

/// Resolve the target ACTIVE mission (shared engine resolver — the same policy
/// as Slack: an explicit id must exist and be non-terminal; a bare command
/// needs exactly one active mission) and enqueue ONE `config-change` control
/// command carrying the same camelCase patch shape Slack produces
/// ([`kranz_slack::inbound::config_patch`]). A refused target enqueues
/// NOTHING. Returns the mission id the change was applied to.
pub fn role_change(
    repo: &Path,
    explicit_mission: Option<&str>,
    role: &str,
    model: &str,
    effort: Option<&str>,
) -> Result<String> {
    let mission_id = control::resolve_active_mission(repo, explicit_mission)?;
    let patch = kranz_slack::inbound::config_patch(role, model, effort)
        .ok_or_else(|| anyhow!("unknown role `{role}`"))?;
    let paths = MissionPaths::new(repo, &mission_id);
    control::enqueue(&paths, &ControlCommand::ConfigChange { patch })
        .context("enqueue config change")?;
    Ok(mission_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use clap::Parser;
    use kranz_engine::events::{Event, EventKind};
    use kranz_engine::types::MissionStatus;
    use serde_json::json;
    use tempfile::TempDir;

    // --- helpers ------------------------------------------------------------

    /// Layers rooted in a tempdir: global at `<tmp>/home-config.json`,
    /// project at `<tmp>/repo/.kranz/config.json`. Nothing touches $HOME.
    fn temp_layers(tmp: &TempDir) -> Layers {
        Layers {
            global: Some(tmp.path().join("home-config.json")),
            project: tmp.path().join("repo").join(".kranz").join("config.json"),
        }
    }

    fn write_json(path: &Path, v: &Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_string_pretty(v).unwrap()).unwrap();
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Seed a mission's events.jsonl (optionally driven to Complete) so the
    /// resolver and control inbox behave like a real mission — no backend.
    fn seed_mission(repo_root: &Path, mission_id: &str, completed: bool) {
        let paths = MissionPaths::new(repo_root, mission_id);
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let mut lines = String::new();
        let created = Event {
            seq: 1,
            ts: Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCreated {
                goal: "goal".into(),
                base_branch: "main".into(),
                mission_branch: format!("kranz/mission-{mission_id}"),
                config: MissionConfig::default(),
            },
        };
        lines.push_str(&serde_json::to_string(&created).unwrap());
        lines.push('\n');
        if completed {
            let done = Event {
                seq: 2,
                ts: Utc::now(),
                mission_id: mission_id.to_string(),
                kind: EventKind::MissionCompleted {},
            };
            lines.push_str(&serde_json::to_string(&done).unwrap());
            lines.push('\n');
        }
        std::fs::write(paths.events_file(), lines).unwrap();
    }

    fn drained(repo: &Path, mission: &str) -> Vec<ControlCommand> {
        control::drain(&MissionPaths::new(repo, mission))
            .unwrap()
            .into_iter()
            .map(|(_, cmd)| cmd)
            .collect()
    }

    // --- argument parsing ----------------------------------------------------

    #[test]
    fn parses_config_subcommands() {
        use crate::cli::{Cli, Command};

        let cli = Cli::try_parse_from(["kranz", "config", "show"]).unwrap();
        let Command::Config {
            command: ConfigCommand::Show { global, project },
        } = cli.command
        else {
            panic!("expected config show");
        };
        assert!(!global && !project);

        let cli =
            Cli::try_parse_from(["kranz", "config", "set", "worker.model", "opus", "--global"])
                .unwrap();
        let Command::Config {
            command:
                ConfigCommand::Set {
                    path,
                    value,
                    global,
                },
        } = cli.command
        else {
            panic!("expected config set");
        };
        assert_eq!(
            (path.as_str(), value.as_str(), global),
            ("worker.model", "opus", true)
        );

        let cli = Cli::try_parse_from(["kranz", "config", "unset", "worker.model"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Config { command: ConfigCommand::Unset { ref path, global: false } }
                if path == "worker.model"
        ));

        // `role` targets a mission via the GLOBAL --mission flag.
        let cli = Cli::try_parse_from([
            "kranz",
            "config",
            "role",
            "worker",
            "opus",
            "high",
            "--mission",
            "m-1",
        ])
        .unwrap();
        assert_eq!(cli.mission.as_deref(), Some("m-1"));
        let Command::Config {
            command:
                ConfigCommand::Role {
                    role,
                    model,
                    effort,
                },
        } = cli.command
        else {
            panic!("expected config role");
        };
        assert_eq!(
            (role.as_str(), model.as_str(), effort.as_deref()),
            ("worker", "opus", Some("high"))
        );

        // --global and --project are mutually exclusive on show.
        assert!(Cli::try_parse_from(["kranz", "config", "show", "--global", "--project"]).is_err());
    }

    // --- show -----------------------------------------------------------------

    #[test]
    fn show_effective_reflects_a_project_override() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(
            &layers.project,
            &json!({ "worker": { "model": "my-custom-model" } }),
        );

        let rendered = render_effective(&layers.merge_order()).unwrap();
        let effective: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(
            effective["worker"]["model"], "my-custom-model",
            "override applied"
        );
        // Untouched keys come from the compiled-in defaults.
        assert_eq!(effective["orchestrator"]["model"], "opus");
        assert_eq!(effective["worker"]["reasoningEffort"], "medium");
    }

    #[test]
    fn show_single_layer_renders_the_file_or_empty_object() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(&layers.project, &json!({ "skipScrutiny": true }));

        let (json_text, exists) = render_layer_file(&layers.project).unwrap();
        assert!(exists);
        assert_eq!(
            serde_json::from_str::<Value>(&json_text).unwrap(),
            json!({ "skipScrutiny": true })
        );

        let (json_text, exists) = render_layer_file(layers.global.as_deref().unwrap()).unwrap();
        assert!(!exists, "absent layer reported as missing");
        assert_eq!(json_text, "{}\n");
    }

    // --- set --------------------------------------------------------------------

    #[test]
    fn set_changes_only_the_dotted_key_and_preserves_unknown_keys() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(
            &layers.project,
            &json!({
                "worker": { "model": "opus", "maxTurns": 30 },
                "unknownTopLevel": { "nested": [1, 2, 3] }
            }),
        );

        let written = set_key(&layers, false, "worker.model", "sonnet").unwrap();
        assert_eq!(written, layers.project);
        assert_eq!(
            read_json(&layers.project),
            json!({
                "worker": { "model": "sonnet", "maxTurns": 30 },
                "unknownTopLevel": { "nested": [1, 2, 3] }
            }),
            "only worker.model changed; siblings and unknown keys intact"
        );
    }

    #[test]
    fn set_parses_json_values_and_falls_back_to_string() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);

        set_key(&layers, false, "worker.maxTurns", "12").unwrap();
        set_key(&layers, false, "skipScrutiny", "true").unwrap();
        set_key(&layers, false, "worker.model", "opus").unwrap();
        assert_eq!(
            read_json(&layers.project),
            json!({ "worker": { "maxTurns": 12, "model": "opus" }, "skipScrutiny": true }),
            "12 is a number, true a bool, opus a bare string"
        );
    }

    #[test]
    fn set_invalid_effort_writes_nothing_and_leaves_the_file_byte_identical() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(
            &layers.project,
            &json!({ "worker": { "model": "opus" }, "keep": 1 }),
        );
        let before = std::fs::read(&layers.project).unwrap();

        let err = set_key(&layers, false, "worker.reasoningEffort", "turbo")
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to write"), "{err}");
        assert_eq!(
            std::fs::read(&layers.project).unwrap(),
            before,
            "file byte-identical"
        );
    }

    #[test]
    fn set_non_numeric_max_parallel_workers_is_rejected_without_creating_the_file() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);

        // "abc" fails deserialization; 99 deserializes but fails validation.
        assert!(set_key(&layers, false, "maxParallelWorkers", "abc").is_err());
        assert!(set_key(&layers, false, "maxParallelWorkers", "99").is_err());
        assert!(!layers.project.exists(), "no file materialized on failure");

        // The valid twin lands.
        set_key(&layers, false, "maxParallelWorkers", "4").unwrap();
        assert_eq!(
            read_json(&layers.project),
            json!({ "maxParallelWorkers": 4 })
        );
    }

    #[test]
    fn set_global_targets_the_global_file_and_project_stays_untouched() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);

        set_key(&layers, true, "worker.model", "sonnet").unwrap();
        assert_eq!(
            read_json(layers.global.as_deref().unwrap()),
            json!({ "worker": { "model": "sonnet" } })
        );
        assert!(
            !layers.project.exists(),
            "project layer untouched by --global"
        );

        // No resolvable home directory → --global is an honest error.
        let no_home = Layers {
            global: None,
            project: layers.project.clone(),
        };
        assert!(set_key(&no_home, true, "worker.model", "opus").is_err());
    }

    #[test]
    fn set_validates_across_layers_not_just_the_edited_file() {
        // The project layer sets an invalid effort; fixing an UNRELATED key in
        // the project file still merges the bad value → refused. The same key
        // set in the GLOBAL layer is masked by the project override → allowed.
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(
            &layers.project,
            &json!({ "worker": { "reasoningEffort": "warp" } }),
        );

        assert!(set_key(&layers, false, "skipScrutiny", "true").is_err());

        write_json(
            &layers.project,
            &json!({ "worker": { "reasoningEffort": "high" } }),
        );
        // Global carries a bad effort, but the project layer wins the merge.
        write_json(
            layers.global.as_deref().unwrap(),
            &json!({ "worker": { "reasoningEffort": "warp" } }),
        );
        set_key(&layers, false, "skipScrutiny", "true").unwrap();
    }

    // --- set --global (finding C: both standalone and merged must hold) ----------

    #[test]
    fn set_global_invalid_value_masked_by_a_project_override_is_refused() {
        // This repo's project layer masks the bad value, so a merged-only
        // validation passes — and the write would poison every OTHER repo
        // that lacks the override. The global layer must also validate
        // standalone (defaults + candidate global).
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(
            &layers.project,
            &json!({ "worker": { "reasoningEffort": "high" } }),
        );

        let err = format!(
            "{:#}",
            set_key(&layers, true, "worker.reasoningEffort", "warp").unwrap_err()
        );
        assert!(err.contains("on its own"), "standalone gate named: {err}");
        assert!(
            !layers.global.as_deref().unwrap().exists(),
            "global file never materialized"
        );

        // The valid twin passes both gates and lands in the global file.
        set_key(&layers, true, "worker.reasoningEffort", "low").unwrap();
        assert_eq!(
            read_json(layers.global.as_deref().unwrap()),
            json!({ "worker": { "reasoningEffort": "low" } })
        );
    }

    #[test]
    fn set_global_names_the_merged_gate_when_this_repo_is_what_breaks() {
        // The candidate global is fine standalone, but this repo's project
        // layer is broken: the refusal must blame the merged combination.
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(
            &layers.project,
            &json!({ "worker": { "reasoningEffort": "warp" } }),
        );

        let err = format!(
            "{:#}",
            set_key(&layers, true, "skipScrutiny", "true").unwrap_err()
        );
        assert!(err.contains("this repo"), "merged gate named: {err}");
        assert!(!err.contains("on its own"), "standalone gate passed: {err}");
        assert!(!layers.global.as_deref().unwrap().exists());
    }

    // --- write_layer atomicity (finding D) ----------------------------------------

    #[cfg(unix)]
    #[test]
    fn set_replaces_the_file_via_rename_and_preserves_permissions() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(&layers.project, &json!({ "worker": { "model": "opus" } }));
        std::fs::set_permissions(&layers.project, std::fs::Permissions::from_mode(0o600)).unwrap();
        let before_ino = std::fs::metadata(&layers.project).unwrap().ino();

        set_key(&layers, false, "worker.model", "sonnet").unwrap();

        let meta = std::fs::metadata(&layers.project).unwrap();
        assert_ne!(
            meta.ino(),
            before_ino,
            "the file must be REPLACED by rename, never truncated in place — a \
             concurrent config::load (serve create, kranz work, slack bridge) must \
             never observe a torn/empty file"
        );
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "permissions carried over"
        );
        assert_eq!(
            read_json(&layers.project),
            json!({ "worker": { "model": "sonnet" } })
        );

        // No tmp litter left beside the target.
        let leftovers: Vec<String> = std::fs::read_dir(layers.project.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp litter: {leftovers:?}");
    }

    // --- schema path check (finding E: typos must never be silent no-ops) --------

    #[test]
    fn set_typo_key_is_refused_naming_the_unknown_part_and_that_levels_keys() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);

        // Nested typo: names the segment, the level, and its valid keys.
        let err = format!(
            "{:#}",
            set_key(&layers, false, "worker.mdoel", "opus").unwrap_err()
        );
        assert!(err.contains("`mdoel`"), "unknown part named: {err}");
        assert!(err.contains("`worker`"), "level named: {err}");
        assert!(
            err.contains("model") && err.contains("reasoningEffort"),
            "valid keys at that level listed: {err}"
        );

        // Top-level snake_case typo: the camelCase twin is in the listing.
        let err = format!(
            "{:#}",
            set_key(&layers, false, "max_parallel_workers", "4").unwrap_err()
        );
        assert!(err.contains("`max_parallel_workers`"), "{err}");
        assert!(err.contains("top level"), "{err}");
        assert!(
            err.contains("maxParallelWorkers"),
            "camelCase twin listed: {err}"
        );

        // Wrong case is an unknown key, not a match.
        assert!(set_key(&layers, false, "Worker.model", "opus").is_err());
        assert!(set_key(&layers, false, "worker.Model", "opus").is_err());

        assert!(
            !layers.project.exists(),
            "no typo'd set ever materialized the file"
        );
    }

    #[test]
    fn set_accepts_optional_keys_that_default_serialization_omits() {
        // claudeBinary and orchestrator.maxTurns are None in default() and so
        // absent from a default() serialization — they are legal keys and
        // must not be false-rejected by the schema gate.
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);

        set_key(&layers, false, "claudeBinary", "/usr/local/bin/claude").unwrap();
        set_key(&layers, false, "orchestrator.maxTurns", "33").unwrap();
        set_key(&layers, false, "orchestrator.maxBudgetUsd", "12.5").unwrap();
        assert_eq!(
            read_json(&layers.project),
            json!({
                "claudeBinary": "/usr/local/bin/claude",
                "orchestrator": { "maxTurns": 33, "maxBudgetUsd": 12.5 }
            })
        );
    }

    #[test]
    fn set_array_keys_work_as_a_whole_but_paths_through_them_are_refused() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);

        set_key(&layers, false, "denyPatterns", r#"["rm -rf"]"#).unwrap();
        assert_eq!(
            read_json(&layers.project),
            json!({ "denyPatterns": ["rm -rf"] })
        );

        let err = format!(
            "{:#}",
            set_key(&layers, false, "denyPatterns.0", "x").unwrap_err()
        );
        assert!(err.contains("array") && err.contains("as a whole"), "{err}");
        let err = format!(
            "{:#}",
            set_key(&layers, false, "allowValidatorCommands.2.cmd", "x").unwrap_err()
        );
        assert!(
            err.contains("array"),
            "deep paths through arrays refused too: {err}"
        );
    }

    #[test]
    fn set_paths_through_scalars_are_refused_cleanly() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);

        let err = format!(
            "{:#}",
            set_key(&layers, false, "worker.model.x", "y").unwrap_err()
        );
        assert!(
            err.contains("worker.model") && err.contains("plain value"),
            "clean error, no object-over-scalar clobber: {err}"
        );
        assert!(!layers.project.exists());
    }

    #[test]
    fn unset_typo_key_gets_the_same_schema_error_not_a_not_set_one() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(&layers.project, &json!({ "worker": { "model": "opus" } }));
        let before = std::fs::read(&layers.project).unwrap();

        let err = format!(
            "{:#}",
            unset_key(&layers, false, "worker.mdoel").unwrap_err()
        );
        assert!(
            err.contains("unknown config key"),
            "schema error, not 'not set': {err}"
        );
        assert!(err.contains("`mdoel`"), "{err}");
        assert_eq!(
            std::fs::read(&layers.project).unwrap(),
            before,
            "file untouched"
        );
    }

    // --- unset ------------------------------------------------------------------

    #[test]
    fn unset_removes_the_key_and_preserves_siblings() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(
            &layers.project,
            &json!({ "worker": { "model": "opus", "maxTurns": 9 }, "skipScrutiny": true }),
        );

        unset_key(&layers, false, "worker.model").unwrap();
        assert_eq!(
            read_json(&layers.project),
            json!({ "worker": { "maxTurns": 9 }, "skipScrutiny": true })
        );
    }

    #[test]
    fn unset_prunes_parents_left_empty_and_last_key_leaves_empty_object() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(
            &layers.project,
            &json!({ "worker": { "model": "opus" }, "skipScrutiny": true }),
        );

        unset_key(&layers, false, "worker.model").unwrap();
        assert_eq!(
            read_json(&layers.project),
            json!({ "skipScrutiny": true }),
            "empty worker object pruned"
        );

        unset_key(&layers, false, "skipScrutiny").unwrap();
        assert_eq!(
            std::fs::read_to_string(&layers.project).unwrap(),
            "{}\n",
            "last key removed leaves an empty object; the file stays"
        );
    }

    #[test]
    fn unset_missing_key_is_an_error_and_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let layers = temp_layers(&tmp);
        write_json(&layers.project, &json!({ "skipScrutiny": true }));
        let before = std::fs::read(&layers.project).unwrap();

        let err = unset_key(&layers, false, "worker.model")
            .unwrap_err()
            .to_string();
        assert!(err.contains("worker.model"), "{err}");
        assert_eq!(std::fs::read(&layers.project).unwrap(), before);
    }

    // --- role (mid-mission control command) --------------------------------------

    #[test]
    fn role_enqueues_exactly_one_config_change_with_the_slack_patch_shape() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-cfg", false);

        let applied =
            role_change(tmp.path(), Some("m-cfg"), "scrutiny", "opus", Some("high")).unwrap();
        assert_eq!(applied, "m-cfg");

        let cmds = drained(tmp.path(), "m-cfg");
        assert_eq!(cmds.len(), 1, "exactly one control command enqueued");
        match &cmds[0] {
            ControlCommand::ConfigChange { patch } => {
                assert_eq!(
                    *patch,
                    json!({ "validatorScrutiny": { "model": "opus", "reasoningEffort": "high" } }),
                    "camelCase patch shape"
                );
                // Same shape Slack produces, by construction AND by assertion.
                assert_eq!(
                    *patch,
                    kranz_slack::inbound::config_patch("scrutiny", "opus", Some("high")).unwrap()
                );
            }
            other => panic!("expected ConfigChange, got {other:?}"),
        }
    }

    #[test]
    fn role_bare_targets_the_single_active_mission_and_omits_effort() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-only", false);

        let applied = role_change(tmp.path(), None, "worker", "sonnet", None).unwrap();
        assert_eq!(applied, "m-only");
        match &drained(tmp.path(), "m-only")[0] {
            ControlCommand::ConfigChange { patch } => {
                assert_eq!(*patch, json!({ "worker": { "model": "sonnet" } }));
            }
            other => panic!("expected ConfigChange, got {other:?}"),
        }
    }

    #[test]
    fn role_refuses_an_ambiguous_target_and_enqueues_nothing() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-a", false);
        seed_mission(tmp.path(), "m-b", false);

        let err = role_change(tmp.path(), None, "worker", "opus", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("several active missions"), "{err}");
        for id in ["m-a", "m-b"] {
            assert!(
                drained(tmp.path(), id).is_empty(),
                "nothing enqueued on {id}"
            );
        }
    }

    #[test]
    fn role_refuses_a_terminal_mission_and_enqueues_nothing() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-done", true);

        let err = role_change(tmp.path(), Some("m-done"), "worker", "opus", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("active missions"),
            "honest error, not false success: {err}"
        );
        assert!(
            drained(tmp.path(), "m-done").is_empty(),
            "no control file leaked"
        );
    }

    #[test]
    fn role_unknown_role_or_effort_is_refused_before_anything_happens() {
        assert!(parse_role("manager").is_err());
        assert!(parse_effort("turbo").is_err());
        // Case-insensitive canonicalization, mirroring Slack's parser.
        assert_eq!(parse_role("WORKER").unwrap(), "worker");
        assert_eq!(parse_role("Functional").unwrap(), "functional");
        assert_eq!(parse_effort("XHIGH").unwrap(), "xhigh");
    }

    #[test]
    fn role_unknown_mission_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let err = role_change(tmp.path(), Some("m-nope"), "worker", "opus", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("m-nope"), "{err}");
    }

    // --- resolver sanity (the hoisted engine function is what role uses) ---------

    #[test]
    fn seeded_missions_fold_to_the_expected_statuses() {
        // Guards the seed helpers: if these drift, the resolver tests above
        // would silently test the wrong thing.
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-live", false);
        seed_mission(tmp.path(), "m-done", true);
        let fold = |id: &str| {
            let events = kranz_engine::event_log::EventLog::read_events(
                &MissionPaths::new(tmp.path(), id).events_file(),
            )
            .unwrap();
            kranz_engine::reducer::fold(&events).unwrap().mission.status
        };
        assert_eq!(fold("m-live"), MissionStatus::Planning);
        assert_eq!(fold("m-done"), MissionStatus::Complete);
    }
}
