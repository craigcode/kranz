//! Explicit ordinary-worker admission for the reviewed ACP image profiles.
//! No discovery, image pulling, ambient credentials or user-config inheritance.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use crate::acp_container::{recover_dead_owner_ledgers, LedgerRecoveryReport};
use crate::backend::SessionSpec;
use crate::error::{EngineError, Result};
use crate::types::{Role, RoleConfig, SandboxEnforce, SandboxProvider, WorkerIsolation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(unix)]
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const IMAGE: &str = "sha256:5d0f56837d3b506013d47da6f3294cf90f24b4e4dbaf2079828d75522167b743";
pub const CLAUDE: &str = "claude-acp-0.77.0-arm64-v1";
pub const CODEX: &str = "codex-acp-1.11.0-arm64-v1";
pub const CLAUDE_V2: &str = "claude-acp-0.77.0-arm64-v2";
pub const CODEX_V2: &str = "codex-acp-1.11.0-arm64-v2";
pub(crate) mod admission;
// Qualified for the fixed Rust/Node workload and admission::POLICY host tuple.
pub const RESOURCE_IMAGE: &str =
    "sha256:8da749ff0a9b09d5d4baa41172992eb92ca1beb9e5ed464feb7e608c5984457c";
pub const RESOURCE_LIMITS: Resources = Resources {
    memory_mib: 2048,
    cpu_millis: 2000,
    pids: 256,
    nofile: 1024,
    fsize_mib: 128,
    tmpfs_mib: 128,
    session_seconds: 600,
};
#[cfg(test)]
pub(crate) const RESOURCE_CANDIDATE_IMAGE: &str = RESOURCE_IMAGE;
#[cfg(test)]
pub(crate) const RESOURCE_CANDIDATE_LIMITS: Resources = RESOURCE_LIMITS;
const CODEX_CONFIG: &str =
    "cli_auth_credentials_store = \"file\"\n\n[features]\nplugins = false\nremote_plugin = false\n";
#[cfg(unix)]
const CREDENTIAL_LIMIT: u64 = 48_000;

/// Operator-only, additive configuration. Persists the ID and source path,
/// never a credential value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpWorkerProfile {
    pub id: String,
    pub credential_file: PathBuf,
    /// Operator lowering or raising of the revision's declared ceilings,
    /// within its qualified range (docs/scoping/acp-resource-budgets.md D-R2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceOverrides>,
}

/// Per-field operator overrides; absent fields keep the revision default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_millis: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nofile: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsize_mib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmpfs_mib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_seconds: Option<u32>,
}

/// Effective container ceilings. Memory has no swap; `fsize` bounds any one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    pub memory_mib: u32,
    pub cpu_millis: u32,
    pub pids: u32,
    pub nofile: u32,
    pub fsize_mib: u32,
    pub tmpfs_mib: u32,
    pub session_seconds: u32,
}

/// Defaults and the qualified override range; older fixtures use runtime floors.
#[derive(Clone, Copy, Serialize)]
pub(crate) struct Ceilings {
    pub defaults: Resources,
    pub max: Resources,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<Resources>,
}

#[derive(Clone, Copy, Serialize)]
pub(crate) struct Definition {
    pub program: &'static str,
    pub args: &'static [&'static str],
    pub image: &'static str,
    pub egress: &'static [&'static str],
    pub claude: bool,
    /// `None` for revisions qualified before ceilings existed; their
    /// container argv stays exactly as reviewed.
    pub resources: Option<Ceilings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualification: Option<admission::Policy>,
}

impl Definition {
    /// Digest of the code-resident contract behind a profile id, so evidence
    /// shows which revision ran even though approval binds only the id.
    pub(crate) fn sha256(&self) -> String {
        let canonical = serde_json::to_vec(self).expect("definition serializes");
        crate::standards_waiver::sha256_hex(&canonical)
    }
}

#[cfg(test)]
const FIXTURE: Definition = Definition {
    program: "/usr/local/bin/python3",
    args: &["-c", include_str!("acp_worker/peer.py")],
    image: "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d",
    egress: &["provider.invalid:443"],
    claude: false,
    resources: None,
    qualification: None,
};

fn refusal(reason: &str) -> EngineError {
    EngineError::Config(format!("ACP worker profile refused: {reason}"))
}

/// Non-secret verification receipt for an operator-owned credential file.
/// Records the SHA-256 digest of the canonical file so out-of-band renewal
/// between sessions is auditable without persisting or logging token values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSourceReceipt {
    pub profile_id: String,
    pub credential_sha256: String,
    pub token_count: usize,
    pub writeback_enabled: bool,
}

impl AcpWorkerProfile {
    /// Validate the operator-owned credential source against the containment and
    /// renewal rules without exposing secret bytes or writing back from any
    /// worker-writable directory.
    pub fn verify_credential_source(
        &self,
        primary_repo: &Path,
        worker_cwd: &Path,
        scratch_tmpdir: &Path,
    ) -> Result<CredentialSourceReceipt> {
        self.verify_credential_source_with_extra_write(
            primary_repo,
            worker_cwd,
            scratch_tmpdir,
            &[],
        )
    }

