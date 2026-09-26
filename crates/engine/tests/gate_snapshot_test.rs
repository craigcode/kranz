use kranz_engine::gate_evaluation::evidence::{SnapshotEntry, SnapshotInventory};
use kranz_engine::gate_evaluation::snapshot::SourceSnapshot;
use kranz_engine::git_ops::GitRepo;
use std::path::Path;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repo() -> (tempfile::TempDir, GitRepo) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "Fixture"]);
    git(
        dir.path(),
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(dir.path().join("tracked.rs"), "original\n").unwrap();
    std::fs::write(dir.path().join("removed.rs"), "remove me\n").unwrap();
    std::fs::write(dir.path().join(".gitignore"), "cache/\n.env\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "fixture"]);
    let repo = GitRepo::open(dir.path()).unwrap();
    (dir, repo)
}

#[test]
fn gate_snapshot_binds_dirty_untracked_deleted_bytes_without_history_or_authority() {
    let (dir, repo) = repo();
    let base = repo.head_sha().unwrap();
    std::fs::write(dir.path().join("tracked.rs"), "changed\n").unwrap();
    std::fs::remove_file(dir.path().join("removed.rs")).unwrap();
    std::fs::write(dir.path().join("new source.rs"), "new\n").unwrap();
    for (name, bytes) in [
        (
            ".kranz/missions/m-fixture/runs/transcript.jsonl",
            "worker reasoning",
        ),
        (".claude/config.json", "provider state"),
        (".kranz/serve.token", "authority fixture"),
        ("cache/ignored.txt", "ignored cache"),
        (".env", "environment fixture"),
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let snapshot = SourceSnapshot::capture(&repo, &base).unwrap();
    let inventory: SnapshotInventory = serde_json::from_slice(&snapshot.inventory).unwrap();
    assert!(inventory
        .entries
        .iter()
        .any(|e| matches!(e, SnapshotEntry::Deleted { path } if path.as_str() == "removed.rs")));
    for (path, expected) in [
        ("tracked.rs", b"changed\n".as_slice()),
        ("new source.rs", b"new\n".as_slice()),
    ] {
        let id = inventory
            .entries
            .iter()
            .find_map(|e| match e {
                SnapshotEntry::File {
                    path: p,
                    artifact_id,
                    ..
                } if p.as_str() == path => Some(artifact_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(snapshot.files[id], expected);
    }
    let names: Vec<_> = inventory
        .entries
        .iter()
        .map(|e| match e {
            SnapshotEntry::File { path, .. } | SnapshotEntry::Deleted { path } => path.as_str(),
        })
        .collect();
    assert_eq!(
        names,
        [".gitignore", "new source.rs", "removed.rs", "tracked.rs"]
    );
    assert_eq!(snapshot.excluded_paths.len(), 3);
    snapshot.verify_current(&repo).unwrap();
    std::fs::write(dir.path().join("new source.rs"), "later edit\n").unwrap();
    assert!(snapshot.verify_current(&repo).is_err());
    assert_eq!(repo.head_sha().unwrap(), base, "HEAD alone misses the edit");
}

#[test]
fn gate_snapshot_rejects_oversized_source_and_detects_head_movement() {
    let (dir, repo) = repo();
    let snapshot = SourceSnapshot::capture(&repo, "HEAD").unwrap();
    git(
        dir.path(),
        &["commit", "--allow-empty", "-m", "another commit"],
    );
    assert!(snapshot.verify_current(&repo).is_err());
    let file = std::fs::File::create(dir.path().join("large.bin")).unwrap();
    file.set_len(kranz_engine::gate_evaluation::snapshot::MAX_SOURCE_FILE_BYTES + 1)
        .unwrap();
    assert!(SourceSnapshot::capture(&repo, "HEAD")
        .unwrap_err()
        .contains("bounded regular file"));
}

#[cfg(unix)]
#[test]
fn gate_snapshot_refuses_links_non_utf8_and_changed_executable_mode() {
    use std::os::unix::{
        ffi::OsStringExt,
        fs::{symlink, PermissionsExt},
    };
    let (dir, repo) = repo();
    let snapshot = SourceSnapshot::capture(&repo, "HEAD").unwrap();
    std::fs::set_permissions(
        dir.path().join("tracked.rs"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(snapshot.verify_current(&repo).is_err());
    std::fs::hard_link(dir.path().join("tracked.rs"), dir.path().join("hardlink")).unwrap();
    assert!(SourceSnapshot::capture(&repo, "HEAD")
        .unwrap_err()
        .contains("hard-linked"));
    std::fs::remove_file(dir.path().join("hardlink")).unwrap();
    symlink("tracked.rs", dir.path().join("link")).unwrap();
    assert!(SourceSnapshot::capture(&repo, "HEAD")
        .unwrap_err()
        .contains("without following links"));
    std::fs::remove_file(dir.path().join("link")).unwrap();
    // APFS rejects non-UTF-8 filenames, but Git can retain such names in
    // its index on every Unix host. No filesystem support is needed to
    // exercise the evidence boundary's lossless-path refusal.
    let blob = repo.rev_parse("HEAD:tracked.rs").unwrap();
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "update-index",
            "--add",
            "--cacheinfo",
            "100644",
            &blob,
        ])
        .arg(std::ffi::OsString::from_vec(b"non-utf8-\xff".to_vec()))
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(SourceSnapshot::capture(&repo, "HEAD")
        .unwrap_err()
        .contains("non-UTF-8"));
}

#[cfg(unix)]
#[test]
fn gate_snapshot_never_follows_a_replaced_tracked_parent() {
    use std::os::unix::fs::symlink;
    let (dir, repo) = repo();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "source").unwrap();
    git(dir.path(), &["add", "src"]);
    git(dir.path(), &["commit", "-m", "nested source"]);
    std::fs::remove_dir_all(dir.path().join("src")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("lib.rs"), "must not be read").unwrap();
    symlink(outside.path(), dir.path().join("src")).unwrap();
    assert!(SourceSnapshot::capture(&repo, "HEAD").is_err());
}

#[test]
fn gate_snapshot_private_path_selection_is_case_insensitive_and_directory_bounded() {
    use kranz_engine::gate_evaluation::snapshot::excluded;
    for private in [
        ".CLAUDE/auth.json",
        ".AWS/credentials",
        ".kranz/missions/m-1/report.md",
        "src/.env.local",
        ".NPMRC",
        "packages/ui/.NPMRC",
        "nested/.ssh/id_ed25519",
        "examples/.kranz/serve.token",
    ] {
        assert!(excluded(private), "{private}");
    }
    for source in [
        ".claude-notes.md",
        "nested/.claude-notes.md",
        "nested/.kranz/workspace.json",
        "src/env.rs",
        ".kranz/workspace.json",
        ".kranz/tickets/gate.md",
    ] {
        assert!(!excluded(source), "{source}");
    }
}

#[test]
fn gate_snapshot_portable_dotfiles_and_spaces_keep_traversal_and_alias_denials() {
    use kranz_engine::gate_evaluation::protocol::WirePath;
    for path in [
        ".gitignore",
        ".github/workflows/ci.yml",
        "inputs/new source.rs",
        "outputs/.receipt",
    ] {
        assert!(WirePath::try_from(path.to_string()).is_ok(), "{path}");
    }
    for path in [
        ".",
        "..",
        "inputs/../escape",
        "inputs/./file",
        "inputs/CON.txt",
        "inputs/COM¹.txt",
        "inputs/CONIN$",
        "inputs/a\u{202e}b",
        "inputs/file.",
        "inputs/file ",
        "inputs/ file",
        "/absolute",
        "C:/drive",
        "inputs/a:b",
        "inputs/a\\b",
        "inputs/a\nb",
    ] {
        assert!(WirePath::try_from(path.to_string()).is_err(), "{path}");
    }
}

#[test]
fn gate_snapshot_excludes_nested_credentials_and_private_pem_but_keeps_public_certificates() {
    let (dir, repo) = repo();
    // Build non-secret fixtures at runtime so source scanners keep rejecting
    // actual committed private-key blocks without a test-file waiver.
    let key =
        |kind| format!("-----BEGIN {kind}-----\nnested-credential-marker\n-----END {kind}-----\n");
    let rsa = key("RSA PRIVATE KEY");
    let pkcs8 = key("PRIVATE KEY");
    for (name, content) in [
        ("packages/ui/.npmrc", "nested-credential-marker"),
        ("nested/.ssh/id_rsa", "nested-credential-marker"),
        ("nested/.kranz/config.json", "nested-credential-marker"),
        ("tls/client.pem", rsa.as_str()),
        ("tls/no-extension", pkcs8.as_str()),
        (
            "tls/public.pem",
            "-----BEGIN CERTIFICATE-----\npublic-certificate-marker\n-----END CERTIFICATE-----\n",
        ),
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    // Tracked private files must also be excluded; Git ignore rules are not a boundary.
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "credential fixtures"]);
    let snapshot = SourceSnapshot::capture(&repo, "HEAD").unwrap();
    assert_eq!(snapshot.excluded_paths.len(), 5);
    assert!(snapshot
        .files
        .values()
        .all(|bytes| !String::from_utf8_lossy(bytes).contains("nested-credential-marker")));
    assert!(snapshot
        .files
        .values()
        .any(|bytes| String::from_utf8_lossy(bytes).contains("public-certificate-marker")));
    let selection: serde_json::Value = serde_json::from_slice(&snapshot.selection).unwrap();
    assert_eq!(
        selection["excludedPaths"],
        serde_json::json!(snapshot.excluded_paths)
    );
    snapshot.verify_current(&repo).unwrap();
    // Becoming private changes the selection binding, even if HEAD stays fixed.
    std::fs::write(dir.path().join("tls/public.pem"), key("EC PRIVATE KEY")).unwrap();
    assert!(snapshot.verify_current(&repo).is_err());
}

#[test]
fn gate_snapshot_acceptance_refuses_hidden_flags_dirty_bytes_and_excluded_changes() {
    let (dir, repo) = repo();
    let base = repo.head_sha().unwrap();
    std::fs::write(dir.path().join("tracked.rs"), "uncommitted replacement").unwrap();
    let snapshot = SourceSnapshot::capture(&repo, &base).unwrap();
    assert!(snapshot
        .verify_candidate(&repo, "m-fixture")
        .unwrap_err()
        .contains("committed candidate"));
    git(
        dir.path(),
        &["update-index", "--skip-worktree", "tracked.rs"],
    );
    assert!(SourceSnapshot::capture(&repo, &base)
        .unwrap_err()
        .contains("hidden index"));
    git(
        dir.path(),
        &["update-index", "--no-skip-worktree", "tracked.rs"],
    );
    git(dir.path(), &["checkout", "--", "tracked.rs"]);
    std::fs::write(dir.path().join("untracked.rs"), "uncommitted dependency").unwrap();
    let snapshot = SourceSnapshot::capture(&repo, &base).unwrap();
    assert!(snapshot
        .verify_candidate(&repo, "m-fixture")
        .unwrap_err()
        .contains("untracked path"));
    std::fs::remove_file(dir.path().join("untracked.rs")).unwrap();
    std::fs::create_dir_all(dir.path().join("src/.config")).unwrap();
    std::fs::write(dir.path().join("src/.config/payload.rs"), "hidden source").unwrap();
    std::fs::write(
        dir.path().join("tracked.rs"),
        format!("// source\n-----BEGIN {}-----\n", "PRIVATE KEY"),
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "candidate"]);
    let snapshot = SourceSnapshot::capture(&repo, &base).unwrap();
    let error = snapshot.verify_candidate(&repo, "m-fixture").unwrap_err();
    assert!(
        error.contains("src/.config/payload.rs") && error.contains("tracked.rs"),
        "{error}"
    );
    assert!(!snapshot
        .files
        .values()
        .any(|b| b.windows(13).any(|v| v == b"hidden source")));
}

#[test]
fn gate_snapshot_acceptance_supports_web_and_unicode_paths() {
    let (dir, repo) = repo();
    let base = repo.head_sha().unwrap();
    for name in [
        "src/[id].tsx",
        "src/+page.svelte",
        "@types/index.ts",
        "café.rs",
        "日本語.rs",
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "source").unwrap();
    }
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "ordinary filenames"]);
    SourceSnapshot::capture(&repo, &base)
        .unwrap()
        .verify_candidate(&repo, "m-fixture")
        .unwrap();
}

#[test]
fn gate_snapshot_detected_json_credentials_do_not_become_source_evidence() {
    let (dir, repo) = repo();
    let base = repo.head_sha().unwrap();
    let credential = format!("ghp_{}", "Ab7Cd9Ef2".repeat(5));
    assert!(!kranz_engine::scrub::scan_text(&credential).is_empty());
    let bytes = serde_json::to_vec(&serde_json::json!({"credential":credential})).unwrap();
    std::fs::write(dir.path().join("credentials.json"), &bytes).unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "credential fixture"]);
    let snapshot = SourceSnapshot::capture(&repo, &base).unwrap();
    assert!(snapshot
        .excluded_paths
        .contains(&"credentials.json".to_string()));
    assert!(snapshot
        .verify_candidate(&repo, "m-fixture")
        .unwrap_err()
        .contains("credentials.json"));
    assert!(snapshot
        .files
        .values()
        .all(|v| !String::from_utf8_lossy(v).contains(&credential)));
}
