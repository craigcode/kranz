//! Pure types and selection logic for the Windows contract-shell decision.
//!
//! [`crate::command_exec::shell_argv`] bottoms out every contract/gate
//! command on `cmd /C` on Windows and `sh -c` elsewhere. On Windows, when
//! the gate posture is uncontained, validation-contract commands should
//! instead run under the POSIX `sh` Git for Windows ships at
//! `<GitRoot>\usr\bin\sh.exe` — kranz already requires Git, so that binary
//! is always present once Git is. This module builds the types and the pure
//! selection/resolution logic only; it is NOT wired into execution (a later
//! feature does that), and it must not change `shell_argv` or any
//! `GateSandbox` arm.
//!
//! Not cfg-gated so its tests run on macOS and Linux too.
//!
//! Nothing outside this module calls these yet — the wiring lands in a
//! later feature — so everything here is allowed to look unused for now.
#![allow(dead_code)]

use std::path::{Component, Path, PathBuf};

/// Which concrete shell binary a resolved [`HostShell`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellKind {
    Sh,
    Cmd,
}

impl ShellKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            ShellKind::Sh => "sh",
            ShellKind::Cmd => "cmd",
        }
    }
}

/// The shell a lane resolved to, recorded against the command it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostShell {
    pub kind: ShellKind,
    pub program: PathBuf,
}

impl HostShell {
    /// Human-readable record of which shell ran a command. On Unix the
    /// program is always the bare `sh`, so the label is exactly `sh`. A
    /// discovered Windows sh labels with its absolute path so the record is
    /// unambiguous about which binary actually ran. cmd always labels as
    /// `cmd`, matching today's resolution.
    pub(crate) fn label(&self) -> String {
        match self.kind {
            ShellKind::Sh if self.program.as_os_str() == "sh" => "sh".to_string(),
            ShellKind::Sh => format!("sh ({})", self.program.display()),
            ShellKind::Cmd => "cmd".to_string(),
        }
    }
}

/// Which family of commands a shell decision applies to.
///
/// `Contract` covers validation-contract command assertions: validation
/// rounds, final-gate command assertions, and the approval contract lint.
/// `Repo` covers everything else that bottoms out in a shell: merge gates,
/// workspace bootstrap/readiness/data hooks, external gates, and
/// pack/standards gate declarations. Only `Contract` lane commands are
/// eligible to move off cmd on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellLane {
    Contract,
    Repo,
}

/// The host facts [`select_shell`] decides from, kept separate from any live
/// discovery so the decision stays a pure function of its inputs.
pub(crate) struct HostFacts {
    pub windows: bool,
    /// Whether the gate posture is AppContainer (contained).
    pub contained: bool,
    /// A Git-for-Windows `sh.exe` the caller has already resolved and
    /// validated, if any. Ignored off Windows and for the `Repo` lane.
    pub posix_sh: Option<PathBuf>,
}

/// Decide which shell a lane should run commands under, given the host
/// facts. Pure: performs no discovery or filesystem access itself.
///
/// Rules:
/// - Not Windows: `Sh` with the bare `sh`, for every lane.
/// - Windows, `Repo` lane: `Cmd`.
/// - Windows, `Contract` lane, contained: `Cmd`. Git's sh has not been
///   proven to run inside the AppContainer wrap — that proof is a CI
///   receipt written in a later feature, so the contained path stays cmd
///   until then.
/// - Windows, `Contract` lane, uncontained, `posix_sh` `Some(p)`: `Sh` with
///   program `p`.
/// - Otherwise (Windows, `Contract` lane, uncontained, no `posix_sh`):
///   `Cmd`, with program `cmd`, matching today's resolution.
pub(crate) fn select_shell(lane: ShellLane, facts: &HostFacts) -> HostShell {
    if !facts.windows {
        return HostShell {
            kind: ShellKind::Sh,
            program: PathBuf::from("sh"),
        };
    }
    if lane == ShellLane::Repo || facts.contained {
        return HostShell {
            kind: ShellKind::Cmd,
            program: PathBuf::from("cmd"),
        };
    }
    match &facts.posix_sh {
        Some(p) => HostShell {
            kind: ShellKind::Sh,
            program: p.clone(),
        },
        None => HostShell {
            kind: ShellKind::Cmd,
            program: PathBuf::from("cmd"),
        },
    }
}

