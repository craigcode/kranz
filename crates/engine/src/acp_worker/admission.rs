//! V2's measured host envelope. Check before credential I/O and freeze the
//! local daemon selection for every subsequent worker control operation.
use super::refusal;
use crate::error::Result;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::gate_evaluation::subprocess::DockerEvaluator;
use crate::sandbox::SandboxInputs;
use serde::Serialize;
use serde_json::Value;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
pub(crate) enum Expected {
    Text(&'static str),
    Count(u64),
    Flag(bool),
    List(&'static [&'static str]),
}

#[derive(Clone, Copy, Serialize)]
pub(crate) struct Policy {
    host: &'static [(&'static str, Expected)],
    endpoint_policy: &'static str,
    supervisor_sha256: &'static str,
    pub evidence: &'static [(&'static str, &'static str)],
}

use Expected::{Count, Flag, List, Text};
pub(crate) const POLICY: Policy = Policy {
    host: &[
        ("/macosVersion", Text("26.6.2")),
        ("/macosBuild", Text("25G83")),
        ("/colimaVersion", Text("colima version 0.10.3")),
        ("/colima/display_name", Text("colima")),
        ("/colima/driver", Text("macOS Virtualization.Framework")),
        ("/colima/arch", Text("aarch64")),
        ("/colima/runtime", Text("docker")),
        ("/colima/mount_type", Text("virtiofs")),
        ("/colima/kubernetes", Flag(false)),
        ("/colima/cpu", Count(4)),
        ("/colima/memory", Count(6_442_450_944)),
        ("/docker/ClientInfo/Version", Text("29.7.2")),
        ("/docker/ClientInfo/Os", Text("darwin")),
        ("/docker/ClientInfo/Arch", Text("arm64")),
        ("/docker/ServerVersion", Text("29.2.1")),
        ("/docker/KernelVersion", Text("6.8.0-100-generic")),
        ("/docker/OperatingSystem", Text("Ubuntu 24.04.4 LTS")),
        ("/docker/OSType", Text("linux")),
        ("/docker/Architecture", Text("aarch64")),
        ("/docker/CgroupVersion", Text("2")),
        ("/docker/CgroupDriver", Text("cgroupfs")),
        ("/docker/DefaultRuntime", Text("runc")),
        ("/docker/RuncCommit/ID", Text("v1.3.4-0-gd6d73eb8")),
        ("/docker/NCPU", Count(4)),
        ("/docker/MemTotal", Count(6_198_104_064)),
        ("/docker/Driver", Text("overlayfs")),
        ("/docker/Name", Text("colima")),
        ("/docker/MemoryLimit", Flag(true)),
        ("/docker/SwapLimit", Flag(true)),
        ("/docker/CpuCfsPeriod", Flag(true)),
        ("/docker/CpuCfsQuota", Flag(true)),
        ("/docker/PidsLimit", Flag(true)),
        (
            "/docker/SecurityOptions",
            List(&[
                "name=apparmor",
                "name=seccomp,profile=builtin",
                "name=cgroupns",
            ]),
        ),
    ],
    endpoint_policy: "current-colima-default-owned-unix-socket",
    supervisor_sha256: "07d91f102a51fde11717939f51aaa25b5d8977b23762d08feb8a3365be161abe",
    evidence: &[
        (
            "docs/reviews/evidence/2026-10-10-acp-resource-codex.json",
            "34737668eb6685e3f90e50ad778d33bcb3bfde49b89a7a115482bbb91c4a83ad",
        ),
        (
            "docs/reviews/evidence/2026-10-10-acp-resource-claude.json",
            "8be38783b876f800f87fb9ba655453976834746d4268da2460a9a0a81835deed",
        ),
        (
            "docs/reviews/evidence/2026-10-10-acp-selected-limits.json",
            "c0e04e7acd22a960723f4c720941bf5fdc794d933f1b20338b0b60480e219cac",
        ),
    ],
};

#[derive(Clone)]
pub(crate) struct AdmittedHost {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub docker: PathBuf,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub client: DockerEvaluator,
    pub receipt: Value,
}

