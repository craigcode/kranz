//! macOS Seatbelt (SBPL) sandbox profile generation — Tier 2 filesystem
//! containment primitive. See docs/scoping/worker-sandboxing.md tier 2.
//!
//! This module only builds the profile string (and optionally writes it to
//! disk); wiring it into the `claude` spawn is a separate feature.

use std::path::{Path, PathBuf};

/// Inputs used to build a session's write-allowlist.
pub struct SandboxInputs {
    pub session_cwd: PathBuf,
    pub mission_dir: PathBuf,
    pub tmpdir: PathBuf,
    pub extra_write: Vec<PathBuf>,
}

/// Absolutize a path without requiring it to exist: canonicalize if possible,
/// otherwise join it onto the current directory when relative.
fn absolutize(path: &Path) -> PathBuf {
    if let Ok(canon) = path.canonicalize() {
        return canon;
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Escape a path for embedding in an SBPL string literal.
fn escape_sbpl_literal(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Generate an SBPL profile: deny-by-default, broad read, write limited to
/// subpaths of `session_cwd`, `mission_dir`, `tmpdir`, and each `extra_write`
/// entry. Network is explicitly allowed (this tier is filesystem-only).
pub fn generate_profile(inputs: &SandboxInputs) -> String {
    let mut write_paths: Vec<PathBuf> = vec![
        absolutize(&inputs.session_cwd),
        absolutize(&inputs.mission_dir),
        absolutize(&inputs.tmpdir),
    ];
    write_paths.extend(inputs.extra_write.iter().map(|p| absolutize(p)));

    let mut profile = String::new();
    profile.push_str("(version 1)\n");
    profile.push_str("(deny default)\n");
    profile.push('\n');
    profile.push_str("(allow process*)\n");
    profile.push_str("(allow signal (target self))\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str("(allow mach-register)\n");
    profile.push_str("(allow iokit-open)\n");
    profile.push('\n');
    profile.push_str("(allow file-read*)\n");
    profile.push('\n');
    profile.push_str("(allow network*)\n");
    profile.push('\n');
    profile.push_str("(allow file-write*\n");
    for p in &write_paths {
        profile.push_str(&format!("  (subpath \"{}\")\n", escape_sbpl_literal(p)));
    }
    profile.push_str(")\n");

    profile
}

/// Write the profile to a uniquely-named file under `dir`, returning its path.
pub fn write_profile_file(dir: &Path, profile: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("kranz-sandbox-{}.sb", uuid::Uuid::new_v4()));
    std::fs::write(&path, profile)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(session_cwd: &Path, mission_dir: &Path, tmpdir: &Path, extra: Vec<PathBuf>) -> SandboxInputs {
        SandboxInputs {
            session_cwd: session_cwd.to_path_buf(),
            mission_dir: mission_dir.to_path_buf(),
            tmpdir: tmpdir.to_path_buf(),
            extra_write: extra,
        }
    }

    #[test]
    fn sandbox_profile_contains_required_clauses() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(
            session.path(),
            mission.path(),
            tmp.path(),
            vec![extra.path().to_path_buf()],
        ));

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-read*)"));
        assert!(profile.contains("(allow network*)"));

        let session_abs = absolutize(session.path());
        let mission_abs = absolutize(mission.path());
        let tmp_abs = absolutize(tmp.path());
        let extra_abs = absolutize(extra.path());

        for p in [&session_abs, &mission_abs, &tmp_abs, &extra_abs] {
            let expected = format!("(subpath \"{}\")", escape_sbpl_literal(p));
            assert!(
                profile.contains(&expected),
                "profile missing subpath rule for {:?}:\n{}",
                p,
                profile
            );
        }
    }

    #[test]
    fn sandbox_profile_excludes_paths_outside_allowlist() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outsider = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(session.path(), mission.path(), tmp.path(), vec![]));

        let outsider_abs = absolutize(outsider.path());
        let forbidden = format!("(subpath \"{}\")", escape_sbpl_literal(&outsider_abs));
        assert!(
            !profile.contains(&forbidden),
            "profile unexpectedly allows write to path outside the allowlist"
        );
    }

    #[test]
    fn sandbox_profile_write_profile_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let profile = "(version 1)\n(deny default)\n";
        let path = write_profile_file(dir.path(), profile).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), profile);
        assert!(path.starts_with(dir.path()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_allows_inside_denies_outside() {
        use std::process::Command;

        if Command::new("which").arg("sandbox-exec").output().map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("sandbox-exec not found on this host; skipping");
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(session.path(), mission.path(), tmp.path(), vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        let inside_file = session.path().join("inside.txt");
        let inside_status = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo hi > {}", inside_file.display()))
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            inside_status.success(),
            "expected write inside session_cwd to succeed"
        );
        assert!(inside_file.exists(), "expected inside file to be created");

        let outside_file = outside.path().join(format!("kranz_sandbox_should_fail_{}", uuid::Uuid::new_v4()));
        let outside_status = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo hi > {}", outside_file.display()))
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            !outside_status.success(),
            "expected write outside allowlist to be denied"
        );
        assert!(
            !outside_file.exists(),
            "denied write must not have created the file"
        );
    }
}
