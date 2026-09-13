use super::*;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    // Git for Windows rejects std's verbatim prefix in command arguments.
    let args: Vec<_> = args
        .iter()
        .map(|arg| arg.strip_prefix(r"\\?\").unwrap_or(arg))
        .collect();
    let out = Command::new("git")
        .args(["-c", "core.hooksPath=/nonexistent-kranz-fixture-hooks"])
        .args(&args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("missing-global-config"))
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn fixture() -> (tempfile::TempDir, SandboxInputs) {
    let dir = tempfile::tempdir().unwrap();
    let root = absolutize(dir.path());
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.name", "Fixture"]);
    git(&root, &["config", "user.email", "fixture@example.invalid"]);
    git(
        &root,
        &[
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    );
    let inputs = SandboxInputs {
        enforce: crate::types::SandboxEnforce::Fs,
        session_cwd: root.clone(),
        mission_dir: root.join(".kranz/missions/m-git-protection"),
        tmpdir: root.join("scratch"),
        extra_write: Vec::new(),
        egress: Vec::new(),
        validator_read_deny_roots: Vec::new(),
    };
    std::fs::create_dir_all(&inputs.tmpdir).unwrap();
    (dir, inputs)
}

#[test]
fn git_config_protection_preserves_disabled_worktree_config_absence() {
    let (_dir, inputs) = fixture();
    let config = inputs.session_cwd.join(".git/config");
    let before = std::fs::read(&config).unwrap();
    validate_git_config_protection(&inputs, true).unwrap();
    assert!(!inputs.session_cwd.join(".git/config.worktree").exists());
    assert_eq!(std::fs::read(config).unwrap(), before);
}

#[test]
fn git_config_protection_refuses_active_absence_only_when_writable_and_mount_based() {
    let (_dir, inputs) = fixture();
    git(
        &inputs.session_cwd,
        &["config", "extensions.worktreeConfig", "true"],
    );
    let error = validate_git_config_protection(&inputs, true).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("absent active Git configuration"),
        "{error}"
    );
    validate_git_config_protection(&inputs, false).unwrap();
    std::fs::write(inputs.session_cwd.join(".git/config.worktree"), "").unwrap();
    validate_git_config_protection(&inputs, true).unwrap();
}

#[test]
fn git_config_protection_resolves_linked_worktree_config_and_rename_ancestors() {
    // Authority masks read HOME repeatedly. Keep the shared metadata check
    // out of other fixtures' temporary HOME values and directory teardown.
    let _env_lock = crate::agent_env::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_dir, mut inputs) = fixture();
    let main = inputs.session_cwd.clone();
    let worktree = main.join("linked");
    git(&main, &["config", "extensions.worktreeConfig", "true"]);
    std::fs::write(main.join(".git/config.worktree"), "").unwrap();
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    inputs.session_cwd = worktree.clone();
    let linked_git = main.join(".git/worktrees/linked");
    let linked_config = linked_git.join("config.worktree");
    if linked_config.exists() {
        std::fs::remove_file(&linked_config).unwrap();
    }
    // Shared metadata outside writable roots needs no mount to preserve absence.
    validate_git_config_protection(&inputs, true).unwrap();
    inputs.extra_write.push(main.clone());
    assert!(validate_git_config_protection(&inputs, true).is_err());
    std::fs::write(linked_git.join("config.worktree"), "").unwrap();
    validate_git_config_protection(&inputs, true).unwrap();
    let denies = git_metadata_write_denies(&inputs);
    for file in [
        main.join(".git/config"),
        linked_git.join("config.worktree"),
        linked_git.join("commondir"),
        worktree.join(".git"),
    ] {
        assert!(
            denies
                .files
                .iter()
                .any(|path| absolutize(path) == absolutize(&file)),
            "missing {}: {:?}",
            file.display(),
            denies.files
        );
    }
    let nodes = git_metadata_mount_nodes(&inputs);
    for node in [main.join(".git"), main.join(".git/worktrees"), linked_git] {
        assert!(
            nodes
                .iter()
                .any(|path| absolutize(path) == absolutize(&node)),
            "unsealed rename ancestor {}",
            node.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn git_config_protection_refuses_symlinks_and_hardlink_aliases() {
    use std::os::unix::fs::symlink;
    let (_dir, inputs) = fixture();
    let config = inputs.session_cwd.join(".git/config");
    let alias = inputs.session_cwd.join("config-alias");
    std::fs::hard_link(&config, &alias).unwrap();
    assert!(validate_git_config_protection(&inputs, false)
        .unwrap_err()
        .to_string()
        .contains("multiply linked"));
    std::fs::remove_file(&alias).unwrap();
    std::fs::rename(&config, &alias).unwrap();
    symlink(&alias, &config).unwrap();
    assert!(validate_git_config_protection(&inputs, false)
        .unwrap_err()
        .to_string()
        .contains("symlink"));
    std::fs::remove_file(&config).unwrap();
    std::fs::rename(&alias, &config).unwrap();
    let metadata = inputs.session_cwd.join(".git");
    let moved = inputs.session_cwd.join("git-alias");
    std::fs::rename(&metadata, &moved).unwrap();
    symlink(&moved, &metadata).unwrap();
    assert!(validate_git_config_protection(&inputs, false)
        .unwrap_err()
        .to_string()
        .contains("symlink"));
}

#[test]
fn git_config_protection_refuses_ordinary_include_targets() {
    let (_dir, inputs) = fixture();
    let target = inputs.session_cwd.join("ordinary.cfg");
    std::fs::write(&target, "[user]\nname = Fixture\n").unwrap();
    git(
        &inputs.session_cwd,
        &["config", "include.path", target.to_str().unwrap()],
    );
    let error = validate_git_config_protection(&inputs, false).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ordinary repository config includes"),
        "{error}"
    );
}

#[cfg(unix)]
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(unix)]
#[test]
#[ignore = "private engine subprocess for the synchronized config race test"]
fn git_race_engine_fixture() {
    let Some(root) = std::env::var_os("KRANZ_GIT_RACE_ROOT") else {
        return;
    };
    let repo = crate::git_ops::GitRepo::open(PathBuf::from(root)).unwrap();
    repo.is_clean().unwrap();
    std::process::exit(0);
}

/// The Git shim stops *after* the driver's last preflight, immediately before
/// real status executes. A contained writer then attempts the exact mutable
/// inputs and rename routes; the engine proceeds only after its completion.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn git_config_protection_synchronized_writer_cannot_race_engine_git() {
    use std::os::unix::fs::PermissionsExt;
    #[cfg(target_os = "macos")]
    let supported = Command::new("sandbox-exec")
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .output()
        .is_ok_and(|out| out.status.success());
    #[cfg(target_os = "linux")]
    let supported = Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--", "/bin/true"])
        .output()
        .is_ok_and(|out| out.status.success());
    if !supported {
        eprintln!(
            "SKIP-UNDER-WRAP: native Git configuration race fixture cannot apply containment"
        );
        return;
    }
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    for layout in ["checkout", "linked", "linked-subdir"] {
        let linked = layout != "checkout";
        let (_dir, mut inputs) = fixture();
        let main = inputs.session_cwd.clone();
        git(&main, &["config", "extensions.worktreeConfig", "true"]);
        std::fs::write(main.join(".git/config.worktree"), "").unwrap();
        let git_dir = if linked {
            let worktree = main.join("linked");
            git(
                &main,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "linked",
                    worktree.to_str().unwrap(),
                ],
            );
            inputs.session_cwd = worktree;
            inputs.extra_write.push(main.clone());
            main.join(".git/worktrees/linked")
        } else {
            main.join(".git")
        };
        std::fs::write(git_dir.join("config.worktree"), "").unwrap();
        if layout == "linked-subdir" {
            inputs.session_cwd = inputs.session_cwd.join("subdir");
            std::fs::create_dir(&inputs.session_cwd).unwrap();
        }
        validate_git_config_protection(&inputs, cfg!(target_os = "linux")).unwrap();
        let config = main.join(".git/config");
        let config_before = std::fs::read(&config).unwrap();
        let ready = inputs.tmpdir.join("preflight-complete");
        let done = inputs.tmpdir.join("writer-done");
        let violation = inputs.tmpdir.join("unexpected-write");
        let bin = inputs.tmpdir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let shim = bin.join("git");
        std::fs::write(&shim, format!(
            "#!/bin/sh\ncase \" $* \" in *' status '*) touch {ready}; n=0; while [ ! -e {done} ]; do n=$((n+1)); [ $n -lt 1000 ] || exit 98; sleep 0.01; done;; esac\nexec {git} \"$@\"\n",
            ready=quote(&ready), done=quote(&done), git=quote(&real_git)
        )).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let engine = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "sandbox::git_config_protection_tests::git_race_engine_fixture",
                "--nocapture",
            ])
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("KRANZ_GIT_RACE_ROOT", &inputs.session_cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut script = format!(
            "n=0; while [ ! -e {} ]; do n=$((n+1)); [ $n -lt 1000 ] || exit 97; sleep 0.01; done\n",
            quote(&ready)
        );
        let mut protected = vec![config.clone(), git_dir.join("config.worktree")];
        if linked {
            protected.extend([
                git_marker(&inputs.session_cwd).unwrap(),
                git_dir.join("commondir"),
            ]);
        }
        for (index, file) in protected.iter().enumerate() {
            let replacement = inputs.tmpdir.join(format!("replacement-{index}"));
            std::fs::write(&replacement, "untrusted replacement").unwrap();
            script.push_str(&format!(
                "if printf changed >> {file}; then touch {violation}; fi\nif mv {replacement} {file}; then touch {violation}; fi\n",
                file=quote(file), replacement=quote(&replacement), violation=quote(&violation)
            ));
        }
        let alias = inputs.session_cwd.join("config-hardlink");
        script.push_str(&format!(
            "if ln {config} {alias}; then if printf changed >> {alias}; then touch {violation}; fi; fi\n",
            config=quote(&config), alias=quote(&alias), violation=quote(&violation)
        ));
        for node in git_metadata_mount_nodes(&inputs) {
            script.push_str(&format!(
                "if mv {} {}; then touch {}; fi\n",
                quote(&node),
                quote(&node.with_extension("moved")),
                quote(&violation)
            ));
        }
        // A normal Git commit proves directory-node sealing retains lockfile
        // creation, object writes, and ref updates instead of freezing .git.
        script.push_str(&format!(
            "{git} -C {cwd} -c core.hooksPath=/nonexistent-kranz-fixture-hooks -c commit.gpgSign=false commit --allow-empty -m contained > {receipt} 2>&1 || touch {violation}\ntouch {done}\n[ ! -e {violation} ]\n",
            git=quote(&real_git), cwd=quote(&inputs.session_cwd), receipt=quote(&inputs.tmpdir.join("commit-receipt")),
            done=quote(&done), violation=quote(&violation)
        ));
        #[cfg(target_os = "macos")]
        let mut actor = {
            let profile = write_profile_file(&inputs.tmpdir, &generate_profile(&inputs)).unwrap();
            let mut actor = Command::new("sandbox-exec");
            actor
                .args(["-f"])
                .arg(profile)
                .args(["/bin/sh", "-c", &script]);
            actor
        };
        #[cfg(target_os = "linux")]
        let mut actor = {
            let args =
                bubblewrap_args(&inputs, Path::new("/bin/sh"), &["-c".into(), script]).unwrap();
            let mut actor = Command::new("bwrap");
            actor.args(args);
            actor
        };
        let out = actor.current_dir(&inputs.session_cwd).output().unwrap();
        let engine_out = engine.wait_with_output().unwrap();
        let receipt =
            std::fs::read_to_string(inputs.tmpdir.join("commit-receipt")).unwrap_or_default();
        assert!(
            out.status.success(),
            "layout={layout}: {out:?}; commit={receipt}"
        );
        assert!(
            engine_out.status.success(),
            "layout={layout}: {engine_out:?}"
        );
        assert_eq!(std::fs::read(&config).unwrap(), config_before);
        assert_eq!(std::fs::read(git_dir.join("config.worktree")).unwrap(), b"");
        assert!(ready.exists() && done.exists(), "synchronization must run");
        assert!(!violation.exists());
    }
}

