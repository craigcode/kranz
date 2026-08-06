//! Optional GitHub PR handoff for COMPLETE-but-unmerged missions.
//!
//! Local invariant: kranz **never pushes**. This module may:
//! - probe remotes / `ls-remote` (read-only git),
//! - run `gh pr create` when the mission branch already exists on the remote,
//! - or surface a copyable human push command for the operator.
//!
//! It must never invoke a mutating git push or the engine cloud-push helper.

use crate::error::{EngineError, Result};
use crate::git_ops::GitRepo;
use crate::scrub;
use crate::types::{Mission, MissionStatus};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

const TITLE_MAX: usize = 120;
const BODY_MAX: usize = 8_000;
const DEFAULT_REMOTE: &str = "origin";

/// What the operator should do next for a completed mission branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PrHandoff {
    /// Branch is not on the remote yet — show a copyable push; never run it.
    NeedsPush {
        command: String,
        remote: String,
        branch: String,
    },
    /// Remote branch exists — ready for `gh pr create` (or the prefilled cmd).
    ReadyToCreate {
        command: String,
        title: String,
        body: String,
        remote: String,
        branch: String,
        base: String,
    },
    /// Missing GitHub remote, `gh`, auth, etc.
    Unavailable { reason: String },
}

/// Inputs gathered from mission artifacts (report/plan optional).
#[derive(Debug, Clone)]
pub struct PrHandoffInputs<'a> {
    pub mission: &'a Mission,
    pub report_md: Option<&'a str>,
    pub plan_md: Option<&'a str>,
    pub remote: &'a str,
    /// When false, skip `git ls-remote` (no network). Used by Slack notify
    /// so a hung remote cannot stall the bridge; the dashboard still probes.
    pub probe_remote: bool,
}

/// Assess PR handoff for a COMPLETE mission. Pure side-effect surface is
/// limited to read-only git probes + `which`-style `gh` discovery.
pub fn assess(repo_root: &Path, inputs: &PrHandoffInputs<'_>) -> PrHandoff {
    if inputs.mission.status != MissionStatus::Complete {
        return PrHandoff::Unavailable {
            reason: format!(
                "mission is {:?}; PR handoff is only for COMPLETE missions",
                inputs.mission.status
            ),
        };
    }
    let branch = &inputs.mission.mission_branch;
    if !branch.starts_with("kranz/") {
        return PrHandoff::Unavailable {
            reason: format!("mission branch {branch:?} is not a kranz/* ref"),
        };
    }

    let repo = match GitRepo::open(repo_root) {
        Ok(r) => r,
        Err(e) => {
            return PrHandoff::Unavailable {
                reason: format!("cannot open git repo: {e}"),
            };
        }
    };

    let remote = inputs.remote;
    let remote_url = match repo.remote_url(remote) {
        Ok(Some(url)) => url,
        Ok(None) => {
            return PrHandoff::Unavailable {
                reason: format!("no git remote named {remote:?}"),
            };
        }
        Err(e) => {
            return PrHandoff::Unavailable {
                reason: format!("could not read remote {remote:?}: {e}"),
            };
        }
    };
    if !looks_like_github_remote(&remote_url) {
        return PrHandoff::Unavailable {
            reason: format!(
                "remote {remote:?} ({remote_url}) does not look like GitHub; \
                 PR handoff uses `gh`"
            ),
        };
    }

    if !inputs.probe_remote {
        // No network: point the operator at the dashboard Delivered panel,
        // which performs the remote-branch probe.
        return PrHandoff::Unavailable {
            reason: "open the dashboard Delivered panel for PR handoff \
                     (Slack skips the remote-branch network probe)"
                .into(),
        };
    }

    let on_remote = match repo.remote_has_branch(remote, branch) {
        Ok(v) => v,
        Err(e) => {
            return PrHandoff::Unavailable {
                reason: format!("could not probe remote branch {branch:?}: {e}"),
            };
        }
    };

    if !on_remote {
        return PrHandoff::NeedsPush {
            command: format!("git push {remote} {branch}"),
            remote: remote.to_string(),
            branch: branch.clone(),
        };
    }

    if which_gh().is_none() {
        return PrHandoff::Unavailable {
            reason: "`gh` CLI not found on PATH (install GitHub CLI to open a PR)".to_string(),
        };
    }

    let (title, body) = build_title_body(inputs);
    let base = &inputs.mission.base_branch;
    let command = format_gh_pr_create_command(base, branch, &title, &body);
    PrHandoff::ReadyToCreate {
        command,
        title,
        body,
        remote: remote.to_string(),
        branch: branch.clone(),
        base: base.clone(),
    }
}