    pub(crate) fn verify_credential_source_with_extra_write(
        &self,
        primary_repo: &Path,
        worker_cwd: &Path,
        scratch_tmpdir: &Path,
        extra_write: &[PathBuf],
    ) -> Result<CredentialSourceReceipt> {
        self.validate_target(std::env::consts::OS, std::env::consts::ARCH)?;
        let definition = self.definition()?;
        let verified = self.load_verified_credential(
            &definition,
            primary_repo,
            worker_cwd,
            scratch_tmpdir,
            extra_write,
        )?;
        Ok(CredentialSourceReceipt {
            profile_id: self.id.clone(),
            credential_sha256: crate::standards_waiver::sha256_hex(&verified.bytes),
            token_count: verified.secrets.len(),
            writeback_enabled: false,
        })
    }

    fn load_verified_credential(
        &self,
        definition: &Definition,
        primary_repo: &Path,
        worker_cwd: &Path,
        scratch_tmpdir: &Path,
        extra_write: &[PathBuf],
    ) -> Result<VerifiedCredential> {
        if credential_path_in_forbidden_roots(
            &self.credential_file,
            primary_repo,
            worker_cwd,
            scratch_tmpdir,
            extra_write,
        ) {
            return Err(refusal(
                "credential source must be outside repository and worker-writable roots",
            ));
        }
        let link_meta = std::fs::symlink_metadata(&self.credential_file)
            .map_err(|_| refusal("credential source is unavailable"))?;
        if link_meta.file_type().is_symlink() {
            return Err(refusal("cannot open credential source"));
        }
        let source = self
            .credential_file
            .canonicalize()
            .map_err(|_| refusal("credential source is unavailable"))?;
        if credential_path_in_forbidden_roots(
            &source,
            primary_repo,
            worker_cwd,
            scratch_tmpdir,
            extra_write,
        ) {
            return Err(refusal(
                "credential source must be outside repository and worker-writable roots",
            ));
        }
        let bytes = private_credential_verified(&self.credential_file, &source)?;
        let value = crate::strict_json::parse(&bytes)
            .map_err(|_| refusal("credential source is not valid unique-key JSON"))?;
        let mut secrets = Vec::new();
        let claude_value = if definition.claude {
            if value.get("credentialEnv").and_then(Value::as_str) != Some("CLAUDE_CODE_OAUTH_TOKEN")
            {
                return Err(refusal(
                    "Claude profile requires the explicit OAuth credential file",
                ));
            }
            let token = value
                .get("value")
                .and_then(Value::as_str)
                .filter(|s| s.len() >= 16 && *s == s.trim())
                .ok_or_else(|| refusal("Claude OAuth token is missing or malformed"))?;
            secrets.push(token.to_owned());
            Some(token.to_owned())
        } else {
            let tokens = value
                .get("tokens")
                .and_then(Value::as_object)
                .filter(|tokens| tokens.contains_key("access_token"))
                .ok_or_else(|| refusal("Codex OAuth access token is missing"))?;
            for key in ["access_token", "refresh_token", "id_token"] {
                if let Some(value) = tokens.get(key) {
                    let token = value
                        .as_str()
                        .filter(|s| s.len() >= 16 && *s == s.trim())
                        .ok_or_else(|| refusal("Codex OAuth token is malformed"))?;
                    secrets.push(token.to_owned());
                }
            }
            if secrets.is_empty() || value.get("OPENAI_API_KEY").is_some_and(|v| !v.is_null()) {
                return Err(refusal("Codex profile requires an existing CLI OAuth login; API-key login is not qualified"));
            }
            None
        };
        Ok(VerifiedCredential {
            bytes,
            claude_value,
            secrets,
        })
    }