#[cfg(any(target_os = "macos", test))]
fn validate_host(observed: &Value, policy: Policy) -> Result<Value> {
    let mut receipt = serde_json::Map::new();
    for (pointer, expected) in policy.host {
        let expected = serde_json::to_value(expected).expect("host policy serializes");
        if observed.pointer(pointer) != Some(&expected) {
            return Err(refusal(&format!(
                "v2 host differs at {pointer}; see docs/acp-containment.md"
            )));
        }
        // Copy only the compared fields, never arbitrary CLI output or paths.
        receipt.insert((*pointer).into(), expected);
    }
    Ok(serde_json::json!({
        "policy": policy.endpoint_policy,
        "checked": receipt,
        "endpointPinned": true,
    }))
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn validate_env_map(env: &std::collections::HashMap<String, String>) -> Result<()> {
    for key in [
        "DOCKER_HOST",
        "DOCKER_CONFIG",
        "DOCKER_TLS",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "DOCKER_API_VERSION",
    ] {
        if env.contains_key(key) {
            return Err(refusal(&format!(
                "v2 requires the default Colima context without {key}"
            )));
        }
    }
    if let Some(tls_key) = env.keys().find(|key| key.starts_with("DOCKER_TLS")) {
        return Err(refusal(&format!(
            "v2 requires the default Colima context without {tls_key}"
        )));
    }
    if env.get("DOCKER_CONTEXT").is_some_and(|v| v != "colima") {
        return Err(refusal(
            "v2 requires the current default Colima local Unix endpoint",
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) fn validate_ambient_docker_env() -> Result<()> {
    for (raw_key, raw_val) in std::env::vars_os() {
        let key = raw_key.to_string_lossy();
        if matches!(
            key.as_ref(),
            "DOCKER_HOST" | "DOCKER_CONFIG" | "DOCKER_CERT_PATH" | "DOCKER_API_VERSION"
        ) || key.starts_with("DOCKER_TLS")
        {
            return Err(refusal(&format!(
                "v2 requires the default Colima context without {key}"
            )));
        }
        if key == "DOCKER_CONTEXT" && raw_val.to_str() != Some("colima") {
            return Err(refusal(
                "v2 requires the current default Colima local Unix endpoint",
            ));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn validate_selection(
    env: &std::collections::HashMap<String, String>,
    context: &Value,
    endpoint: &str,
) -> Result<()> {
    validate_env_map(env)?;
    if context.as_array().map(Vec::len) != Some(1)
        || context.pointer("/0/Name").and_then(Value::as_str) != Some("colima")
        || context
            .pointer("/0/Endpoints/docker/Host")
            .and_then(Value::as_str)
            != Some(endpoint)
        || context
            .pointer("/0/Endpoints/docker/SkipTLSVerify")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(refusal(
            "v2 requires the current default Colima local Unix endpoint",
        ));
    }
    Ok(())
}

#[cfg(all(unix, any(target_os = "macos", test)))]
fn verify_colima_socket(
    home: &std::path::Path,
    inputs: &SandboxInputs,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let euid = unsafe { libc::geteuid() };
    let colima_dir = home.join(".colima");
    let default_dir = colima_dir.join("default");
    for parent in [&colima_dir, &default_dir] {
        let meta = std::fs::symlink_metadata(parent)
            .map_err(|_| refusal("v2 default Colima socket parent is unavailable"))?;
        if meta.file_type().is_symlink()
            || !meta.file_type().is_dir()
            || meta.uid() != euid
            || (meta.mode() & 0o022) != 0
        {
            return Err(refusal(
                "v2 requires operator-owned non-symlink .colima directories without group/world write",
            ));
        }
    }
    let socket = default_dir.join("docker.sock");
    let before = std::fs::symlink_metadata(&socket)
        .map_err(|_| refusal("v2 default Colima socket is unavailable"))?;
    if before.file_type().is_symlink()
        || !before.file_type().is_socket()
        || before.uid() != euid
        || (before.mode() & 0o077) != 0
    {
        return Err(refusal(
            "v2 requires an operator-owned socket outside worker-writable roots",
        ));
    }
    let socket_path = socket
        .canonicalize()
        .map_err(|_| refusal("v2 default Colima socket is unavailable"))?;
    let after = std::fs::symlink_metadata(&socket_path)
        .map_err(|_| refusal("v2 default Colima socket is unavailable"))?;
    if after.file_type().is_symlink()
        || !after.file_type().is_socket()
        || after.uid() != euid
        || (after.mode() & 0o077) != 0
        || (before.dev(), before.ino()) != (after.dev(), after.ino())
    {
        return Err(refusal(
            "v2 requires an operator-owned socket outside worker-writable roots",
        ));
    }
    let primary = inputs
        .mission_dir
        .ancestors()
        .find(|ancestor| ancestor.file_name().and_then(|n| n.to_str()) == Some(".kranz"))
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    let socket_abs = crate::sandbox::absolutize(&socket);
    if std::iter::once(&inputs.session_cwd)
        .chain(std::iter::once(&inputs.tmpdir))
        .chain(&inputs.extra_write)
        .chain(primary.iter())
        .any(|root| {
            let root_abs = crate::sandbox::absolutize(root);
            socket_path.starts_with(&root_abs) || socket_abs.starts_with(&root_abs)
        })
    {
        return Err(refusal(
            "v2 requires an operator-owned socket outside worker-writable roots",
        ));
    }
    Ok((socket, socket_path))
}

#[cfg(not(target_os = "macos"))]
pub(crate) async fn probe(_inputs: &SandboxInputs, _policy: Policy) -> Result<AdmittedHost> {
    validate_ambient_docker_env()?;
    Err(refusal(
        "v2 host admission requires the qualified macOS ARM64 Colima host",
    ))
}

#[cfg(target_os = "macos")]
pub(crate) async fn probe(inputs: &SandboxInputs, policy: Policy) -> Result<AdmittedHost> {
    use crate::command_exec::evaluator_io;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    async fn output(
        program: &Path,
        args: &[&str],
        env: &HashMap<String, String>,
    ) -> Result<Vec<u8>> {
        let args: Vec<_> = args.iter().map(|s| (*s).into()).collect();
        let output = evaluator_io::run(
            program,
            &args,
            env,
            b"",
            Duration::from_secs(5),
            Duration::from_secs(1),
            65_536,
            65_536,
            &AtomicBool::new(false),
        )
        .await
        .map_err(|_| refusal("v2 host probe failed or exceeded its bound"))?;
        if output.code != Some(0) {
            return Err(refusal("v2 host probe returned an unsuccessful status"));
        }
        Ok(output.stdout)
    }
    fn parse(bytes: &[u8]) -> Result<Value> {
        crate::strict_json::parse(bytes)
            .map_err(|_| refusal("v2 host probe returned invalid unique-key JSON"))
    }
    fn line(bytes: &[u8]) -> Result<String> {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| s.lines().next())
            .map(|s| s.trim().to_owned())
            .ok_or_else(|| refusal("v2 host probe returned invalid text"))
    }

    // Refuse banned Docker selection environment variables BEFORE locating or
    // spawning `docker context inspect`.
    validate_ambient_docker_env()?;
    let docker = crate::acp_container::trusted_control_program(inputs, "docker")?;
    let colima = crate::acp_container::trusted_control_program(inputs, "colima")?;
    let mut client =
        DockerEvaluator::new(&docker).map_err(|_| refusal("v2 Docker client is unavailable"))?;
    let env = client.environment();
    validate_env_map(env)?;
    let home = env
        .get("HOME")
        .map(Path::new)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| refusal("v2 requires an absolute operator HOME"))?;
    let (socket, socket_path) = verify_colima_socket(home, inputs)?;
    let endpoint = format!("unix://{}", socket.display());
    // Context inspection is local configuration only. No daemon request is
    // made until the endpoint has been checked and pinned.
    let context = client
        .control(&["context".into(), "inspect".into()])
        .await
        .map_err(|_| refusal("v2 Docker context probe failed"))?;
    if context.code != Some(0) {
        return Err(refusal("v2 Docker context probe failed"));
    }
    validate_selection(env, &parse(&context.stdout)?, &endpoint)?;
    let control_env: HashMap<_, _> = [
        "HOME", "PATH", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE",
    ]
    .into_iter()
    .filter_map(|key| env.get(key).map(|v| (key.to_owned(), v.clone())))
    .collect();
    client.pin_local_endpoint(&format!("unix://{}", socket_path.display()));
    let version = output(&colima, &["version"], &control_env).await?;
    let colima_status = parse(&output(&colima, &["status", "--json"], &control_env).await?)?;
    if colima_status.get("docker_socket").and_then(Value::as_str) != Some(&endpoint) {
        return Err(refusal("v2 Colima status and Docker endpoint differ"));
    }
    let docker_info = client
        .control(&["info".into(), "--format".into(), "{{json .}}".into()])
        .await
        .map_err(|_| refusal("v2 Docker host probe failed"))?;
    if docker_info.code != Some(0) {
        return Err(refusal("v2 Docker host probe failed"));
    }
    let observed = serde_json::json!({
        "macosVersion": line(&output(Path::new("/usr/bin/sw_vers"), &["-productVersion"], &control_env).await?)?,
        "macosBuild": line(&output(Path::new("/usr/bin/sw_vers"), &["-buildVersion"], &control_env).await?)?,
        "colimaVersion": line(&version)?,
        "colima": colima_status,
        "docker": parse(&docker_info.stdout)?,
    });
    Ok(AdmittedHost {
        docker,
        client,
        receipt: validate_host(&observed, policy)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn acp_v2_admission_refuses_each_missing_mistyped_or_drifted_host_field() {
        let observed: Value =
            serde_json::from_str(include_str!("tests/qualified-host.json")).unwrap();
        let receipt = validate_host(&observed, POLICY).unwrap();
        assert_eq!(receipt["endpointPinned"], true);
        assert_eq!(
            receipt["checked"].as_object().unwrap().len(),
            POLICY.host.len()
        );
        for (pointer, _) in POLICY.host {
            for wrong in [Value::Null, json!({"untrusted":true}), json!("drift")] {
                let mut changed = observed.clone();
                *changed.pointer_mut(pointer).unwrap() = wrong;
                let error = validate_host(&changed, POLICY).unwrap_err().to_string();
                assert!(error.contains(pointer), "{error}");
                assert!(!error.contains("untrusted"));
            }
            let mut missing = observed.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            missing
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(validate_host(&missing, POLICY).is_err(), "{pointer}");
        }
    }

    #[test]
    fn acp_v2_admission_refuses_remote_ambiguous_or_overridden_docker_selection() {
        let endpoint = "unix:///operator/.colima/default/docker.sock";
        let good = json!([{"Name":"colima", "Endpoints":{"docker":{"Host":endpoint,"SkipTLSVerify":false}}}]);
        let empty = std::collections::HashMap::new();
        validate_selection(&empty, &good, endpoint).unwrap();
        validate_selection(
            &std::collections::HashMap::from([("DOCKER_CONTEXT".into(), "colima".into())]),
            &good,
            endpoint,
        )
        .unwrap();
        for key in [
            "DOCKER_HOST",
            "DOCKER_CONFIG",
            "DOCKER_TLS",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
            "DOCKER_API_VERSION",
            "DOCKER_CONTEXT",
        ] {
            for value in ["", "other"] {
                assert!(
                    validate_selection(
                        &std::collections::HashMap::from([(key.into(), value.into())]),
                        &good,
                        endpoint
                    )
                    .is_err(),
                    "{key}"
                );
            }
        }
        for bad in [
            Value::Null,
            json!([]),
            json!([good[0], good[0]]),
            json!([{}]),
        ] {
            assert!(validate_selection(&empty, &bad, endpoint).is_err());
        }
        for (pointer, value) in [
            ("/0/Name", json!("remote")),
            ("/0/Endpoints/docker/Host", json!("ssh://remote")),
            ("/0/Endpoints/docker/Host", json!("unix:///other.sock")),
            ("/0/Endpoints/docker/SkipTLSVerify", json!(true)),
        ] {
            let mut bad = good.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(validate_selection(&empty, &bad, endpoint).is_err());
        }
    }

    #[test]
    fn acp_v2_admission_policy_binds_reviewed_evidence_and_supervisor_bytes() {
        let files: &[&[u8]] = &[
            include_bytes!("../../../../docs/reviews/evidence/2026-10-10-acp-resource-codex.json"),
            include_bytes!("../../../../docs/reviews/evidence/2026-10-10-acp-resource-claude.json"),
            include_bytes!("../../../../docs/reviews/evidence/2026-10-10-acp-selected-limits.json"),
        ];
        assert_eq!(files.len(), POLICY.evidence.len());
        for (bytes, (_, expected)) in files.iter().zip(POLICY.evidence) {
            assert_eq!(crate::standards_waiver::sha256_hex(bytes), *expected);
        }
        assert_eq!(
            crate::standards_waiver::sha256_hex(include_bytes!(
                "../acp_container/supervisor_resources.py"
            )),
            POLICY.supervisor_sha256
        );
    }

    #[test]
    fn acp_v2_admission_validate_env_map_refuses_arbitrary_docker_tls_prefix() {
        assert!(validate_env_map(&std::collections::HashMap::from([(
            "DOCKER_TLS_CA".into(),
            "1".into()
        )]))
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn acp_v2_admission_verifies_colima_parent_dirs_and_socket_permissions() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        if std::env::var_os("KRANZ_TEST_BIND_RELATIVE_SOCK").is_some() {
            let _ = UnixListener::bind("docker.sock").unwrap();
            return;
        }

        let base = tempfile::Builder::new().prefix("k").tempdir().unwrap();
        let home = base.path().join("h");
        let colima = home.join(".colima");
        let default = colima.join("default");
        std::fs::create_dir_all(&default).unwrap();
        std::fs::set_permissions(&colima, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o700)).unwrap();

        let sock_path = default.join("docker.sock");
        let _listener = match UnixListener::bind(&sock_path) {
            Ok(listener) => Some(listener),
            Err(_) => {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .current_dir(&default)
                    .env("KRANZ_TEST_BIND_RELATIVE_SOCK", "1")
                    .args([
                        "--exact",
                        "acp_worker::admission::tests::acp_v2_admission_verifies_colima_parent_dirs_and_socket_permissions",
                        "--nocapture",
                    ])
                    .status()
                    .unwrap();
                assert!(
                    status.success(),
                    "relative UnixListener::bind helper failed"
                );
                None
            }
        };
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let work = base.path().join("w");
        let mission = base.path().join("p/.kranz/missions/m-1");
        let tmp = base.path().join("t");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&mission).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();

        let inputs = SandboxInputs {
            enforce: crate::types::SandboxEnforce::FsNet,
            session_cwd: work.clone(),
            mission_dir: mission.clone(),
            tmpdir: tmp.clone(),
            extra_write: Vec::new(),
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        };

        let (_, canon) = verify_colima_socket(&home, &inputs).unwrap();
        assert_eq!(canon, sock_path.canonicalize().unwrap());

        // Group-writable `.colima` or `.colima/default` must be refused.
        std::fs::set_permissions(&colima, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(verify_colima_socket(&home, &inputs).is_err());
        std::fs::set_permissions(&colima, std::fs::Permissions::from_mode(0o700)).unwrap();

        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(verify_colima_socket(&home, &inputs).is_err());
        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o700)).unwrap();

        // Socket inside `extra_write` or `primary` must be refused.
        let mut inside_extra = inputs.clone();
        inside_extra.extra_write = vec![home.clone()];
        assert!(verify_colima_socket(&home, &inside_extra).is_err());

        // Symlinked `.colima` directory must be refused.
        let sym_home = base.path().join("sh");
        std::fs::create_dir_all(&sym_home).unwrap();
        std::os::unix::fs::symlink(&colima, sym_home.join(".colima")).unwrap();
        assert!(verify_colima_socket(&sym_home, &inputs).is_err());
    }
}
