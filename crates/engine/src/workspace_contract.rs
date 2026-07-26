//! Tracked workspace contract (`.kranz/workspace.json`) — schema + validation.
//!
//! Design `docs/scoping/workspace-contract.md` D-A: a base-branch-owned,
//! schema-versioned, additive contract describing the runnable workspace a
//! mission expects — bootstrap, services, readiness, optional data hooks,
//! previews, secret *names*, disk hints, and mounts. Keeping it on the base
//! branch (like `.kranz/merge-gates.json`) prevents a mission branch from
//! weakening the contract that judges it.
//!
//! Ownership of behavior:
//! - **Missing contract ⇒ `Ok(None)`** — today's worktree-only behavior,
//!   never an error.
//! - **Present-but-invalid ⇒ fail closed** at draft/approve with the
//!   violation named and the owner identified as repo setup.
//!
//! This module is schema + validation only. Provisioning, bootstrap, and the
//! provider seam are later tickets (`workspace-bootstrap-preflight`,
//! `workspace-provider-seam`); nothing here starts services or injects
//! secrets, and secret *values* never appear in the contract, the event log,
//! or any error message.
//!
//! Mount/cache-dir convention (acceptance note 2): `mounts[]` names paths
//! the provider must make writable OUTSIDE the worktree — package caches,
//! DB dirs — so container bootstraps do not die on the tier-3 `--read-only`
//! boundary. v1 entries are plain strings mapping to `extra_write` grants on
//! sandboxed runs, so they must be absolute paths without parent components.

use crate::error::{EngineError, Result};
use crate::git_ops::GitRepo;
use serde::Deserialize;
use std::path::Path;

pub const WORKSPACE_CONTRACT_PATH: &str = ".kranz/workspace.json";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceContract {
    /// Schema version; only `1` is understood. Missing or any other value
    /// fails closed — a newer contract must not be silently half-read.
    #[serde(default)]
    pub schema_version: u32,
    /// Ordered setup commands, cwd relative to the workspace root.
    #[serde(default)]
    pub bootstrap: Vec<String>,
    #[serde(default)]
    pub services: Vec<ServiceSpec>,
    /// Checks that must pass before the first worker turn.
    #[serde(default)]
    pub readiness: Vec<String>,
    #[serde(default)]
    pub data: Option<DataHooks>,
    #[serde(default)]
    pub previews: Vec<PreviewSpec>,
    /// Names the provider must inject — never values.
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub disk: Option<DiskHints>,
    /// Paths the provider must make writable outside the worktree (see the
    /// module docs' mount/cache-dir convention).
    #[serde(default)]
    pub mounts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceSpec {
    pub name: String,
    pub start: String,
    #[serde(default)]
    pub health_check: Option<String>,
    pub port: ServicePort,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServicePort {
    pub policy: PortPolicy,
}

/// `dynamic` — the provider allocates a free port; `{"fixed": N}` — the
/// service must bind exactly N, colliding with any other fixed N in the
/// same contract (refused at validation).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortPolicy {
    Dynamic,
    Fixed(u16),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewSpec {
    pub name: String,
    pub url_template: String,
}

/// Optional golden-data hooks (commands). Free-form strings; secret *names*
/// only inside them, never values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataHooks {
    #[serde(default)]
    pub clone: Option<String>,
    #[serde(default)]
    pub migrate: Option<String>,
    #[serde(default)]
    pub reset: Option<String>,
    #[serde(default)]
    pub skew_check: Option<String>,
}

/// Optional provider cleanup hints.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiskHints {
    #[serde(default)]
    pub prune: Option<String>,
    #[serde(default)]
    pub retain: Option<String>,
}

/// Load and validate the contract from the repo ROOT (never the mission
/// branch — base-branch-owned, mirroring merge-gates ownership). Missing
/// file ⇒ `Ok(None)`; present-but-invalid ⇒ an [`EngineError`] naming the
/// workspace contract, the specific violation, and the repo-setup owner.
pub fn load_workspace_contract(repo_root: &Path) -> Result<Option<WorkspaceContract>> {
    let path = repo_root.join(WORKSPACE_CONTRACT_PATH);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(EngineError::Io(e)),
    };
    parse_workspace_contract(&bytes)
        .map(Some)
        .map_err(|violation| {
            EngineError::Config(format!(
                "workspace contract {WORKSPACE_CONTRACT_PATH} is invalid (owner: repo-setup): {violation}"
            ))
        })
}