    pub(crate) fn definition(&self) -> Result<Definition> {
        match self.id.as_str() {
            CLAUDE_V2 | CODEX_V2 => {
                let base = Self {
                    id: if self.id == CLAUDE_V2 { CLAUDE } else { CODEX }.into(),
                    credential_file: self.credential_file.clone(),
                    resources: None,
                }
                .definition()?;
                Ok(Definition {
                    image: RESOURCE_IMAGE,
                    resources: Some(Ceilings {
                        defaults: RESOURCE_LIMITS,
                        max: RESOURCE_LIMITS,
                        min: Some(RESOURCE_LIMITS),
                    }),
                    qualification: Some(admission::POLICY),
                    ..base
                })
            }
            #[cfg(test)]
            "qualification-claude-resource-r3" | "qualification-codex-resource-r3" => {
                let base = Self {
                    id: if self.id == "qualification-claude-resource-r3" {
                        CLAUDE
                    } else {
                        CODEX
                    }
                    .into(),
                    credential_file: self.credential_file.clone(),
                    resources: None,
                }
                .definition()?;
                Ok(Definition {
                    image: RESOURCE_CANDIDATE_IMAGE,
                    resources: Some(Ceilings {
                        defaults: RESOURCE_CANDIDATE_LIMITS,
                        max: RESOURCE_CANDIDATE_LIMITS,
                        min: None,
                    }),
                    ..base
                })
            }
            CLAUDE => Ok(Definition {
                program: "/usr/local/bin/node",
                args: &[
                    "/opt/acp/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js",
                ],
                image: IMAGE,
                egress: &["api.anthropic.com:443", "claude.ai:443"],
                claude: true,
                resources: None,
                qualification: None,
            }),
            CODEX => Ok(Definition {
                program: "/opt/acp/node_modules/.bin/codex-acp",
                args: &[],
                image: IMAGE,
                egress: &[
                    "chatgpt.com:443",
                    "auth.openai.com:443",
                    "api.openai.com:443",
                ],
                claude: false,
                resources: None,
                qualification: None,
            }),
            #[cfg(test)]
            "fixture-acp-worker-v1" => Ok(FIXTURE),
            #[cfg(test)]
            "fixture-acp-worker-v2" => Ok(Definition {
                resources: Some(Ceilings {
                    min: None,
                    defaults: Resources {
                        memory_mib: 256,
                        cpu_millis: 1_000,
                        pids: 64,
                        nofile: 256,
                        fsize_mib: 64,
                        tmpfs_mib: 64,
                        session_seconds: 3_600,
                    },
                    max: Resources {
                        memory_mib: 1_024,
                        cpu_millis: 2_000,
                        pids: 256,
                        nofile: 1_024,
                        fsize_mib: 256,
                        tmpfs_mib: 256,
                        session_seconds: 14_400,
                    },
                }),
                ..FIXTURE
            }),
            _ => Err(refusal("unknown or unqualified profile id")),
        }
    }

    pub(crate) fn validate_target(&self, os: &str, arch: &str) -> Result<()> {
        let definition = self.definition()?;
        let supported = if definition.qualification.is_some() {
            os == "macos" && arch == "aarch64"
        } else {
            matches!(os, "macos" | "linux") && arch == "aarch64"
        };
        #[cfg(test)]
        let supported = supported
            || (self.id.starts_with("fixture-acp-worker-") && matches!(os, "macos" | "linux"));
        if !supported {
            return Err(refusal(if definition.qualification.is_some() {
                "v2 requires the qualified macOS ARM64 Colima host; see docs/acp-containment.md"
            } else {
                "only the reviewed macOS/Linux ARM64 hosts are admitted"
            }));
        }
        if !self.credential_file.is_absolute() {
            return Err(refusal(
                "credentialFile must be an explicit absolute operator-owned file",
            ));
        }
        self.effective_resources()?;
        Ok(())
    }

    /// The ceilings this session runs under: revision defaults with operator
    /// overrides applied. Reject unusable values and Docker's unlimited sentinels.
    pub(crate) fn effective_resources(&self) -> Result<Option<Resources>> {
        let Some(ceilings) = self.definition()?.resources else {
            return match self.resources {
                Some(_) => Err(refusal(
                    "this profile revision declares no resource ceilings to override",
                )),
                None => Ok(None),
            };
        };
        let overrides = self.resources.clone().unwrap_or_default();
        let pick = |name: &str, value: Option<u32>, default: u32, min: u32, max: u32| match value {
            None => Ok(default),
            Some(v) if (min..=max).contains(&v) => Ok(v),
            Some(_) => Err(refusal(&format!(
                "resources.{name} must be between {min} and the revision maximum {max}"
            ))),
        };
        let (d, m) = (ceilings.defaults, ceilings.max);
        let min = ceilings.min.unwrap_or(Resources {
            memory_mib: 6,
            cpu_millis: 10,
            pids: 1,
            nofile: 1,
            fsize_mib: 1,
            tmpfs_mib: 64,
            session_seconds: 1,
        });
        Ok(Some(Resources {
            memory_mib: pick(
                "memoryMib",
                overrides.memory_mib,
                d.memory_mib,
                min.memory_mib,
                m.memory_mib,
            )?,
            cpu_millis: pick(
                "cpuMillis",
                overrides.cpu_millis,
                d.cpu_millis,
                min.cpu_millis,
                m.cpu_millis,
            )?,
            pids: pick("pids", overrides.pids, d.pids, min.pids, m.pids)?,
            nofile: pick("nofile", overrides.nofile, d.nofile, min.nofile, m.nofile)?,
            fsize_mib: pick(
                "fsizeMib",
                overrides.fsize_mib,
                d.fsize_mib,
                min.fsize_mib,
                m.fsize_mib,
            )?,
            // Docker's managed /dev tmpfs is 64 MiB. The configurable /dev/shm
            // and explicit writable tmpfs mounts may not exceed this ceiling.
            tmpfs_mib: pick(
                "tmpfsMib",
                overrides.tmpfs_mib,
                d.tmpfs_mib,
                min.tmpfs_mib,
                m.tmpfs_mib,
            )?,
            session_seconds: pick(
                "sessionSeconds",
                overrides.session_seconds,
                d.session_seconds,
                min.session_seconds,
                m.session_seconds,
            )?,
        }))
    }