/// Convenience: assess from on-disk mission state + report/plan files.
pub fn assess_mission(repo_root: &Path, mission_id: &str) -> Result<PrHandoff> {
    let paths = crate::paths::MissionPaths::new(repo_root, mission_id);
    if !paths.events_file().is_file() {
        return Err(EngineError::Config(format!(
            "unknown mission '{mission_id}'"
        )));
    }
    let state: crate::types::MissionState = {
        let text = std::fs::read_to_string(paths.state_file()).map_err(|e| {
            EngineError::Config(format!("cannot read state for '{mission_id}': {e}"))
        })?;
        serde_json::from_str(&text).map_err(|e| {
            EngineError::Config(format!("invalid state.json for '{mission_id}': {e}"))
        })?
    };
    let report = std::fs::read_to_string(paths.report_file()).ok();
    let plan = std::fs::read_to_string(paths.plan_md_file()).ok();
    Ok(assess(
        repo_root,
        &PrHandoffInputs {
            mission: &state.mission,
            report_md: report.as_deref(),
            plan_md: plan.as_deref(),
            remote: DEFAULT_REMOTE,
            probe_remote: true,
        },
    ))
}

/// Run `gh pr create` for a [`PrHandoff::ReadyToCreate`] only. Never pushes.
pub fn create_pull_request(repo_root: &Path, handoff: &PrHandoff) -> Result<String> {
    let PrHandoff::ReadyToCreate {
        title,
        body,
        branch,
        base,
        ..
    } = handoff
    else {
        return Err(EngineError::Config(
            "create_pull_request requires ReadyToCreate handoff (remote branch present)".into(),
        ));
    };
    let gh = which_gh().ok_or_else(|| EngineError::Config("`gh` CLI not found on PATH".into()))?;
    // Title/body via flags — never shell-interpolated. No `git` argv here.
    let output = Command::new(&gh)
        .current_dir(repo_root)
        .args([
            "pr", "create", "--base", base, "--head", branch, "--title", title, "--body", body,
        ])
        .output()
        .map_err(|e| EngineError::Backend(format!("failed to spawn gh: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(EngineError::Backend(format!(
            "gh pr create failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn looks_like_github_remote(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("github.com") || lower.contains("github.")
}

fn which_gh() -> Option<PathBuf> {
    which_binary("gh")
}

fn which_binary(name: &str) -> Option<PathBuf> {
    let Ok(path_var) = std::env::var("PATH") else {
        return None;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

fn build_title_body(inputs: &PrHandoffInputs<'_>) -> (String, String) {
    let goal = scrub::scrub_and_truncate(inputs.mission.goal.trim(), TITLE_MAX);
    let title = if goal.is_empty() {
        format!("kranz mission {}", inputs.mission.id)
    } else {
        goal
    };

    let mut body = String::new();
    body.push_str(&format!(
        "## Mission `{}`\n\n",
        scrub::scrub(&inputs.mission.id)
    ));
    body.push_str(&format!(
        "**Branch:** `{}` → `{}`\n\n",
        inputs.mission.mission_branch, inputs.mission.base_branch
    ));
    body.push_str(
        "**Merge gates:** not yet run. Merge gates run at merge time \
         (`kranz` / dashboard Merge) and are still pending. This PR does \
         **not** mean the mission has landed on the base branch.\n\n",
    );
    body.push_str(&format!(
        "**Artifacts:** `.kranz/missions/{}/plan.md`, \
         `.kranz/missions/{}/report.md`\n\n",
        inputs.mission.id, inputs.mission.id
    ));

    if let Some(report) = inputs.report_md.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str("## Report excerpt\n\n");
        body.push_str(&scrub::scrub_and_truncate(report, 3_000));
        body.push_str("\n\n");
    } else if let Some(plan) = inputs.plan_md.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str("## Plan excerpt\n\n");
        body.push_str(&scrub::scrub_and_truncate(plan, 2_000));
        body.push_str("\n\n");
    }

    if !inputs.mission.validation_contract.is_empty() {
        body.push_str("## Validation contract (from mission)\n\n");
        for a in inputs.mission.validation_contract.iter().take(12) {
            body.push_str(&format!(
                "- `{}`: {}\n",
                scrub::scrub(&a.id),
                scrub::scrub_and_truncate(&a.statement, 160)
            ));
        }
        body.push('\n');
    }

    let body = scrub::scrub_and_truncate(&body, BODY_MAX);
    (title, body)
}

fn format_gh_pr_create_command(base: &str, head: &str, title: &str, body: &str) -> String {
    // Prefilled copyable command; body truncated for shell pasteability.
    let end = floor_char_boundary(body, 400);
    let body_short = if body.len() > end {
        format!("{}…", &body[..end])
    } else {
        body.to_string()
    };
    format!("gh pr create --base {base} --head {head} --title {title:?} --body {body_short:?}")
}

fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Assertion, AssertionCheck, MissionStatus};
    use chrono::Utc;

    fn sample_mission(status: MissionStatus) -> Mission {
        Mission {
            id: "m-test".into(),
            goal: "Ship PR handoff without auto-push".into(),
            validation_contract: vec![Assertion {
                id: "a1".into(),
                statement: "tests pass".into(),
                check: AssertionCheck::Command,
                command: Some("cargo test".into()),
                pty_script: None,
            }],
            milestones: vec![],
            status,
            created_at: Utc::now(),
            base_branch: "main".into(),
            base_sha: Some("abc".into()),
            mission_branch: "kranz/mission-m-test".into(),
            command_grants: vec![],
            touch_set: vec![],
            deny_exceptions: vec![],
            egress_grants: vec![],
            executor_route: None,
        }
    }

    #[test]
    fn non_complete_is_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let m = sample_mission(MissionStatus::Running);
        let h = assess(
            tmp.path(),
            &PrHandoffInputs {
                mission: &m,
                report_md: None,
                plan_md: None,
                remote: "origin",
                probe_remote: true,
            },
        );
        assert!(matches!(h, PrHandoff::Unavailable { .. }));
    }

    #[test]
    fn body_states_merge_gates_pending() {
        let m = sample_mission(MissionStatus::Complete);
        let (title, body) = build_title_body(&PrHandoffInputs {
            mission: &m,
            report_md: Some("# Report\n\nAll good."),
            plan_md: None,
            remote: "origin",
            probe_remote: true,
        });
        assert!(title.contains("Ship PR handoff"));
        assert!(body.contains("still pending"));
        assert!(!body.to_ascii_lowercase().contains("merge gates passed"));
        assert!(body.contains("m-test"));
    }

    #[test]
    fn pr_handoff_source_never_invokes_git_push() {
        // String/command deny on production code only (exclude this test module).
        let src = include_str!("pr_handoff.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        let code: String = prod
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && !t.starts_with("//!") && !t.starts_with('*')
            })
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [
            "push_mission_branch",
            ".args([\"push\"",
            ".arg(\"push\")",
            "[\"push\",",
            "\"git\", \"push\"",
            "Command::new(\"git\")",
        ] {
            assert!(
                !code.contains(needle),
                "pr_handoff.rs must not contain {needle:?}"
            );
        }
        // The copyable human command is allowed as a string template only:
        assert!(code.contains("git push {remote} {branch}"));
    }

    #[test]
    fn needs_push_command_is_copyable_not_executed() {
        let handoff = PrHandoff::NeedsPush {
            command: "git push origin kranz/mission-m-test".into(),
            remote: "origin".into(),
            branch: "kranz/mission-m-test".into(),
        };
        let err = create_pull_request(Path::new("/tmp"), &handoff).unwrap_err();
        assert!(
            err.to_string().contains("ReadyToCreate"),
            "must refuse create on NeedsPush: {err}"
        );
    }

    #[test]
    fn skip_remote_probe_does_not_call_network() {
        let tmp = tempfile::tempdir().unwrap();
        // Minimal git repo with a github remote — assess must not need ls-remote.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?} {:?}", out);
        };
        git(&["init"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(tmp.path().join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-m", "i"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/example/repo.git",
        ]);

        let m = sample_mission(MissionStatus::Complete);
        let h = assess(
            tmp.path(),
            &PrHandoffInputs {
                mission: &m,
                report_md: None,
                plan_md: None,
                remote: "origin",
                probe_remote: false,
            },
        );
        match h {
            PrHandoff::Unavailable { reason } => {
                assert!(reason.contains("dashboard"), "{reason}");
            }
            other => panic!("expected Unavailable dashboard pointer, got {other:?}"),
        }
    }
}