#[cfg(unix)]
#[test]
fn git_config_protection_reads_scope_enablement_from_common_config() {
    let (_dir, inputs) = fixture();
    let root = &inputs.session_cwd;
    git(root, &["config", "extensions.worktreeConfig", "true"]);
    let target = root.join("mutable-worktree-config");
    // This effective-value override must not hide the fact Git already read
    // this worktree scope because the common config enabled it.
    std::fs::write(&target, "[extensions]\nworktreeConfig = false\n").unwrap();
    std::os::unix::fs::symlink(&target, root.join(".git/config.worktree")).unwrap();
    assert!(validate_git_config_protection(&inputs, true)
        .unwrap_err()
        .to_string()
        .contains("symlink"));
}

#[cfg(unix)]
#[test]
fn git_config_protection_mounts_preserve_index_writes_and_seal_config_nodes() {
    let (_dir, inputs) = fixture();
    let root = &inputs.session_cwd;
    let git_dir = root.join(".git");
    let config = git_dir.join("config");
    let index = git_dir.join("index.lock");
    let args = bubblewrap_args(&inputs, Path::new("/bin/true"), &[]).unwrap();
    for (path, expected) in [(&config, "--ro-bind"), (&index, "--bind")] {
        let last = args
            .windows(3)
            .rev()
            .find(|part| {
                matches!(part[0].as_str(), "--bind" | "--ro-bind") && path.starts_with(&part[2])
            })
            .unwrap();
        assert_eq!(last[0], expected, "{}: {args:?}", path.display());
    }
    assert!(args
        .windows(3)
        .any(|part| part[0] == "--bind" && part[2] == git_dir.to_string_lossy()));
    let spec = crate::sandbox_container::ContainerSpec {
        runtime: crate::sandbox_container::ContainerRuntime::Docker,
        image: "fixture-unused".into(),
        network: None,
        name: None,
    };
    let args = crate::sandbox_container::container_run_args(
        &inputs,
        &spec,
        Path::new("/bin/true"),
        &[],
        None,
    );
    let mounts: Vec<_> = args
        .windows(2)
        .filter(|part| part[0] == "-v")
        .map(|part| &part[1])
        .collect();
    let git_path = git_dir.to_string_lossy();
    assert!(
        mounts
            .iter()
            .any(|value| value.starts_with(&format!("{git_path}:{git_path}"))
                && !value.ends_with(":ro")),
        "{mounts:?}"
    );
    assert!(
        !mounts
            .iter()
            .any(|value| value.as_str() == format!("{git_path}:{git_path}:ro")),
        "index/ref writes must remain available: {mounts:?}"
    );
    let config_path = config.to_string_lossy();
    assert!(
        mounts
            .iter()
            .any(|value| value.as_str() == format!("{config_path}:{config_path}:ro")),
        "{mounts:?}"
    );
}