    pub(crate) fn validate_config(
        &self,
        role: Role,
        cfg: &RoleConfig,
        isolation: WorkerIsolation,
    ) -> Result<()> {
        self.validate_target(std::env::consts::OS, std::env::consts::ARCH)?;
        let definition = self.definition()?;
        if role != Role::Worker
            || cfg.backend.as_deref() != Some("acp")
            || isolation != WorkerIsolation::Worktree
        {
            return Err(refusal(
                "requires the ACP worker role and worktree isolation",
            ));
        }
        if cfg.acp_command.is_some() || !cfg.acp_args.is_empty() {
            return Err(refusal(
                "acpCommand/acpArgs must be omitted; the profile owns the guest argv",
            ));
        }
        if cfg.sandbox.provider != SandboxProvider::Container
            || cfg.sandbox.enforce != SandboxEnforce::FsNet
            || cfg.sandbox.image.as_deref() != Some(definition.image)
            || !cfg.sandbox.extra_write.is_empty()
            || !same_egress(&cfg.sandbox.egress, definition.egress)
        {
            return Err(refusal("requires the exact pinned container image, fs+net, profile egress and no extraWrite"));
        }
        Ok(())
    }

    pub(crate) async fn prepare(&self, spec: &mut SessionSpec) -> Result<PreparedProfile> {
        self.validate_target(std::env::consts::OS, std::env::consts::ARCH)?;
        let definition = self.definition()?;
        let sandbox = spec
            .sandbox
            .as_ref()
            .ok_or_else(|| refusal("missing resolved container boundary"))?;
        let container = sandbox
            .container
            .as_ref()
            .ok_or_else(|| refusal("missing Docker image/runtime"))?;
        if !same_egress(&sandbox.inputs.egress, definition.egress) {
            return Err(refusal(
                "resolved egress differs from the qualified profile; mission egress grants cannot widen its fixed allowlist",
            ));
        }
        if sandbox.backend != crate::sandbox::SandboxBackend::Container
            || container.runtime != crate::sandbox_container::ContainerRuntime::Docker
            || container.image != definition.image
            || sandbox.inputs.enforce != SandboxEnforce::FsNet
            || !sandbox.inputs.extra_write.is_empty()
            || crate::sandbox::absolutize(&spec.cwd)
                != crate::sandbox::absolutize(&sandbox.inputs.session_cwd)
            || !spec.writable
            || spec.resume.is_some()
        {
            return Err(refusal(
                "resolved worker boundary differs from the qualified profile",
            ));
        }
        if !std::fs::symlink_metadata(spec.cwd.join(".git")).is_ok_and(|m| m.is_file()) {
            return Err(refusal("worker cwd must be an isolated Git worktree"));
        }
        crate::sandbox::validate_git_config_protection(&sandbox.inputs, true)?;
        // The ordinary runner supplies attribution/base/proxy variables only.
        // Embedding callers cannot inject loader, credential or startup settings.
        if spec.env.keys().any(|key| {
            !matches!(
                key.as_str(),
                "KRANZ_BASE_SHA"
                    | "GIT_AUTHOR_NAME"
                    | "GIT_AUTHOR_EMAIL"
                    | "GIT_COMMITTER_NAME"
                    | "GIT_COMMITTER_EMAIL"
                    | "HTTP_PROXY"
                    | "HTTPS_PROXY"
                    | "NO_PROXY"
            )
        }) {
            return Err(refusal("unexpected caller environment variable"));
        }
        let primary = sandbox
            .inputs
            .mission_dir
            .ancestors()
            .find(|p| p.file_name().is_some_and(|n| n == ".kranz"))
            .and_then(Path::parent);
        let primary = primary.ok_or_else(|| refusal("missing mission repository boundary"))?;
        if crate::sandbox::absolutize(primary) == crate::sandbox::absolutize(&spec.cwd) {
            return Err(refusal("worker cwd must differ from the primary checkout"));
        }
        // Admission precedes even opening the provider credential source.
        let admitted_host = match definition.qualification {
            Some(policy) => {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                {
                    admission::validate_ambient_docker_env()?;
                    if let Some(host) = session_admitted_host(&spec.session_id, &container.image) {
                        Some(host)
                    } else {
                        Some(admission::probe(&sandbox.inputs, policy).await?)
                    }
                }
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                {
                    Some(admission::probe(&sandbox.inputs, policy).await?)
                }
            }
            None => None,
        };
        let VerifiedCredential {
            bytes,
            claude_value,
            secrets,
        } = self.load_verified_credential(
            &definition,
            primary,
            &spec.cwd,
            &sandbox.inputs.tmpdir,
            &sandbox.inputs.extra_write,
        )?;
        let scratch_base = crate::sandbox::absolutize(&crate::backend_claude::scratch_root_base());
        if [primary, spec.cwd.as_path()]
            .iter()
            .any(|root| scratch_base.starts_with(crate::sandbox::absolutize(root)))
        {
            return Err(refusal(
                "private scratch base must be outside the primary and worker checkouts",
            ));
        }
        let home = tempfile::Builder::new()
            .prefix("kranz-acp-profile-")
            .tempdir_in(scratch_base)?;
        // Only the inner directory is mounted writable. The engine-owned 0700
        // parent stays outside the guest mounts, so chmod inside HOME cannot
        // expose its credentials to other host users through a shared /tmp.
        let home_path = home.path().canonicalize()?.join("home");
        std::fs::create_dir(&home_path)?;
        if home_path.starts_with(crate::sandbox::absolutize(&spec.cwd)) {
            return Err(refusal(
                "private credential home must be outside the worktree",
            ));
        }
        if let Some(claude_value) = claude_value {
            spec.env
                .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), claude_value);
            spec.env.insert(
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                "1".into(),
            );
        } else {
            let codex = home_path.join(".codex");
            std::fs::create_dir(&codex)?;
            private_write(&codex.join("auth.json"), &bytes)?;
            private_write(&codex.join("config.toml"), CODEX_CONFIG.as_bytes())?;
            spec.env
                .insert("CODEX_HOME".into(), codex.display().to_string());
            spec.env.insert("NO_BROWSER".into(), "1".into());
            spec.env
                .insert("INITIAL_AGENT_MODE".into(), "read-only".into());
        }
        spec.sandbox
            .as_mut()
            .expect("checked sandbox")
            .inputs
            .tmpdir = home_path;
        let resources = self.effective_resources()?;
        let mut receipt = json!({
            "profile":self.id,"definitionSha256":definition.sha256(),"resources":resources,
            "credentialSource":"operator-selected-file","startupPolicy":"v1",
            "hostOs":std::env::consts::OS,"hostArch":std::env::consts::ARCH,
            "proofs":["native-tool-proof-v2.json","linux-native-tool-proof-v1.json"],
            "fullMissionAcceptanceProven":false,
        });
        if let Some(host) = &admitted_host {
            receipt["hostAdmission"] = host.receipt.clone();
            receipt["proofs"] = serde_json::to_value(definition.qualification.unwrap().evidence)
                .expect("qualification serializes");
        }
        Ok(PreparedProfile {
            home: Some(home),
            secrets,
            stream_tails: Default::default(),
            resources,
            receipt,
            admitted_host,
        })
    }
}