/// Render a path into the spelling [`posix_sh_resolution`] compares by:
/// strip the `\\?\` verbatim prefix (consistently, so both sides of a
/// comparison are stripped the same way) and lowercase on Windows, where
/// path comparison is case-insensitive. A no-op off Windows beyond the
/// prefix strip, which also never fires off Windows.
fn comparable_path_components(path: &Path) -> Vec<String> {
    let rendered = path.as_os_str().to_string_lossy();
    let stripped = rendered.strip_prefix(r"\\?\").unwrap_or(&rendered);
    let normalized = PathBuf::from(stripped);
    normalized
        .components()
        .map(|c| match c {
            Component::Normal(part) => {
                let s = part.to_string_lossy().to_string();
                if cfg!(windows) {
                    s.to_lowercase()
                } else {
                    s
                }
            }
            other => {
                let s = other.as_os_str().to_string_lossy().to_string();
                if cfg!(windows) {
                    s.to_lowercase()
                } else {
                    s
                }
            }
        })
        .collect()
}

fn path_is_within(root: &[String], candidate: &[String]) -> bool {
    candidate.len() >= root.len() && candidate[..root.len()] == root[..]
}

/// Resolve the first candidate that is an absolute, existing regular file
/// outside every forbidden root, in order.
///
/// A candidate is refused if it is not absolute (bare names like `sh.exe`
/// and relative paths are refused), if `std::fs::canonicalize` fails (it
/// does not exist), or if its canonical path falls inside any canonicalized
/// forbidden root — compared by path components, case-insensitively on
/// Windows, with the `\\?\` verbatim prefix stripped consistently on both
/// sides so a canonicalized root compares correctly against a canonicalized
/// candidate.
pub(crate) fn posix_sh_resolution(
    candidates: &[PathBuf],
    forbidden_roots: &[PathBuf],
) -> Option<PathBuf> {
    let forbidden: Vec<Vec<String>> = forbidden_roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .map(|r| comparable_path_components(&r))
        .collect();

    for candidate in candidates {
        if !candidate.is_absolute() {
            continue;
        }
        let Ok(canonical) = std::fs::canonicalize(candidate) else {
            continue;
        };
        if !canonical.is_file() {
            continue;
        }
        let candidate_components = comparable_path_components(&canonical);
        if forbidden
            .iter()
            .any(|root| path_is_within(root, &candidate_components))
        {
            continue;
        }
        return Some(canonical);
    }
    None
}

/// Discover candidate Git-for-Windows `sh.exe` paths. Never reads a child's
/// PATH or env (uses the engine's own host environment) and never runs with
/// the worktree as cwd (uses a neutral directory). No registry access.
///
/// Runs the engine's own `git --exec-path` (typically
/// `C:/Program Files/Git/mingw64/libexec/git-core`) and walks up its
/// ancestors to the first directory containing `usr\bin\sh.exe`. Also
/// offers `%ProgramFiles%\Git\usr\bin\sh.exe` from the engine's own
/// environment. `usr\bin\sh.exe` (the real MSYS shell) is preferred over
/// the `bin\sh.exe` launcher wrapper, which is never offered here.
#[cfg(windows)]
pub(crate) fn git_for_windows_sh_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(output) = std::process::Command::new("git")
        .arg("--exec-path")
        .current_dir(std::env::temp_dir())
        .output()
    {
        if output.status.success() {
            let exec_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !exec_path.is_empty() {
                let exec_path = PathBuf::from(exec_path);
                for ancestor in exec_path.ancestors() {
                    let candidate = ancestor.join("usr").join("bin").join("sh.exe");
                    if candidate.is_file() {
                        out.push(candidate);
                        break;
                    }
                }
            }
        }
    }

    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        out.push(
            PathBuf::from(program_files)
                .join("Git")
                .join("usr")
                .join("bin")
                .join("sh.exe"),
        );
    }

    out
}

/// Build [`HostFacts`] and resolve the shell a lane should use.
///
/// On Windows, for the `Contract` lane, when `contained` is false, this
/// discovers and validates Git-for-Windows candidates against
/// `forbidden_roots`. Callers pass: the primary repo root, the active
/// root/worktree, the mission dir, gate scratch and `TMPDIR`, and every
/// sandbox write root (`crate::sandbox::write_allowlist(inputs)` where
/// inputs exist).
pub(crate) fn resolve_contract_shell(
    lane: ShellLane,
    contained: bool,
    forbidden_roots: &[PathBuf],
) -> HostShell {
    let windows = cfg!(windows);
    let posix_sh = if windows && lane == ShellLane::Contract && !contained {
        discover_posix_sh(forbidden_roots)
    } else {
        None
    };
    select_shell(
        lane,
        &HostFacts {
            windows,
            contained,
            posix_sh,
        },
    )
}

#[cfg(windows)]
fn discover_posix_sh(forbidden_roots: &[PathBuf]) -> Option<PathBuf> {
    posix_sh_resolution(&git_for_windows_sh_candidates(), forbidden_roots)
}