#[test]
fn git_config_protection_refuses_metadata_that_would_rebind_authority() {
    let (_dir, inputs) = fixture();
    let relocated = inputs.session_cwd.join(".kranz/git-metadata");
    std::fs::create_dir_all(relocated.parent().unwrap()).unwrap();
    std::fs::rename(inputs.session_cwd.join(".git"), &relocated).unwrap();
    let pointer = relocated.to_string_lossy();
    let pointer = pointer.strip_prefix(r"\\?\").unwrap_or(&pointer);
    std::fs::write(
        inputs.session_cwd.join(".git"),
        format!("gitdir: {pointer}\n"),
    )
    .unwrap();
    let error = validate_git_config_protection(&inputs, true).unwrap_err();
    assert!(error.to_string().contains("authority directory"), "{error}");
}

#[test]
fn git_config_protection_seals_ancestor_gitlink_for_subdirectory_sessions() {
    let (_dir, mut inputs) = fixture();
    let main = inputs.session_cwd.clone();
    let worktree = main.join("linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    inputs.session_cwd = worktree.join("subdir");
    std::fs::create_dir(&inputs.session_cwd).unwrap();
    inputs.extra_write.push(main);
    validate_git_config_protection(&inputs, true).unwrap();
    let marker = worktree.join(".git");
    assert!(git_metadata_write_denies(&inputs)
        .files
        .iter()
        .any(|path| absolutize(path) == absolutize(&marker)));
    assert!(git_metadata_mount_nodes(&inputs)
        .iter()
        .any(|path| absolutize(path) == absolutize(&worktree)));
}

#[test]
fn git_config_protection_refuses_grants_covering_neutral_global_config() {
    let (_dir, mut inputs) = fixture();
    let neutral = crate::git_ops::empty_global_config_path().unwrap();
    let before = std::fs::read(neutral).unwrap();
    let private_dir = neutral.parent().unwrap();
    for grant in [
        private_dir.to_path_buf(),
        private_dir.parent().unwrap().to_path_buf(),
    ] {
        inputs.extra_write = vec![grant];
        let error = validate_git_config_protection(&inputs, true).unwrap_err();
        assert!(
            error.to_string().contains("neutral Git configuration"),
            "{error}"
        );
    }
    // Ordinary sibling scratch and explicit temp grants remain usable. The
    // policy checks overlap with this process's actual neutral file, rather
    // than banning temp-directory grants as a category.
    let sibling = tempfile::tempdir().unwrap();
    inputs.extra_write = vec![sibling.path().to_path_buf()];
    validate_git_config_protection(&inputs, true).unwrap();
    assert_eq!(std::fs::read(neutral).unwrap(), before);
}

#[test]
fn git_config_protection_checks_neutral_config_even_for_non_git_sessions() {
    let session = tempfile::tempdir().unwrap();
    let neutral = crate::git_ops::empty_global_config_path().unwrap();
    let inputs = SandboxInputs {
        enforce: crate::types::SandboxEnforce::Fs,
        session_cwd: session.path().to_path_buf(),
        mission_dir: session.path().join(".kranz/missions/m-fixture"),
        tmpdir: session.path().join("scratch"),
        extra_write: vec![neutral.parent().unwrap().to_path_buf()],
        egress: Vec::new(),
        validator_read_deny_roots: Vec::new(),
    };
    assert!(validate_git_config_protection(&inputs, false)
        .unwrap_err()
        .to_string()
        .contains("neutral Git configuration"));
}