struct VerifiedCredential {
    bytes: Vec<u8>,
    claude_value: Option<String>,
    secrets: Vec<String>,
}

fn credential_path_in_forbidden_roots(
    candidate: &Path,
    primary_repo: &Path,
    worker_cwd: &Path,
    scratch_tmpdir: &Path,
    extra_write: &[PathBuf],
) -> bool {
    let abs_candidate = crate::sandbox::absolutize(candidate);
    [worker_cwd, scratch_tmpdir, primary_repo]
        .into_iter()
        .chain(extra_write.iter().map(PathBuf::as_path))
        .any(|root| {
            let abs_root = crate::sandbox::absolutize(root);
            abs_candidate.starts_with(&abs_root)
                || candidate.starts_with(&abs_root)
                || root.canonicalize().ok().is_some_and(|canon| {
                    abs_candidate.starts_with(&canon) || candidate.starts_with(&canon)
                })
        })
}

fn same_egress(actual: &[String], expected: &[&str]) -> bool {
    let mut actual: Vec<_> = actual.iter().map(String::as_str).collect();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    actual == expected
}

#[cfg(all(unix, test))]
fn private_credential(path: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        let link_meta = std::fs::symlink_metadata(path)
            .map_err(|_| refusal("cannot open credential source"))?;
        if link_meta.file_type().is_symlink() {
            return Err(refusal("cannot open credential source"));
        }
        let source = path
            .canonicalize()
            .map_err(|_| refusal("cannot open credential source"))?;
        private_credential_verified(path, &source)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(refusal(
            "credential file channel is unqualified on this platform",
        ))
    }
}

