//! Opt-in proof fixture, not production permission policy or mission admission.
use anyhow::{bail, Context, Result};
use kranz_engine::backend::AgentEvent;
use kranz_engine::git_ops::GitRepo;
use kranz_engine::live_permission::{Delivery, Proposal};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const COMMAND: &str = "echo kranz-acp-tool-fixture-v1 > fixture-result.txt";
const OUTPUT: &[u8] = b"kranz-acp-tool-fixture-v1\n";
const FILE: &str = "fixture-result.txt";
pub const REPORT: &str = r#"{"result":"partial","summary":"kranz-acp-tool-fixture-v1","filesTouched":["fixture-result.txt"],"testsAdded":[],"dependenciesAdded":[],"knownGaps":["Tool compatibility fixture only; no mission completion claimed."],"commits":[],"commandsRun":["echo kranz-acp-tool-fixture-v1 > fixture-result.txt"],"escalation":null,"questions":[]}"#;

pub fn prompt() -> String {
    format!("Tool compatibility fixture only. Use your native shell tool exactly once to run exactly this command in the session working directory: {COMMAND}\nRequest explicit one-time approval before executing it (Codex: require_escalated with no prefix rule). Do not run a preliminary command, read files, use any other tool, change mode, add permissions, call the network, retry, or create a commit. If approval is unavailable, stop. After successful execution return exactly this JSON, without Markdown fences:\n{REPORT}")
}

// Read only bounded, single-link regular files. O_NONBLOCK prevents a planted
// FIFO from blocking the host; O_NOFOLLOW prevents symlink traversal.
fn bytes(path: &Path) -> Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    if !std::fs::symlink_metadata(path)?.is_file() {
        bail!("fixture file is not regular: {}", path.display());
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            bail!("fixture file has multiple links");
        }
    }
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        bail!("fixture file is not bounded regular data");
    }
    let mut out = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut out)?;
    if out.len() > 64 * 1024 {
        bail!("fixture file exceeded its limit");
    }
    Ok(out)
}