#[cfg(not(windows))]
fn discover_posix_sh(_forbidden_roots: &[PathBuf]) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path) {
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
    }

    #[test]
    fn posix_sh_resolution_bare_name_refused() {
        let dir = tempfile::tempdir().unwrap();
        let sh = dir.path().join("sh.exe");
        write_file(&sh);
        let bare = PathBuf::from("sh.exe");
        assert_eq!(posix_sh_resolution(&[bare], &[]), None);
    }

    #[test]
    fn posix_sh_resolution_relative_path_refused() {
        let relative = PathBuf::from("usr/bin/sh.exe");
        assert_eq!(posix_sh_resolution(&[relative], &[]), None);
    }

    #[test]
    fn posix_sh_resolution_planted_in_forbidden_root_refused() {
        let root = tempfile::tempdir().unwrap();
        let sh = root.path().join("usr").join("bin");
        std::fs::create_dir_all(&sh).unwrap();
        let sh = sh.join("sh.exe");
        write_file(&sh);

        let forbidden = vec![root.path().to_path_buf()];
        assert_eq!(posix_sh_resolution(&[sh], &forbidden), None);
    }

    #[test]
    fn posix_sh_resolution_planted_in_forbidden_root_refused_via_dotdot() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("usr").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let sh = bin.join("sh.exe");
        write_file(&sh);

        let via_dotdot = root
            .path()
            .join("sub")
            .join("..")
            .join("usr")
            .join("bin")
            .join("sh.exe");
        std::fs::create_dir_all(root.path().join("sub")).unwrap();

        let forbidden = vec![root.path().to_path_buf()];
        assert_eq!(posix_sh_resolution(&[via_dotdot], &forbidden), None);
    }

    #[cfg(unix)]
    #[test]
    fn posix_sh_resolution_planted_in_forbidden_root_refused_via_symlink() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("usr").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let sh = bin.join("sh.exe");
        write_file(&sh);

        let outside = tempfile::tempdir().unwrap();
        let link = outside.path().join("sh-link");
        std::os::unix::fs::symlink(&sh, &link).unwrap();

        let forbidden = vec![root.path().to_path_buf()];
        assert_eq!(posix_sh_resolution(&[link], &forbidden), None);
    }

    #[test]
    fn posix_sh_resolution_missing_file_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist").join("sh.exe");
        assert_eq!(posix_sh_resolution(&[missing], &[]), None);
    }

    #[test]
    fn posix_sh_resolution_valid_candidate_outside_every_root_yields_canonical_path() {
        let dir = tempfile::tempdir().unwrap();
        let sh = dir.path().join("sh.exe");
        write_file(&sh);

        let forbidden_root = tempfile::tempdir().unwrap();
        let forbidden = vec![forbidden_root.path().to_path_buf()];

        let expected = std::fs::canonicalize(&sh).unwrap();
        assert_eq!(posix_sh_resolution(&[sh], &forbidden), Some(expected));
    }

    #[test]
    fn posix_sh_resolution_first_valid_candidate_wins() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first-sh.exe");
        let second = dir.path().join("second-sh.exe");
        write_file(&first);
        write_file(&second);

        let expected = std::fs::canonicalize(&first).unwrap();
        assert_eq!(posix_sh_resolution(&[first, second], &[]), Some(expected));
    }

    #[test]
    fn contract_shell_lane_unix_yields_bare_sh_program_and_label() {
        let facts = HostFacts {
            windows: false,
            contained: false,
            posix_sh: None,
        };
        let shell = select_shell(ShellLane::Contract, &facts);
        assert_eq!(shell.kind, ShellKind::Sh);
        assert_eq!(shell.program, PathBuf::from("sh"));
        assert_eq!(shell.label(), "sh");

        let facts = HostFacts {
            windows: false,
            contained: true,
            posix_sh: None,
        };
        let shell = select_shell(ShellLane::Repo, &facts);
        assert_eq!(shell.kind, ShellKind::Sh);
        assert_eq!(shell.program, PathBuf::from("sh"));
        assert_eq!(shell.label(), "sh");
    }

    #[test]
    fn contract_shell_lane_windows_repo_lane_yields_cmd() {
        let facts = HostFacts {
            windows: true,
            contained: false,
            posix_sh: Some(PathBuf::from(r"C:\Program Files\Git\usr\bin\sh.exe")),
        };
        let shell = select_shell(ShellLane::Repo, &facts);
        assert_eq!(shell.kind, ShellKind::Cmd);
        assert_eq!(shell.program, PathBuf::from("cmd"));
        assert_eq!(shell.label(), "cmd");
    }

    #[test]
    fn contract_shell_lane_windows_contract_contained_yields_cmd() {
        let facts = HostFacts {
            windows: true,
            contained: true,
            posix_sh: Some(PathBuf::from(r"C:\Program Files\Git\usr\bin\sh.exe")),
        };
        let shell = select_shell(ShellLane::Contract, &facts);
        assert_eq!(shell.kind, ShellKind::Cmd);
        assert_eq!(shell.program, PathBuf::from("cmd"));
    }

    #[test]
    fn contract_shell_lane_windows_contract_uncontained_with_posix_sh_yields_sh() {
        let sh_path = PathBuf::from(r"C:\Program Files\Git\usr\bin\sh.exe");
        let facts = HostFacts {
            windows: true,
            contained: false,
            posix_sh: Some(sh_path.clone()),
        };
        let shell = select_shell(ShellLane::Contract, &facts);
        assert_eq!(shell.kind, ShellKind::Sh);
        assert_eq!(shell.program, sh_path);
        assert_eq!(shell.label(), format!("sh ({})", sh_path.display()));
    }

    #[test]
    fn contract_shell_lane_windows_contract_uncontained_without_posix_sh_yields_cmd() {
        let facts = HostFacts {
            windows: true,
            contained: false,
            posix_sh: None,
        };
        let shell = select_shell(ShellLane::Contract, &facts);
        assert_eq!(shell.kind, ShellKind::Cmd);
        assert_eq!(shell.program, PathBuf::from("cmd"));
    }
}