fn private_credential_verified(original_path: &Path, source: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let orig_meta = std::fs::symlink_metadata(original_path)
            .map_err(|_| refusal("cannot open credential source"))?;
        let source_meta = std::fs::symlink_metadata(source)
            .map_err(|_| refusal("cannot open credential source"))?;
        if orig_meta.file_type().is_symlink()
            || source_meta.file_type().is_symlink()
            || (orig_meta.dev(), orig_meta.ino()) != (source_meta.dev(), source_meta.ino())
        {
            return Err(refusal("cannot open credential source"));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(source)
            .map_err(|_| refusal("cannot open credential source"))?;
        let metadata = file.metadata()?;
        let post_orig_meta = std::fs::symlink_metadata(original_path)
            .map_err(|_| refusal("cannot open credential source"))?;
        if post_orig_meta.file_type().is_symlink()
            || (metadata.dev(), metadata.ino()) != (source_meta.dev(), source_meta.ino())
            || (metadata.dev(), metadata.ino()) != (post_orig_meta.dev(), post_orig_meta.ino())
            || original_path.canonicalize().ok().as_deref() != Some(source)
        {
            return Err(refusal("cannot open credential source"));
        }
        // SAFETY: geteuid has no preconditions and returns this process's uid.
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || metadata.len() > CREDENTIAL_LIMIT
        {
            return Err(refusal(
                "credential source must be bounded, owner-only, single-link regular data",
            ));
        }
        let mut bytes = Vec::new();
        file.take(CREDENTIAL_LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > CREDENTIAL_LIMIT {
            return Err(refusal("credential source grew beyond its limit"));
        }
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (original_path, source);
        Err(refusal(
            "credential file channel is unqualified on this platform",
        ))
    }
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

fn purge_profile_home(root: &Path) -> std::io::Result<()> {
    // Remove the credential-bearing adapter directories FIRST, then the rest
    // of the home. `home/` is the guest's mount and may be rewritten by a
    // container that is still alive (Drop, or a failed removal), so nothing
    // under it is addressed by path: `remove_owned_tree_nofollow` walks by
    // directory descriptor, repairs a guest `chmod 0000`, and unlinks a
    // swapped-in symlink instead of following it (2026-11-10 audit ACP-1).
    // `root` itself is outside every guest mount.
    let inner = root.join("home");
    for credentials in [".claude", ".codex"] {
        let _ = crate::sandbox_container::remove_owned_tree_nofollow(&inner.join(credentials));
    }
    crate::sandbox_container::remove_owned_tree_nofollow(root)
}

pub(crate) struct PreparedProfile {
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    pub admitted_host: Option<admission::AdmittedHost>,
    home: Option<tempfile::TempDir>,
    secrets: Vec<String>,
    stream_tails: std::collections::HashMap<String, String>,
    // Read only by the macOS/Linux container create path.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    pub resources: Option<Resources>,
    pub receipt: Value,
}

impl PreparedProfile {
    pub fn contains_secret(&mut self, text: &str) -> bool {
        fn contains(value: &Value, secrets: &[String]) -> bool {
            match value {
                Value::String(text) => secrets.iter().any(|s| text.contains(s)),
                Value::Array(values) => values.iter().any(|v| contains(v, secrets)),
                Value::Object(values) => values.iter().any(|(key, v)| {
                    secrets.iter().any(|s| key.contains(s)) || contains(v, secrets)
                }),
                _ => false,
            }
        }
        if self.secrets.iter().any(|s| text.contains(s)) {
            return true;
        }
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return false;
        };
        if contains(&value, &self.secrets) {
            return true;
        }
        // NDJSON frames are not text boundaries. Keep a bounded suffix for
        // each ACP text stream, across intervening protocol notifications.
        let update = &value["params"]["update"];
        let Some(kind @ ("agent_message_chunk" | "agent_thought_chunk")) =
            update["sessionUpdate"].as_str()
        else {
            return false;
        };
        let Some(chunk) = update["content"]["text"].as_str() else {
            return false;
        };
        let tail = self.stream_tails.entry(kind.into()).or_default();
        tail.push_str(chunk);
        if self.secrets.iter().any(|secret| tail.contains(secret)) {
            return true;
        }
        let retain = self
            .secrets
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(0)
            .saturating_sub(1);
        let mut start = tail.len().saturating_sub(retain);
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        tail.drain(..start);
        false
    }

    pub fn scrub(&self, mut text: String) -> String {
        for secret in &self.secrets {
            text = text.replace(secret, "[REDACTED]");
        }
        text
    }

    pub fn close(&mut self) -> Result<()> {
        if let Some(home) = self.home.take() {
            let path = home.keep();
            purge_profile_home(&path).map_err(|_| {
                refusal(&format!(
                    "private credential home cleanup failed; inspect retained directory {}",
                    path.display()
                ))
            })?;
        }
        Ok(())
    }
}

impl Drop for PreparedProfile {
    fn drop(&mut self) {
        if let Some(home) = self.home.take() {
            let path = home.keep();
            if let Err(error) = purge_profile_home(&path) {
                // Drop cannot fail the run; make a surviving credential home
                // visible instead of silent.
                tracing::error!(%error, "ACP private home was not fully removed on drop");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pre-admission & single-mount-proof session coordination (KRZ-H2)
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct PendingResolutionAdmission {
    cwd_key: PathBuf,
    mission_key: PathBuf,
    extra_keys: Vec<PathBuf>,
    image: String,
    admitted_host: Option<admission::AdmittedHost>,
    proven_roots: Vec<PathBuf>,
    created_at: std::time::Instant,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct ActiveSessionAdmission {
    image: String,
    admitted_host: Option<admission::AdmittedHost>,
    proven_roots: Vec<PathBuf>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
static PENDING_RESOLUTIONS: std::sync::Mutex<Vec<PendingResolutionAdmission>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(any(target_os = "macos", target_os = "linux"))]
static ACTIVE_SESSIONS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, ActiveSessionAdmission>>,
> = std::sync::OnceLock::new();

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn active_sessions(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, ActiveSessionAdmission>> {
    ACTIVE_SESSIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
const PENDING_RESOLUTION_TTL: std::time::Duration = std::time::Duration::from_secs(60);

pub(crate) struct SessionAdmissionGuard {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    session_id: Option<String>,
}

impl Drop for SessionAdmissionGuard {
    fn drop(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(session_id) = self.session_id.take() {
            if let Ok(mut map) = active_sessions().lock() {
                map.remove(&session_id);
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_admission_probe_sync(
    inputs: &crate::sandbox::SandboxInputs,
    policy: admission::Policy,
) -> Result<admission::AdmittedHost> {
    let inputs = inputs.clone();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| EngineError::Backend(format!("admission runtime failed: {e}")))?;
                runtime.block_on(admission::probe(&inputs, policy))
            })
            .join()
            .unwrap_or_else(|_| {
                Err(EngineError::Backend(
                    "admission probe thread panicked".into(),
                ))
            })
    })
}

/// Runs `admission::probe` BEFORE any Docker mount-proof container when `role_sandbox`
/// selects the `-v2` `RESOURCE_IMAGE`, and records the proven canonical roots so
/// `OwnedContainer::prepare_with_host` does not prove `declared_mount_roots` twice.
pub(crate) fn admit_or_prove_session_mounts(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
    runtime: crate::sandbox_container::ContainerRuntime,
    roots: &[PathBuf],
) -> crate::sandbox_container::MountProof {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let image = role_sandbox
            .image
            .as_deref()
            .unwrap_or(crate::sandbox_container::DEFAULT_IMAGE);
        let extra_write: Vec<PathBuf> = role_sandbox
            .extra_write
            .iter()
            .map(|raw| crate::sandbox::expand_tilde(raw))
            .collect();
        let (admitted_host, proof) = if image == RESOURCE_IMAGE {
            if runtime != crate::sandbox_container::ContainerRuntime::Docker {
                return crate::sandbox_container::MountProof::Failed(
                    "ACP v2 requires the Docker container runtime".into(),
                );
            }
            let inputs = crate::sandbox::SandboxInputs {
                enforce: role_sandbox.enforce,
                session_cwd: session_cwd.to_path_buf(),
                mission_dir: mission_dir.to_path_buf(),
                tmpdir: mission_dir.join("runs").join("contract-home"),
                extra_write: extra_write.clone(),
                egress: role_sandbox.egress.clone(),
                validator_read_deny_roots: Vec::new(),
            };
            let admitted = match run_admission_probe_sync(&inputs, admission::POLICY) {
                Ok(host) => host,
                Err(err) => return crate::sandbox_container::MountProof::Failed(err.to_string()),
            };
            let proof = crate::sandbox_container::prove_mount_roots_with_client(
                &admitted.client,
                roots,
                image,
            );
            (Some(admitted), proof)
        } else {
            (
                None,
                crate::sandbox_container::prove_mount_roots(runtime, roots, image),
            )
        };
        if matches!(proof, crate::sandbox_container::MountProof::Proven) {
            let mut proven_roots = Vec::new();
            for root in roots {
                if !root.as_os_str().is_empty() {
                    let key = crate::sandbox_container::canonical_mount_root_key(root);
                    if !proven_roots.contains(&key) {
                        proven_roots.push(key);
                    }
                }
            }
            let entry = PendingResolutionAdmission {
                cwd_key: crate::sandbox_container::canonical_mount_root_key(session_cwd),
                mission_key: crate::sandbox_container::canonical_mount_root_key(mission_dir),
                extra_keys: extra_write
                    .iter()
                    .map(|p| crate::sandbox_container::canonical_mount_root_key(p))
                    .collect(),
                image: image.to_string(),
                admitted_host,
                proven_roots,
                created_at: std::time::Instant::now(),
            };
            if let Ok(mut pending) = PENDING_RESOLUTIONS.lock() {
                pending.retain(|item| item.created_at.elapsed() < PENDING_RESOLUTION_TTL);
                if pending.len() >= 32 {
                    pending.remove(0);
                }
                pending.push(entry);
            }
        }
        proof
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (session_cwd, mission_dir);
        crate::sandbox_container::prove_mount_roots(
            runtime,
            roots,
            role_sandbox
                .image
                .as_deref()
                .unwrap_or(crate::sandbox_container::DEFAULT_IMAGE),
        )
    }
}

/// Ensures ACP `-v2` (`RESOURCE_IMAGE`) host admission runs BEFORE
/// `ContainerEgressBoundary::start` or backend session launch, and binds any
/// pre-proven roots from `sandbox::resolve` to `spec.session_id`.
pub(crate) async fn prepare_session_admission(spec: &SessionSpec) -> Result<SessionAdmissionGuard> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let Some(sandbox) = spec.sandbox.as_ref() else {
            return Ok(SessionAdmissionGuard { session_id: None });
        };
        if sandbox.backend != crate::sandbox::SandboxBackend::Container
            || sandbox.inputs.enforce == SandboxEnforce::Off
        {
            return Ok(SessionAdmissionGuard { session_id: None });
        }
        let Some(container) = sandbox.container.as_ref() else {
            return Ok(SessionAdmissionGuard { session_id: None });
        };
        let cwd_key =
            crate::sandbox_container::canonical_mount_root_key(&sandbox.inputs.session_cwd);
        let mission_key =
            crate::sandbox_container::canonical_mount_root_key(&sandbox.inputs.mission_dir);
        let extra_keys: Vec<PathBuf> = sandbox
            .inputs
            .extra_write
            .iter()
            .map(|p| crate::sandbox_container::canonical_mount_root_key(p))
            .collect();
        let consumed = PENDING_RESOLUTIONS.lock().ok().and_then(|mut pending| {
            pending.retain(|item| item.created_at.elapsed() < PENDING_RESOLUTION_TTL);
            let idx = pending.iter().rposition(|item| {
                item.cwd_key == cwd_key
                    && item.mission_key == mission_key
                    && item.extra_keys == extra_keys
                    && item.image == container.image
            })?;
            Some(pending.remove(idx))
        });

        let (admitted_host, proven_roots) = match consumed {
            Some(entry) => (entry.admitted_host, entry.proven_roots),
            None => (None, Vec::new()),
        };

        let admitted_host = if container.image == RESOURCE_IMAGE {
            admission::validate_ambient_docker_env()?;
            match admitted_host {
                Some(host) => Some(host),
                None => Some(admission::probe(&sandbox.inputs, admission::POLICY).await?),
            }
        } else {
            admitted_host
        };

        if admitted_host.is_some() || !proven_roots.is_empty() {
            if let Ok(mut map) = active_sessions().lock() {
                map.insert(
                    spec.session_id.clone(),
                    ActiveSessionAdmission {
                        image: container.image.clone(),
                        admitted_host,
                        proven_roots,
                    },
                );
            }
            return Ok(SessionAdmissionGuard {
                session_id: Some(spec.session_id.clone()),
            });
        }
        Ok(SessionAdmissionGuard { session_id: None })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = spec;
        Ok(SessionAdmissionGuard {})
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn session_admitted_host(session_id: &str, image: &str) -> Option<admission::AdmittedHost> {
    let map = active_sessions().lock().ok()?;
    let entry = map.get(session_id)?;
    (entry.image == image)
        .then(|| entry.admitted_host.clone())
        .flatten()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn take_session_proven_roots(
    session_id: &str,
    image: &str,
    requires_admitted_host: bool,
) -> Vec<PathBuf> {
    let Ok(mut map) = active_sessions().lock() else {
        return Vec::new();
    };
    let Some(entry) = map.get_mut(session_id) else {
        return Vec::new();
    };
    if entry.image != image || (requires_admitted_host && entry.admitted_host.is_none()) {
        return Vec::new();
    }
    std::mem::take(&mut entry.proven_roots)
}

/// Returns an `env_clear`d, trusted-binary `DockerEvaluator` for
/// `ContainerEgressBoundary`, running `admission::probe` first and pinning to
/// the Colima `unix://` socket whenever the session uses ACP `-v2` (`RESOURCE_IMAGE`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) async fn docker_client_for_egress(
    spec: &SessionSpec,
) -> Result<crate::gate_evaluation::subprocess::DockerEvaluator> {
    let sandbox = spec.sandbox.as_ref().ok_or_else(|| {
        EngineError::Backend("container egress requested without a sandbox".to_string())
    })?;
    let container = sandbox.container.as_ref().ok_or_else(|| {
        EngineError::Backend("container sandbox is missing runtime/image".to_string())
    })?;
    if container.image == RESOURCE_IMAGE {
        admission::validate_ambient_docker_env()?;
        if let Some(host) = session_admitted_host(&spec.session_id, &container.image) {
            return Ok(host.client);
        }
        let host = admission::probe(&sandbox.inputs, admission::POLICY).await?;
        if let Ok(mut map) = active_sessions().lock() {
            let entry =
                map.entry(spec.session_id.clone())
                    .or_insert_with(|| ActiveSessionAdmission {
                        image: container.image.clone(),
                        admitted_host: None,
                        proven_roots: Vec::new(),
                    });
            entry.admitted_host = Some(host.clone());
        }
        return Ok(host.client);
    }
    let docker = crate::acp_container::trusted_control_program(&sandbox.inputs, "docker")?;
    crate::gate_evaluation::subprocess::DockerEvaluator::new(&docker).map_err(EngineError::Backend)
}

#[cfg(test)]
mod tests;