fn names(path: &Path) -> Result<Vec<String>> {
    if !std::fs::symlink_metadata(path)?.is_dir() {
        bail!("fixture directory was replaced");
    }
    let mut names = std::fs::read_dir(path)?
        .take(16)
        .map(|e| {
            e?.file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF8 fixture path"))
        })
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

pub struct Workspace {
    primary: PathBuf,
    workspace: PathBuf,
    base: String,
    frozen: BTreeMap<PathBuf, Vec<u8>>,
}

impl Workspace {
    pub async fn prepare(root: &Path) -> Result<Self> {
        let primary = root.join("primary");
        let workspace = root.join("workspace");
        // The only custom Git spawn happens before any agent starts, with a
        // fixed argv and no hooks/templates/global config or inherited secrets.
        let mut child = tokio::process::Command::new("git")
            .args(["init", "--template=", "--initial-branch=main"])
            .arg(&primary)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
            Ok(result) => result?,
            Err(error) => {
                child.kill().await?;
                return Err(error.into());
            }
        };
        if !status.success() {
            bail!("fixture git init failed");
        }
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(primary.join(".git/config"))?
            .write_all(b"\n[user]\n name = ACP Fixture\n email = fixture@example.invalid\n[commit]\n gpgsign = false\n")?;
        std::fs::write(primary.join("README"), b"ACP tool fixture base\n")?;
        let repo = GitRepo::open(&primary)?;
        let base = repo.commit_paths(&[Path::new("README")], "fixture base")?;
        repo.add_worktree(&workspace, "fixture", &base)?;
        std::fs::create_dir(workspace.join(".kranz"))?;
        let mut frozen = BTreeMap::new();
        for path in [
            "README",
            ".git/config",
            ".git/HEAD",
            ".git/index",
            ".git/refs/heads/main",
        ] {
            let path = primary.join(path);
            frozen.insert(path.clone(), bytes(&path)?);
        }
        for path in ["README", ".git"] {
            let path = workspace.join(path);
            frozen.insert(path.clone(), bytes(&path)?);
        }
        let fixture = Self {
            primary,
            workspace,
            base,
            frozen,
        };
        fixture.verify(false)?;
        Ok(fixture)
    }

    pub fn verify(&self, delivered: bool) -> Result<()> {
        if names(&self.primary)? != [".git", "README"]
            || !std::fs::symlink_metadata(self.primary.join(".git"))?.is_dir()
        {
            bail!("primary checkout changed");
        }
        for (path, expected) in &self.frozen {
            if bytes(path)? != *expected {
                bail!("fixture baseline changed: {}", path.display());
            }
        }
        let expected = if delivered {
            vec![".git", ".kranz", "README", FILE]
        } else {
            vec![".git", ".kranz", "README"]
        };
        if names(&self.workspace)? != expected || !names(&self.workspace.join(".kranz"))?.is_empty()
        {
            bail!("fixture workspace has unexpected paths");
        }
        if delivered && bytes(&self.workspace.join(FILE))? != OUTPUT {
            bail!("fixture output differs");
        }
        Ok(())
    }

    pub fn commit(&self) -> Result<Value> {
        // Called only after the contained session has confirmed its shutdown.
        self.verify(true)?;
        let primary = GitRepo::open(&self.primary)?;
        let worker = GitRepo::open(&self.workspace)?;
        if primary.head_sha()? != self.base
            || worker.head_sha()? != self.base
            || worker.current_branch()? != "fixture"
        {
            bail!("worker changed Git history");
        }
        let commit = worker.commit_paths(&[Path::new(FILE)], "bounded ACP tool fixture")?;
        if worker.commits_between(&self.base, &commit)?.len() != 1
            || worker.changed_paths(&self.base, &commit)? != [FILE]
        {
            bail!("fixture did not deliver exactly one expected feature commit");
        }
        self.verify(true)?;
        if primary.head_sha()? != self.base || !primary.is_clean()? || !worker.is_clean()? {
            bail!("fixture checkout is not clean after host checkpoint");
        }
        Ok(
            json!({"event":"probe.tool.deliverable","base":self.base,"commit":commit,"featureCommits":1,"files":[FILE],"primaryUnchanged":true,"commitActor":"probe-host","missionCompletionProven":false}),
        )
    }
}

#[derive(Default)]
pub struct Evidence {
    tool_id: Option<String>,
    saw_tool: bool,
    request_id: Option<String>,
    sent: bool,
    completed: bool,
}

