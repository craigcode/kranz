//! Explicit ordinary-worker admission for the reviewed ACP image profiles.
//! No discovery, image pulling, ambient credentials or user-config inheritance.
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
}

#[derive(Clone, Copy)]
pub(crate) struct Definition {
    pub program: &'static str,
    pub args: &'static [&'static str],
    pub image: &'static str,
    pub egress: &'static [&'static str],
    pub claude: bool,
}

fn refusal(reason: &str) -> EngineError {
    EngineError::Config(format!("ACP worker profile refused: {reason}"))
}

impl AcpWorkerProfile {
    pub(crate) fn definition(&self) -> Result<Definition> {
        match self.id.as_str() {
            CLAUDE => Ok(Definition {
                program: "/usr/local/bin/node",
                args: &[
                    "/opt/acp/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js",
                ],
                image: IMAGE,
                egress: &["api.anthropic.com:443", "claude.ai:443"],
                claude: true,
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
            }),
            #[cfg(test)]
            "fixture-acp-worker-v1" => Ok(Definition {
                program: "/usr/local/bin/python3",
                args: &["-c", include_str!("acp_worker/peer.py")],
                image:
                    "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d",
                egress: &["provider.invalid:443"],
                claude: false,
            }),
            _ => Err(refusal("unknown or unqualified profile id")),
        }
    }

    pub(crate) fn validate_target(&self, os: &str, arch: &str) -> Result<()> {
        self.definition()?;
        let supported = matches!(os, "macos" | "linux") && arch == "aarch64";
        #[cfg(test)]
        let supported =
            supported || (self.id == "fixture-acp-worker-v1" && matches!(os, "macos" | "linux"));
        if !supported {
            return Err(refusal(
                "only the reviewed macOS/Linux ARM64 hosts are admitted",
            ));
        }
        if !self.credential_file.is_absolute() {
            return Err(refusal(
                "credentialFile must be an explicit absolute operator-owned file",
            ));
        }
        Ok(())
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

    pub(crate) fn prepare(&self, spec: &mut SessionSpec) -> Result<PreparedProfile> {
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
        let source = self
            .credential_file
            .canonicalize()
            .map_err(|_| refusal("credential source is unavailable"))?;
        if [spec.cwd.as_path(), sandbox.inputs.tmpdir.as_path(), primary]
            .into_iter()
            .any(|root| source.starts_with(crate::sandbox::absolutize(root)))
        {
            return Err(refusal(
                "credential source must be outside repository and worker-writable roots",
            ));
        }
        let bytes = private_credential(&self.credential_file)?;
        let value = crate::strict_json::parse(&bytes)
            .map_err(|_| refusal("credential source is not valid unique-key JSON"))?;
        let mut secrets = Vec::new();
        let token = if definition.claude {
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
        if let Some(token) = token {
            spec.env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), token);
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
        Ok(PreparedProfile {
            home: Some(home),
            secrets,
            receipt: json!({
                "profile":self.id,"credentialSource":"operator-selected-file","startupPolicy":"v1",
                "hostOs":std::env::consts::OS,"hostArch":std::env::consts::ARCH,
                "proofs":["native-tool-proof-v2.json","linux-native-tool-proof-v1.json"],
                "fullMissionAcceptanceProven":false,
            }),
        })
    }
}

fn same_egress(actual: &[String], expected: &[&str]) -> bool {
    let mut actual: Vec<_> = actual.iter().map(String::as_str).collect();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    actual == expected
}

fn private_credential(path: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| refusal("cannot open credential source"))?;
        let metadata = file.metadata()?;
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
        let _ = path;
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

pub(crate) struct PreparedProfile {
    home: Option<tempfile::TempDir>,
    secrets: Vec<String>,
    pub receipt: Value,
}

impl PreparedProfile {
    pub fn contains_secret(&self, text: &str) -> bool {
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
        self.secrets.iter().any(|s| text.contains(s))
            || serde_json::from_str::<Value>(text)
                .is_ok_and(|value| contains(&value, &self.secrets))
    }
    pub fn scrub(&self, mut text: String) -> String {
        for secret in &self.secrets {
            text = text.replace(secret, "[REDACTED]");
        }
        text
    }
    pub fn close(&mut self) -> Result<()> {
        if let Some(home) = self.home.take() {
            let path = home.path().to_path_buf();
            home.close().map_err(|_| {
                refusal(&format!(
                    "private credential home cleanup failed; inspect retained directory {}",
                    path.display()
                ))
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