/// Load the contract as COMMITTED on `ref_name` (the run-time read — design
/// D-C/D-A): the workspace bootstrap/readiness gate reads the LIVE BASE
/// BRANCH, mirroring merge.rs's `live_base_sha` idiom, so the contract
/// holds in BOTH isolation modes (a mission branch cannot weaken the
/// contract that gates its own spend — checkout mode's working tree IS the
/// mission branch mid-run), and an operator's committed contract fix on the
/// base branch is picked up on resume. Missing ⇒ `Ok(None)`;
/// present-but-invalid ⇒ the same fail-closed [`EngineError`] shape as
/// [`load_workspace_contract`].
pub fn load_workspace_contract_at_ref(
    repo: &GitRepo,
    ref_name: &str,
) -> Result<Option<WorkspaceContract>> {
    match repo.show_file(ref_name, WORKSPACE_CONTRACT_PATH)? {
        None => Ok(None),
        Some(bytes) => parse_workspace_contract(&bytes)
            .map(Some)
            .map_err(|violation| {
                EngineError::Config(format!(
                    "workspace contract {WORKSPACE_CONTRACT_PATH} at {ref_name} is invalid (owner: repo-setup): {violation}"
                ))
            }),
    }
}

/// Parse and validate contract bytes. Every validation failure names the
/// specific rule it broke; parse failures carry the serde location.
pub fn parse_workspace_contract(bytes: &[u8]) -> std::result::Result<WorkspaceContract, String> {
    let contract: WorkspaceContract = serde_json::from_slice(bytes)
        .map_err(|e| format!("invalid JSON in {WORKSPACE_CONTRACT_PATH}: {e}"))?;
    validate_workspace_contract(&contract)?;
    Ok(contract)
}

fn validate_workspace_contract(contract: &WorkspaceContract) -> std::result::Result<(), String> {
    if contract.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported schemaVersion {} (expected {SCHEMA_VERSION})",
            contract.schema_version
        ));
    }

    for (i, command) in contract.bootstrap.iter().enumerate() {
        if command.trim().is_empty() {
            return Err(format!("bootstrap[{i}] has an empty command string"));
        }
    }
    for (i, command) in contract.readiness.iter().enumerate() {
        if command.trim().is_empty() {
            return Err(format!("readiness[{i}] has an empty command string"));
        }
    }

    for (i, name) in contract.secrets.iter().enumerate() {
        if !is_secret_name(name) {
            return Err(format!(
                "secrets[{i}] {name:?} is not a secret NAME (expected ^[A-Z][A-Z0-9_]*$); \
                 secret values never belong in the tracked contract"
            ));
        }
    }

    for (i, mount) in contract.mounts.iter().enumerate() {
        // Mount paths describe the TARGET runtime (a Linux container or the
        // host), not the validation host: a POSIX-absolute path is valid even
        // when kranz itself runs on Windows, where Path::is_absolute would
        // reject it for lacking a drive letter. Accept both forms, and check
        // '..' across both separators.
        if !is_contract_absolute(mount) || has_parent_components(mount) {
            return Err(format!(
                "mounts[{i}] {mount:?} must be an absolute path without '..' components"
            ));
        }
    }

    for (i, service) in contract.services.iter().enumerate() {
        if contract.services[..i]
            .iter()
            .any(|s| s.name == service.name)
        {
            return Err(format!("duplicate service name {:?}", service.name));
        }
        if let PortPolicy::Fixed(fixed) = &service.port.policy {
            if let Some(other) = contract.services[..i]
                .iter()
                .find(|s| matches!(&s.port.policy, PortPolicy::Fixed(f) if f == fixed))
            {
                return Err(format!(
                    "fixed port {fixed} collides between services {:?} and {:?}",
                    other.name, service.name
                ));
            }
        }
    }

    for (i, preview) in contract.previews.iter().enumerate() {
        if contract.previews[..i]
            .iter()
            .any(|p| p.name == preview.name)
        {
            return Err(format!("duplicate preview name {:?}", preview.name));
        }
    }

    Ok(())
}