impl Evidence {
    fn bind_tool(&mut self, value: &Value) -> Result<()> {
        let id = value["toolCallId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("tool call has no id")?;
        if self.tool_id.as_deref().is_some_and(|old| old != id) {
            bail!("more than one tool invocation");
        }
        self.tool_id = Some(id.to_owned());
        Ok(())
    }

    fn input(value: &Value, workspace: &Path, wrapped: bool) -> Result<()> {
        let input = value.as_object().context("missing exact raw tool input")?;
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .context("missing exact shell command")?;
        // Codex's pinned adapter leaves /usr/bin/bash in permission requests
        // even when the announcement contains the bare command. Accept only
        // that observed wrapper at consent; legacy notification encodings
        // remain notification-only. Never parse or strip arbitrary shell syntax.
        let matches = command == COMMAND
            || command == format!("/usr/bin/bash -lc '{COMMAND}'")
            || (wrapped
                && ["/bin/bash", "/bin/sh", "/bin/zsh"].iter().any(|shell| {
                    ["-c", "-lc"]
                        .iter()
                        .any(|flag| command == format!("{shell} {flag} '{COMMAND}'"))
                }));
        if !matches {
            bail!("command differs from the authorized fixture");
        }
        for (key, value) in input {
            let valid = match key.as_str() {
                "command" => true,
                "cwd" => value.as_str() == workspace.to_str(),
                "description" => value.as_str().is_some_and(|v| v.len() <= 1024),
                "timeout" => value.as_u64().is_some_and(|v| v > 0 && v <= 10_000),
                "run_in_background" | "dangerouslyDisableSandbox" => value == false,
                _ => false,
            };
            if !valid {
                bail!("unsupported tool input field: {key}");
            }
        }
        Ok(())
    }

    pub fn authorize(&mut self, proposal: &Proposal, workspace: &Path) -> Result<String> {
        self.authorize_at(proposal, workspace, chrono::Utc::now())
    }

    pub fn authorize_at(
        &mut self,
        proposal: &Proposal,
        workspace: &Path,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<String> {
        proposal.validate()?;
        if self.request_id.is_some()
            || self.completed
            || proposal.prohibition.is_some()
            || at < proposal.observed_at
            || at >= proposal.deadline
        {
            bail!("permission is repeated, prohibited, stale or after completion");
        }
        if proposal.action["kind"] != "execute" {
            bail!("fixture authorizes only execute");
        }
        self.bind_tool(&proposal.action)?;
        Self::input(&proposal.action["rawInput"], workspace, false)?;
        let selected = proposal
            .option(true)
            .context("no unique allow_once option")?;
        let option = proposal
            .options
            .iter()
            .find(|o| o["optionId"] == selected)
            .unwrap();
        if option
            .as_object()
            .context("invalid permission option")?
            .keys()
            .any(|key| !matches!(key.as_str(), "optionId" | "name" | "kind"))
        {
            bail!("permission option carries unsupported extensions");
        }
        self.request_id = Some(proposal.id.clone());
        Ok(selected)
    }

    pub fn observe(&mut self, event: &AgentEvent, workspace: &Path) -> Result<()> {
        match event {
            AgentEvent::PermissionResponded {
                request_id,
                delivery,
                ..
            } => {
                if self.request_id.as_ref() != Some(request_id)
                    || self.sent
                    || *delivery != Delivery::Sent
                {
                    bail!("permission delivery is missing, repeated or uncertain");
                }
                self.sent = true;
            }
            AgentEvent::ToolUse { raw, .. }
            | AgentEvent::ToolResult { raw, .. }
            | AgentEvent::Other { raw } => {
                let update = &raw["params"]["update"];
                if !matches!(
                    update["sessionUpdate"].as_str(),
                    Some("tool_call" | "tool_call_update")
                ) {
                    return Ok(());
                }
                if self.completed {
                    bail!("tool activity after completion");
                }
                self.bind_tool(update)?;
                if update.get("kind").is_some_and(|v| v != "execute") {
                    bail!("unexpected tool kind");
                }
                if let Some(input) = update.get("rawInput") {
                    // Claude streams an empty input before the full Bash call.
                    // It carries no authority; the permission must still have
                    // the complete exact command. After consent, reject erasure.
                    let empty_announcement = self.request_id.is_none()
                        && input.as_object().is_some_and(|input| input.is_empty());
                    if !empty_announcement {
                        Self::input(input, workspace, true)?;
                    }
                }
                if matches!(event, AgentEvent::ToolUse { .. }) {
                    if self.saw_tool {
                        bail!("repeated tool start");
                    }
                    self.saw_tool = true;
                }
                if matches!(event, AgentEvent::ToolResult { .. }) {
                    if !self.sent
                        || !self.saw_tool
                        || update["status"] != "completed"
                        || matches!(event, AgentEvent::ToolResult { denied: true, .. })
                    {
                        bail!("tool did not complete after a delivered one-time grant");
                    }
                    if update["rawOutput"]
                        .get("exit_code")
                        .is_some_and(|code| code != 0)
                        || update["rawOutput"]
                            .get("isError")
                            .is_some_and(|error| error != false)
                    {
                        bail!("tool output reports failure or unknown exit status");
                    }
                    self.completed = true;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        if !self.saw_tool || !self.sent || !self.completed {
            bail!("missing tool/permission/completion evidence");
        }
        Ok(())
    }
}