/// A secret NAME (env-var shape), not a value: `^[A-Z][A-Z0-9_]*$`.
fn is_secret_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// True when a mount path is absolute in EITHER the POSIX form (`/var/…`)
/// or the Windows form (`C:\…`, `C:/…`, or a UNC `\\host\…`). Contract paths
/// describe the target runtime, not the validation host, so both forms are
/// valid on every platform.
fn is_contract_absolute(mount: &str) -> bool {
    if mount.starts_with('/') {
        return true;
    }
    if mount.starts_with("\\\\") {
        return true;
    }
    let bytes = mount.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// True when any path component is `..` (either separator).
fn has_parent_components(mount: &str) -> bool {
    mount.split(['/', '\\']).any(|component| component == "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_contract_json() -> &'static [u8] {
        br#"{
            "schemaVersion": 1,
            "bootstrap": ["cargo fetch", "npm ci"],
            "services": [
                {
                    "name": "api",
                    "start": "cargo run -p api",
                    "healthCheck": "curl -sf localhost:8080/health",
                    "port": { "policy": "dynamic" }
                },
                {
                    "name": "db",
                    "start": "docker compose up db",
                    "port": { "policy": { "fixed": 5432 } }
                }
            ],
            "readiness": ["curl -sf localhost:8080/health", "pg_isready"],
            "data": {
                "clone": "pg_dump golden | psql workspace",
                "migrate": "sqlx migrate run",
                "reset": "dropdb workspace && createdb workspace",
                "skewCheck": "sqlx migrate info --check"
            },
            "previews": [
                { "name": "app", "urlTemplate": "http://localhost:{port}/" }
            ],
            "secrets": ["DATABASE_URL", "STRIPE_API_KEY"],
            "disk": { "prune": "docker system prune -f", "retain": "7d" },
            "mounts": ["/var/cache/cargo", "/var/lib/postgres"]
        }"#
    }

    #[test]
    fn workspace_contract_full_schema_round_trip() {
        let contract = parse_workspace_contract(full_contract_json()).expect("valid contract");
        assert_eq!(contract.schema_version, 1);
        assert_eq!(contract.bootstrap, ["cargo fetch", "npm ci"]);
        assert_eq!(contract.services.len(), 2);
        assert_eq!(contract.services[0].name, "api");
        assert_eq!(
            contract.services[0].health_check.as_deref(),
            Some("curl -sf localhost:8080/health")
        );
        assert_eq!(contract.services[0].port.policy, PortPolicy::Dynamic);
        assert_eq!(contract.services[1].port.policy, PortPolicy::Fixed(5432));
        assert_eq!(contract.readiness.len(), 2);
        let data = contract.data.expect("data hooks");
        assert_eq!(data.migrate.as_deref(), Some("sqlx migrate run"));
        assert_eq!(
            data.skew_check.as_deref(),
            Some("sqlx migrate info --check")
        );
        assert_eq!(contract.previews.len(), 1);
        assert_eq!(
            contract.previews[0].url_template,
            "http://localhost:{port}/"
        );
        assert_eq!(contract.secrets, ["DATABASE_URL", "STRIPE_API_KEY"]);
        let disk = contract.disk.expect("disk hints");
        assert_eq!(disk.prune.as_deref(), Some("docker system prune -f"));
        assert_eq!(contract.mounts.len(), 2);
    }

    #[test]
    fn workspace_contract_minimal_is_valid() {
        let contract = parse_workspace_contract(br#"{"schemaVersion": 1}"#).expect("minimal");
        assert!(contract.bootstrap.is_empty());
        assert!(contract.services.is_empty());
        assert!(contract.mounts.is_empty());
        assert!(contract.data.is_none());
    }

    #[test]
    fn workspace_contract_missing_file_is_none_not_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let loaded = load_workspace_contract(dir.path()).expect("load must not error");
        assert!(loaded.is_none());
    }

    #[test]
    fn workspace_contract_loads_from_repo_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let kranz = dir.path().join(".kranz");
        std::fs::create_dir_all(&kranz).unwrap();
        std::fs::write(kranz.join("workspace.json"), full_contract_json()).unwrap();
        let loaded = load_workspace_contract(dir.path())
            .expect("load")
            .expect("present");
        assert_eq!(loaded.services.len(), 2);
    }

    #[test]
    fn workspace_contract_invalid_json_refused() {
        let err = parse_workspace_contract(b"{ not json").unwrap_err();
        assert!(err.contains("invalid JSON"), "{err}");
    }

    #[test]
    fn workspace_contract_wrong_schema_version_refused() {
        let err = parse_workspace_contract(br#"{"schemaVersion": 2}"#).unwrap_err();
        assert!(err.contains("unsupported schemaVersion 2"), "{err}");
        // Missing version defaults to 0 and fails closed too.
        let err = parse_workspace_contract(br#"{"bootstrap": ["true"]}"#).unwrap_err();
        assert!(err.contains("unsupported schemaVersion 0"), "{err}");
    }

    #[test]
    fn workspace_contract_duplicate_service_names_refused() {
        let err = parse_workspace_contract(
            br#"{"schemaVersion": 1, "services": [
                {"name": "api", "start": "a", "port": {"policy": "dynamic"}},
                {"name": "api", "start": "b", "port": {"policy": "dynamic"}}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.contains("duplicate service name \"api\""), "{err}");
    }

    #[test]
    fn workspace_contract_duplicate_preview_names_refused() {
        let err = parse_workspace_contract(
            br#"{"schemaVersion": 1, "previews": [
                {"name": "app", "urlTemplate": "http://a/"},
                {"name": "app", "urlTemplate": "http://b/"}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.contains("duplicate preview name \"app\""), "{err}");
    }

    #[test]
    fn workspace_contract_fixed_port_collision_refused() {
        let err = parse_workspace_contract(
            br#"{"schemaVersion": 1, "services": [
                {"name": "db", "start": "a", "port": {"policy": {"fixed": 5432}}},
                {"name": "db-replica", "start": "b", "port": {"policy": {"fixed": 5432}}}
            ]}"#,
        )
        .unwrap_err();
        assert!(
            err.contains("fixed port 5432 collides between services \"db\" and \"db-replica\""),
            "{err}"
        );
        // Same port on a dynamic sibling is fine (dynamic never collides).
        parse_workspace_contract(
            br#"{"schemaVersion": 1, "services": [
                {"name": "db", "start": "a", "port": {"policy": {"fixed": 5432}}},
                {"name": "api", "start": "b", "port": {"policy": "dynamic"}}
            ]}"#,
        )
        .expect("dynamic + fixed mix is valid");
    }

    #[test]
    fn workspace_contract_empty_bootstrap_command_refused() {
        let err = parse_workspace_contract(
            br#"{"schemaVersion": 1, "bootstrap": ["cargo fetch", "  "]}"#,
        )
        .unwrap_err();
        assert!(
            err.contains("bootstrap[1] has an empty command string"),
            "{err}"
        );
    }

    #[test]
    fn workspace_contract_empty_readiness_command_refused() {
        let err =
            parse_workspace_contract(br#"{"schemaVersion": 1, "readiness": [""]}"#).unwrap_err();
        assert!(
            err.contains("readiness[0] has an empty command string"),
            "{err}"
        );
    }

    #[test]
    fn workspace_contract_secret_value_shaped_entries_refused() {
        for bad in [
            "database_url",
            "DATABASE-URL",
            "sk-live-abc123",
            "9LIVES",
            "",
        ] {
            let json = format!(r#"{{"schemaVersion": 1, "secrets": ["DATABASE_URL", "{bad}"]}}"#);
            let err = parse_workspace_contract(json.as_bytes()).unwrap_err();
            assert!(
                err.contains("secrets[1]") && err.contains("not a secret NAME"),
                "entry {bad:?}: {err}"
            );
        }
        // data hook commands stay free-form — no NAME-shape policing there.
        parse_workspace_contract(
            br#"{"schemaVersion": 1, "data": {"clone": "pg_dump $golden | psql -h db workspace"}}"#,
        )
        .expect("data hooks are free-form commands");
    }

    #[test]
    fn workspace_contract_mount_must_be_absolute_without_parent_components() {
        for (bad, why) in [
            ("relative/cache", "non-absolute"),
            ("~/.cargo", "tilde is not absolute to Path"),
            ("/var/cache/../outside", "parent component"),
            ("../outside", "relative parent component"),
        ] {
            let json = format!(r#"{{"schemaVersion": 1, "mounts": ["{bad}"]}}"#);
            let err = parse_workspace_contract(json.as_bytes()).unwrap_err();
            assert!(
                err.contains("mounts[0]") && err.contains("absolute path without '..'"),
                "{why}: {err}"
            );
        }
        parse_workspace_contract(br#"{"schemaVersion": 1, "mounts": ["/var/cache/cargo"]}"#)
            .expect("absolute mount without '..' is valid");
        // Contract paths describe the target runtime, so BOTH absolute forms
        // validate on every host platform (Path::is_absolute is host-biased).
        for good in ["/var/cache/cargo", "C:\\cache\\cargo", "C:/cache/cargo"] {
            let json = serde_json::json!({ "schemaVersion": 1, "mounts": [good] }).to_string();
            parse_workspace_contract(json.as_bytes())
                .unwrap_or_else(|e| panic!("{good:?} must validate on every platform: {e}"));
        }
    }

    #[test]
    fn workspace_contract_load_error_names_owner_and_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let kranz = dir.path().join(".kranz");
        std::fs::create_dir_all(&kranz).unwrap();
        std::fs::write(kranz.join("workspace.json"), br#"{"schemaVersion": 9}"#).unwrap();
        let err = load_workspace_contract(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("workspace contract"), "{msg}");
        assert!(msg.contains(WORKSPACE_CONTRACT_PATH), "{msg}");
        assert!(msg.contains("repo-setup"), "{msg}");
        assert!(msg.contains("unsupported schemaVersion 9"), "{msg}");
    }

    /// The run-time read comes from the COMMITTED ref (the live base
    /// branch), not the working tree: an uncommitted working-tree edit is
    /// invisible, and another branch's copy is never read (D-A).
    #[test]
    fn workspace_contract_at_ref_reads_the_committed_ref_not_the_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .expect("spawn git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping test: git is not on PATH");
            return;
        }
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "test"]);
        git(&["config", "user.email", "test@example.com"]);
        let kranz = root.join(".kranz");
        std::fs::create_dir_all(&kranz).unwrap();
        std::fs::write(
            kranz.join("workspace.json"),
            br#"{"schemaVersion": 1, "readiness": ["pg_isready"]}"#,
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "contract"]);

        let repo = GitRepo::open(root).expect("open repo");
        let loaded = load_workspace_contract_at_ref(&repo, "main")
            .expect("load")
            .expect("present on main");
        assert_eq!(loaded.readiness, ["pg_isready"]);

        // Uncommitted working-tree edits do not leak into the ref read.
        std::fs::write(kranz.join("workspace.json"), br#"{"schemaVersion": 1}"#).unwrap();
        let loaded = load_workspace_contract_at_ref(&repo, "main")
            .expect("load")
            .expect("still the committed contract");
        assert_eq!(loaded.readiness, ["pg_isready"]);

        // A ref without the file is `None` (missing, never an error). The
        // orphan checkout keeps the index, so clear it before committing —
        // otherwise the "empty" branch would still carry the contract.
        git(&["checkout", "--orphan", "empty"]);
        git(&["rm", "-rf", "."]);
        git(&["commit", "--allow-empty", "-m", "empty"]);
        assert!(load_workspace_contract_at_ref(&repo, "empty")
            .expect("load")
            .is_none());
    }

    /// Present-but-invalid at the ref fails closed, naming the owner.
    #[test]
    fn workspace_contract_at_ref_invalid_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .expect("spawn git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping test: git is not on PATH");
            return;
        }
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "test"]);
        git(&["config", "user.email", "test@example.com"]);
        let kranz = root.join(".kranz");
        std::fs::create_dir_all(&kranz).unwrap();
        std::fs::write(kranz.join("workspace.json"), br#"{"schemaVersion": 9}"#).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "broken contract"]);

        let repo = GitRepo::open(root).expect("open repo");
        let err = load_workspace_contract_at_ref(&repo, "main").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("workspace contract"), "{msg}");
        assert!(msg.contains("repo-setup"), "{msg}");
        assert!(msg.contains("unsupported schemaVersion 9"), "{msg}");
    }
}
