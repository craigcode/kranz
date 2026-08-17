//! Stable Windows AppContainer launcher used by production session and gate paths.
//!
//! The engine starts a trusted copy of itself as a thin launcher. That helper
//! creates the prompt-injectable child suspended, assigns it to a kill-on-close
//! Job Object, applies `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, and only
//! then resumes it. The helper inherits the engine's already-cleared environment
//! and stdio pipes, so the existing async stream bounds and timeout machinery do
//! not need a second Windows-only implementation.
//!
//! Filesystem authority is granted to a unique per-launch AppContainer SID. The
//! parent snapshots every DACL it changes and restores those descriptors when the
//! wrapped process is reaped or aborted; restoring an inheritable parent ACE also
//! removes its inherited copies from descendants. The profile name is random and
//! deleted with the same lease. A process crash can leave an inert orphan SID ACE,
//! but no other AppContainer principal can use it and normal/abort paths restore
//! the original descriptors exactly.

use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, LocalFree, DUPLICATE_SAME_ACCESS, HANDLE, HLOCAL, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, DENY_ACCESS, EXPLICIT_ACCESS_W,
    GRANT_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    CreateWellKnownSid, FreeSid, GetTokenInformation, TokenIsAppContainer,
    WinCapabilityInternetClientSid, ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
    NO_INHERITANCE, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
    SID_AND_ATTRIBUTES, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{
    DELETE, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::SystemServices::SE_GROUP_ENABLED;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, ResumeThread, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

pub(crate) const INTERNAL_LAUNCHER_ARG: &str = "__kranz-appcontainer-launch";
pub(crate) const INTERNAL_SELF_TEST_ARG: &str = "__kranz-appcontainer-self-test";
pub(crate) const INTERNAL_HOSTILE_CHILD_ARG: &str = "__kranz-appcontainer-hostile-child";
const PLAN_VERSION: u32 = 1;
const SELF_TEST_MANIFEST: &str = "kranz-appcontainer-production-self-test.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchPlan {
    version: u32,
    profile_name: String,
    executable: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    allow_network: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelfTestManifest {
    executable: PathBuf,
    toolchain_denied_write: PathBuf,
    worktree_write: PathBuf,
    scratch_write: PathBuf,
    outside_write: PathBuf,
    authority_file: PathBuf,
    real_source: PathBuf,
    shared_git_marker: PathBuf,
    loopback_addr: SocketAddr,
    receipt: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionHostileReceipt {
    pub token_is_appcontainer: bool,
    pub toolchain_read: bool,
    pub toolchain_write_denied: bool,
    pub worktree_write: bool,
    pub scratch_write: bool,
    pub outside_write_denied: bool,
    pub authority_read_denied: bool,
    pub real_checkout_read_denied: bool,
    pub shared_git_read: bool,
    pub network_denied: bool,
    pub dacl_restored: bool,
}

/// Prepared wrapper argv plus the lease that keeps its profile and ACL grants
/// alive. Callers must store `lease` for at least as long as the wrapper child.
pub(crate) struct PreparedLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub lease: AppContainerLease,
}

#[derive(Debug)]
struct DaclSnapshot {
    path: PathBuf,
    acl: Option<Vec<u8>>,
}

/// Parent-owned cleanup guard. It deliberately carries no SID pointer, so it
/// is safe to move with an async session across executor threads.
pub(crate) struct AppContainerLease {
    profile_name: String,
    original_dacls: Vec<DaclSnapshot>,
    plan_path: Option<PathBuf>,
}

impl Drop for AppContainerLease {
    fn drop(&mut self) {
        // Parent directories first: removing their inheritable AppContainer
        // ACEs makes Windows retract inherited copies from existing children;
        // the later child snapshots then restore any direct descriptor exactly.
        self.original_dacls
            .sort_by_key(|entry| entry.path.components().count());
        for snapshot in &self.original_dacls {
            if let Err(error) = restore_dacl(snapshot) {
                tracing::error!(path = %snapshot.path.display(), error = %error,
                    "failed to restore an AppContainer-modified DACL");
            }
        }
        if let Some(path) = self.plan_path.take() {
            let _ = std::fs::remove_file(path);
        }
        let name = wide(&self.profile_name);
        // Deletion is idempotent for our cleanup purposes: the helper may have
        // exited normally, while an earlier preparation error may never have
        // made the profile visible to a child.
        let _ = unsafe { DeleteAppContainerProfile(PCWSTR(name.as_ptr())) };
    }
}

struct OwnedSid(PSID);

impl Drop for OwnedSid {
    fn drop(&mut self) {
        unsafe {
            FreeSid(self.0);
        }
    }
}

struct LocalAllocation(HLOCAL);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(self.0));
        }
    }
}

#[derive(Clone, Copy)]
enum AclMode {
    Grant,
    Deny,
}

struct AclChange {
    path: PathBuf,
    permissions: u32,
    inherit: bool,
    mode: AclMode,
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn win32(status: windows::Win32::Foundation::WIN32_ERROR) -> Result<()> {
    status
        .ok()
        .map_err(|error| EngineError::Backend(format!("Windows ACL operation failed: {error}")))
}

fn create_profile() -> Result<(String, OwnedSid)> {
    let profile_name = format!("kranz.production.{}", uuid::Uuid::new_v4().simple());
    let name = wide(&profile_name);
    let display = wide("Kranz contained process");
    let description = wide("Disposable Kranz AppContainer profile");
    let sid = unsafe {
        CreateAppContainerProfile(
            PCWSTR(name.as_ptr()),
            PCWSTR(display.as_ptr()),
            PCWSTR(description.as_ptr()),
            None,
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!("failed to create AppContainer profile: {error}"))
    })?;
    Ok((profile_name, OwnedSid(sid)))
}

fn snapshot_dacl(path: &Path) -> Result<DaclSnapshot> {
    let path_wide = wide(path.as_os_str());
    let mut acl: *mut ACL = null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    win32(unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut acl),
            None,
            &mut descriptor,
        )
    })?;
    let _descriptor = LocalAllocation(HLOCAL(descriptor.0));
    let acl = if acl.is_null() {
        None
    } else {
        let len = unsafe { (*acl).AclSize as usize };
        Some(unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), len) }.to_vec())
    };
    Ok(DaclSnapshot {
        path: path.to_path_buf(),
        acl,
    })
}

fn restore_dacl(snapshot: &DaclSnapshot) -> Result<()> {
    if !snapshot.path.exists() {
        return Ok(());
    }
    let path = wide(snapshot.path.as_os_str());
    let acl = snapshot
        .acl
        .as_ref()
        .map(|bytes| bytes.as_ptr() as *const ACL);
    win32(unsafe {
        SetNamedSecurityInfoW(
            PWSTR(path.as_ptr().cast_mut()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            acl,
            None,
        )
    })
}

fn apply_acl_change(change: &AclChange, sid: PSID) -> Result<()> {
    let path = wide(change.path.as_os_str());
    let mut old_acl: *mut ACL = null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    win32(unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut old_acl),
            None,
            &mut descriptor,
        )
    })?;
    let _descriptor = LocalAllocation(HLOCAL(descriptor.0));
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: change.permissions,
        grfAccessMode: match change.mode {
            AclMode::Grant => GRANT_ACCESS,
            AclMode::Deny => DENY_ACCESS,
        },
        grfInheritance: if change.inherit {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: PWSTR(sid.0.cast()),
            ..Default::default()
        },
    };
    let mut new_acl: *mut ACL = null_mut();
    win32(unsafe { SetEntriesInAclW(Some(&[entry]), Some(old_acl), &mut new_acl) })?;
    let _new_acl = LocalAllocation(HLOCAL(new_acl.cast()));
    win32(unsafe {
        SetNamedSecurityInfoW(
            PWSTR(path.as_ptr().cast_mut()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl),
            None,
        )
    })
}

fn env_value_ci<'a>(env: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    env.iter()
        .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.as_str()))
}

fn resolve_executable(program: &Path, env: &HashMap<String, String>) -> Result<PathBuf> {
    let has_path = program.components().count() > 1;
    let mut candidates = Vec::new();
    let extensions: Vec<String> = if program.extension().is_some() {
        vec![String::new()]
    } else {
        env_value_ci(env, "PATHEXT")
            .unwrap_or(".COM;.EXE;.BAT;.CMD")
            .split(';')
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    };
    let bases: Vec<PathBuf> = if has_path {
        vec![program.to_path_buf()]
    } else {
        env_value_ci(env, "PATH")
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .map(|dir| dir.join(program))
            .collect()
    };
    for base in bases {
        for extension in &extensions {
            let candidate = if extension.is_empty() {
                base.clone()
            } else {
                let mut value = base.as_os_str().to_os_string();
                value.push(extension);
                PathBuf::from(value)
            };
            if candidate.is_file() {
                candidates.push(candidate);
            }
        }
    }
    let candidate = candidates.into_iter().next().ok_or_else(|| {
        EngineError::Backend(format!(
            "AppContainer child executable {:?} could not be resolved from the cleared PATH",
            program
        ))
    })?;
    let extension = candidate
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
        return Err(EngineError::Backend(format!(
            "AppContainer enforcement refuses batch shim {}: passing model prompts through cmd.exe would create a command-injection surface; configure a native .exe backend binary",
            candidate.display()
        )));
    }
    std::fs::canonicalize(&candidate).map_err(|error| {
        EngineError::Backend(format!(
            "failed to canonicalize AppContainer executable {}: {error}",
            candidate.display()
        ))
    })
}

fn git_read_roots(cwd: &Path) -> Vec<PathBuf> {
    let dot_git = cwd.join(".git");
    if dot_git.is_dir() {
        return vec![dot_git];
    }
    let Ok(pointer) = std::fs::read_to_string(&dot_git) else {
        return Vec::new();
    };
    let Some(raw) = pointer
        .lines()
        .find_map(|line| line.strip_prefix("gitdir: "))
    else {
        return Vec::new();
    };
    let git_dir = {
        let path = PathBuf::from(raw.trim());
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    let git_dir = std::fs::canonicalize(&git_dir).unwrap_or(git_dir);
    let mut roots = vec![git_dir.clone()];
    if let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) {
        let common = PathBuf::from(common.trim());
        let common = if common.is_absolute() {
            common
        } else {
            git_dir.join(common)
        };
        roots.push(std::fs::canonicalize(&common).unwrap_or(common));
    }
    roots
}

fn read_roots(executable: &Path, cwd: &Path, env: &HashMap<String, String>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(parent) = executable.parent() {
        if !system_managed_path(parent) {
            roots.push(parent.to_path_buf());
        }
    }
    if let Some(path) = env_value_ci(env, "PATH") {
        roots.extend(
            std::env::split_paths(path)
                .filter(|entry| entry.is_dir() && caller_owned_path(entry, cwd)),
        );
    }
    if let Some(rustup) = env_value_ci(env, "RUSTUP_HOME").map(PathBuf::from) {
        if rustup.is_dir() {
            roots.push(rustup);
        }
    }
    if let Some(cargo) = env_value_ci(env, "CARGO_HOME").map(PathBuf::from) {
        for name in ["bin", "registry", "git"] {
            let root = cargo.join(name);
            if root.exists() {
                roots.push(root);
            }
        }
    }
    roots.extend(git_read_roots(cwd));
    roots.sort();
    roots.dedup();
    roots
}

fn path_under_env(path: &Path, name: &str) -> bool {
    std::env::var_os(name)
        .map(PathBuf::from)
        .is_some_and(|root| path_contains(&root, path))
}

fn system_managed_path(path: &Path) -> bool {
    [
        "SystemRoot",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMW6432",
    ]
    .iter()
    .any(|name| path_under_env(path, name))
}

fn caller_owned_path(path: &Path, cwd: &Path) -> bool {
    path_contains(cwd, path)
        || path_under_env(path, "USERPROFILE")
        || path_under_env(path, "HOME")
        || path_contains(&std::env::temp_dir(), path)
}

fn path_contains(root: &Path, child: &Path) -> bool {
    child == root || child.starts_with(root)
}

fn validate_recursive_roots(
    inputs: &crate::sandbox::SandboxInputs,
    write_roots: &[PathBuf],
    read_roots: &[PathBuf],
) -> Result<()> {
    let authority_paths = crate::sandbox::authority_read_deny_paths(inputs);
    let authority_dirs = crate::sandbox::authority_read_deny_dirs(inputs);
    for root in write_roots.iter().chain(read_roots) {
        for protected in authority_paths.iter().chain(&authority_dirs) {
            if path_contains(root, protected) {
                return Err(EngineError::Backend(format!(
                    "AppContainer recursive grant {} would cover protected authority path {}; refusing before ACL mutation",
                    root.display(),
                    protected.display()
                )));
            }
        }
    }
    let mission = crate::sandbox::absolutize(&inputs.mission_dir);
    for root in write_roots {
        if path_contains(root, &mission) {
            return Err(EngineError::Backend(format!(
                "AppContainer writable root {} covers engine-owned mission metadata {}; Windows enforcement requires a discardable worktree outside that metadata root",
                root.display(),
                mission.display()
            )));
        }
    }
    for cache in crate::sandbox::cargo_cache_write_deny_paths() {
        for root in write_roots {
            if path_contains(root, &cache) {
                return Err(EngineError::Backend(format!(
                    "AppContainer writable root {} covers shared Cargo cache {}; refusing to make the operator cache writable",
                    root.display(),
                    cache.display()
                )));
            }
        }
    }
    Ok(())
}

fn acl_changes(
    inputs: &crate::sandbox::SandboxInputs,
    executable: &Path,
    env: &HashMap<String, String>,
) -> Result<Vec<AclChange>> {
    let write_roots = crate::sandbox::write_allowlist(inputs);
    for root in &write_roots {
        if !root.is_dir() {
            return Err(EngineError::Backend(format!(
                "AppContainer writable root {} must be an existing directory",
                root.display()
            )));
        }
    }
    let read_roots = read_roots(executable, &inputs.session_cwd, env);
    validate_recursive_roots(inputs, &write_roots, &read_roots)?;

    let mut changes = Vec::new();
    let rwx = FILE_GENERIC_READ.0
        | FILE_GENERIC_WRITE.0
        | FILE_GENERIC_EXECUTE.0
        | FILE_DELETE_CHILD.0
        | DELETE.0;
    let rx = FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0;
    for root in &write_roots {
        changes.push(AclChange {
            path: root.clone(),
            permissions: rwx,
            inherit: true,
            mode: AclMode::Grant,
        });
    }
    for root in &read_roots {
        changes.push(AclChange {
            path: root.clone(),
            permissions: rx,
            inherit: root.is_dir(),
            mode: AclMode::Grant,
        });
    }

    let deny_all = FILE_GENERIC_READ.0
        | FILE_GENERIC_WRITE.0
        | FILE_GENERIC_EXECUTE.0
        | FILE_DELETE_CHILD.0
        | DELETE.0;
    let deny_write = FILE_GENERIC_WRITE.0 | FILE_DELETE_CHILD.0 | DELETE.0;
    for path in crate::sandbox::authority_read_deny_paths(inputs) {
        if path.exists() {
            changes.push(AclChange {
                path,
                permissions: deny_all,
                inherit: false,
                mode: AclMode::Deny,
            });
        }
    }
    for path in crate::sandbox::authority_read_deny_dirs(inputs) {
        if path.is_dir() {
            changes.push(AclChange {
                path,
                permissions: deny_all,
                inherit: true,
                mode: AclMode::Deny,
            });
        }
    }
    let mission_denies = crate::sandbox::mission_write_denies(inputs);
    for path in mission_denies.files {
        if path.exists() {
            changes.push(AclChange {
                path,
                permissions: deny_write,
                inherit: false,
                mode: AclMode::Deny,
            });
        }
    }
    for path in mission_denies.control_dirs {
        if path.is_dir() {
            changes.push(AclChange {
                path,
                permissions: deny_write,
                inherit: true,
                mode: AclMode::Deny,
            });
        }
    }
    for runs_dir in mission_denies.runs_dirs {
        let Ok(entries) = std::fs::read_dir(runs_dir) else {
            continue;
        };
        for path in entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && path.extension().is_some_and(|value| value == "jsonl"))
        {
            changes.push(AclChange {
                path,
                permissions: deny_write,
                inherit: false,
                mode: AclMode::Deny,
            });
        }
    }
    for path in crate::sandbox::cargo_cache_write_deny_paths() {
        if path.exists() {
            changes.push(AclChange {
                path,
                permissions: deny_write,
                inherit: true,
                mode: AclMode::Deny,
            });
        }
    }
    for entry in crate::sandbox::validator_read_deny_entries(inputs) {
        if entry.path.exists() {
            changes.push(AclChange {
                path: entry.path,
                permissions: deny_all,
                inherit: entry.is_dir,
                mode: AclMode::Deny,
            });
        }
    }
    Ok(changes)
}

/// Build the trusted helper command and apply the profile's temporary ACLs.
pub(crate) fn prepare_launch(
    inputs: &crate::sandbox::SandboxInputs,
    program: &Path,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<PreparedLaunch> {
    std::fs::create_dir_all(&inputs.tmpdir).map_err(|error| {
        EngineError::Backend(format!(
            "failed to create AppContainer private scratch {}: {error}",
            inputs.tmpdir.display()
        ))
    })?;
    let executable = resolve_executable(program, env)?;
    let (profile_name, sid) = create_profile()?;
    let mut lease = AppContainerLease {
        profile_name: profile_name.clone(),
        original_dacls: Vec::new(),
        plan_path: None,
    };
    let changes = acl_changes(inputs, &executable, env)?;
    let mut seen = BTreeSet::new();
    for change in &changes {
        if seen.insert(change.path.clone()) {
            lease.original_dacls.push(snapshot_dacl(&change.path)?);
        }
    }
    for change in &changes {
        apply_acl_change(change, sid.0)?;
    }

    let plan = LaunchPlan {
        version: PLAN_VERSION,
        profile_name,
        executable,
        args: args.to_vec(),
        cwd: crate::sandbox::absolutize(&inputs.session_cwd),
        // fs keeps ordinary outbound access; fs+net is a hard-offline
        // AppContainer (no network capabilities). A proxy-only environment is
        // never treated as a boundary.
        allow_network: inputs.enforce == crate::types::SandboxEnforce::Fs,
    };
    let plan_path = inputs.tmpdir.join(format!(
        "appcontainer-plan-{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&plan_path)
        .map_err(|error| {
            EngineError::Backend(format!(
                "failed to create AppContainer launch plan {}: {error}",
                plan_path.display()
            ))
        })?;
    serde_json::to_writer(&mut file, &plan).map_err(|error| {
        EngineError::Backend(format!(
            "failed to serialize AppContainer launch plan: {error}"
        ))
    })?;
    file.flush().map_err(|error| {
        EngineError::Backend(format!("failed to flush AppContainer launch plan: {error}"))
    })?;
    lease.plan_path = Some(plan_path.clone());
    let helper = std::env::current_exe().map_err(|error| {
        EngineError::Backend(format!(
            "failed to locate Kranz AppContainer helper: {error}"
        ))
    })?;
    Ok(PreparedLaunch {
        program: helper,
        args: vec![
            INTERNAL_LAUNCHER_ARG.to_string(),
            plan_path.to_string_lossy().into_owned(),
        ],
        lease,
    })
}

struct AttributeList {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}

impl AttributeList {
    fn new(security: &SECURITY_CAPABILITIES, handles: &[HANDLE]) -> Result<Self> {
        let mut bytes = 0usize;
        let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut bytes) };
        if bytes == 0 {
            return Err(EngineError::Backend(
                "AppContainer attribute-list sizing returned zero bytes".to_string(),
            ));
        }
        let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &mut bytes) }.map_err(
            |error| {
                EngineError::Backend(format!(
                    "failed to initialize AppContainer attributes: {error}"
                ))
            },
        )?;
        let result = Self {
            list,
            _storage: storage,
        };
        unsafe {
            UpdateProcThreadAttribute(
                result.list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                Some((security as *const SECURITY_CAPABILITIES).cast()),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                None,
                None,
            )
        }
        .map_err(|error| {
            EngineError::Backend(format!(
                "failed to set AppContainer security attributes: {error}"
            ))
        })?;
        unsafe {
            UpdateProcThreadAttribute(
                result.list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                std::mem::size_of_val(handles),
                None,
                None,
            )
        }
        .map_err(|error| {
            EngineError::Backend(format!(
                "failed to restrict inherited AppContainer handles: {error}"
            ))
        })?;
        Ok(result)
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct ProcessHandles {
    process: HANDLE,
    thread: HANDLE,
}

impl Drop for ProcessHandles {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
        }
    }
}

fn duplicate_standard_handle(
    which: windows::Win32::System::Console::STD_HANDLE,
) -> Result<OwnedHandle> {
    let source = unsafe { GetStdHandle(which) }.map_err(|error| {
        EngineError::Backend(format!("failed to read helper standard handle: {error}"))
    })?;
    let process = unsafe { GetCurrentProcess() };
    let mut duplicate = HANDLE::default();
    unsafe {
        DuplicateHandle(
            process,
            source,
            process,
            &mut duplicate,
            0,
            true,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!(
            "failed to duplicate helper standard handle: {error}"
        ))
    })?;
    Ok(OwnedHandle(duplicate))
}

fn internet_capability() -> Result<Vec<u8>> {
    let mut bytes = 0u32;
    let _ = unsafe { CreateWellKnownSid(WinCapabilityInternetClientSid, None, None, &mut bytes) };
    if bytes == 0 {
        return Err(EngineError::Backend(
            "internetClient capability SID sizing returned zero bytes".to_string(),
        ));
    }
    let mut storage = vec![0u8; bytes as usize];
    unsafe {
        CreateWellKnownSid(
            WinCapabilityInternetClientSid,
            None,
            Some(PSID(storage.as_mut_ptr().cast())),
            &mut bytes,
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!(
            "failed to build internetClient capability SID: {error}"
        ))
    })?;
    Ok(storage)
}

fn quote_arg(value: &OsStr) -> Vec<u16> {
    let source: Vec<u16> = value.encode_wide().collect();
    let mut out = Vec::with_capacity(source.len() + 2);
    out.push(b'"' as u16);
    let mut slashes = 0usize;
    for unit in source {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        if unit == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            out.push(unit);
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            out.push(unit);
        }
        slashes = 0;
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    out
}

fn command_line(executable: &Path, args: &[String]) -> Vec<u16> {
    let mut out = quote_arg(executable.as_os_str());
    for arg in args {
        out.push(b' ' as u16);
        out.extend(quote_arg(OsStr::new(arg)));
    }
    out.push(0);
    out
}

fn environment_block() -> Result<Vec<u16>> {
    let mut values = BTreeMap::<String, (OsString, OsString)>::new();
    for (key, value) in std::env::vars_os() {
        let folded = key.to_string_lossy().to_ascii_uppercase();
        values.insert(folded, (key, value));
    }
    let mut out = Vec::new();
    for (_folded, (key, value)) in values {
        let key: Vec<u16> = key.encode_wide().collect();
        let value: Vec<u16> = value.encode_wide().collect();
        if key.is_empty() || key.contains(&0) || key.contains(&(b'=' as u16)) || value.contains(&0)
        {
            return Err(EngineError::Backend(
                "cleared AppContainer environment contains an invalid key/value".to_string(),
            ));
        }
        out.extend(key);
        out.push(b'=' as u16);
        out.extend(value);
        out.push(0);
    }
    if out.is_empty() {
        out.push(0);
    }
    out.push(0);
    Ok(out)
}

fn derive_profile_sid(name: &str) -> Result<OwnedSid> {
    let name = wide(name);
    let sid = unsafe { DeriveAppContainerSidFromAppContainerName(PCWSTR(name.as_ptr())) }.map_err(
        |error| EngineError::Backend(format!("failed to derive AppContainer SID: {error}")),
    )?;
    Ok(OwnedSid(sid))
}

fn run_plan(plan: LaunchPlan) -> Result<u32> {
    if plan.version != PLAN_VERSION {
        return Err(EngineError::Backend(format!(
            "unsupported AppContainer plan version {}",
            plan.version
        )));
    }
    if !plan.executable.is_absolute() || !plan.executable.is_file() || !plan.cwd.is_dir() {
        return Err(EngineError::Backend(
            "AppContainer plan paths were not absolute existing executable/cwd paths".to_string(),
        ));
    }
    let sid = derive_profile_sid(&plan.profile_name)?;
    let mut internet_sid = plan.allow_network.then(internet_capability).transpose()?;
    let mut capabilities = internet_sid
        .as_mut()
        .map(|storage| {
            vec![SID_AND_ATTRIBUTES {
                Sid: PSID(storage.as_mut_ptr().cast()),
                Attributes: SE_GROUP_ENABLED as u32,
            }]
        })
        .unwrap_or_default();
    let security = SECURITY_CAPABILITIES {
        AppContainerSid: sid.0,
        Capabilities: if capabilities.is_empty() {
            null_mut()
        } else {
            capabilities.as_mut_ptr()
        },
        CapabilityCount: capabilities.len() as u32,
        Reserved: 0,
    };
    let stdin = duplicate_standard_handle(STD_INPUT_HANDLE)?;
    let stdout = duplicate_standard_handle(STD_OUTPUT_HANDLE)?;
    let stderr = duplicate_standard_handle(STD_ERROR_HANDLE)?;
    let inherited = [stdin.0, stdout.0, stderr.0];
    let attributes = AttributeList::new(&security, &inherited)?;

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.0;
    startup.StartupInfo.hStdOutput = stdout.0;
    startup.StartupInfo.hStdError = stderr.0;
    startup.lpAttributeList = attributes.list;
    let application = wide(plan.executable.as_os_str());
    let cwd = wide(plan.cwd.as_os_str());
    let mut line = command_line(&plan.executable, &plan.args);
    let environment = environment_block()?;
    let mut process_info = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            Some(environment.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            &startup.StartupInfo,
            &mut process_info,
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!(
            "failed to create suspended AppContainer child: {error}"
        ))
    })?;
    let handles = ProcessHandles {
        process: process_info.hProcess,
        thread: process_info.hThread,
    };
    let job = crate::backend_claude::win_job::JobHandle::create_and_assign(handles.process.0)
        .map_err(|error| {
            EngineError::Backend(format!(
                "failed to assign suspended AppContainer child to Job Object: {error}"
            ))
        })?;
    if unsafe { ResumeThread(handles.thread) } == u32::MAX {
        job.kill();
        return Err(EngineError::Backend(format!(
            "failed to resume AppContainer child: {}",
            std::io::Error::last_os_error()
        )));
    }
    let wait = unsafe { WaitForSingleObject(handles.process, INFINITE) };
    if wait != WAIT_OBJECT_0 {
        job.kill();
        return Err(EngineError::Backend(format!(
            "waiting for AppContainer child returned {wait:?}"
        )));
    }
    let mut exit_code = 0u32;
    unsafe { GetExitCodeProcess(handles.process, &mut exit_code) }.map_err(|error| {
        EngineError::Backend(format!("failed to read AppContainer exit code: {error}"))
    })?;
    Ok(exit_code)
}

fn is_appcontainer_process() -> Result<bool> {
    let mut access_handle = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut access_handle) }
        .map_err(|error| EngineError::Backend(format!("failed to open child token: {error}")))?;
    let access_handle = OwnedHandle(access_handle);
    let mut value = 0u32;
    let mut returned = 0u32;
    unsafe {
        GetTokenInformation(
            access_handle.0,
            TokenIsAppContainer,
            Some((&mut value as *mut u32).cast()),
            std::mem::size_of::<u32>() as u32,
            &mut returned,
        )
    }
    .map_err(|error| EngineError::Backend(format!("failed to inspect child token: {error}")))?;
    if returned as usize != std::mem::size_of::<u32>() {
        return Err(EngineError::Backend(
            "TokenIsAppContainer returned an unexpected byte count".to_string(),
        ));
    }
    Ok(value != 0)
}

struct SelfTestRoot(PathBuf);

impl SelfTestRoot {
    fn create() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "kranz-appcontainer-production-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&path).map_err(|error| {
            EngineError::Backend(format!(
                "failed to create AppContainer self-test root {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self(path))
    }
}

impl Drop for SelfTestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Exercise the exact production helper, ACL lease, validator denial, stdio
/// inheritance, and hard-offline fs+net posture. The CLI integration test and
/// Windows CI invoke this private entry point; production remains fail-closed
/// until this receipt passes on the target host.
pub fn run_production_hostile_self_test() -> std::result::Result<String, String> {
    production_hostile_self_test().map_err(|error| error.to_string())
}

fn production_hostile_self_test() -> Result<String> {
    let root = SelfTestRoot::create()?;
    let repo = root.0.join("repo");
    let kranz_dir = repo.join(".kranz");
    let mission = kranz_dir.join("missions").join("m-production-self-test");
    let worktree = root.0.join("worktree");
    let scratch = root.0.join("scratch");
    let outside = root.0.join("outside");
    let real_checkout = root.0.join("real-checkout");
    let real_git = real_checkout.join(".git");
    for path in [
        &mission, &worktree, &scratch, &outside, &real_git, &kranz_dir,
    ] {
        std::fs::create_dir_all(path).map_err(|error| {
            EngineError::Backend(format!("failed to create {}: {error}", path.display()))
        })?;
    }
    let authority_file = kranz_dir.join("serve.token");
    std::fs::write(&authority_file, "must-not-cross")?;
    let real_source = real_checkout.join("source.rs");
    std::fs::write(&real_source, "must-not-read")?;
    let shared_git_marker = real_git.join("inspection-marker");
    std::fs::write(&shared_git_marker, "git-readable")?;
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", real_git.display()),
    )?;

    let executable = std::env::current_exe().map_err(|error| {
        EngineError::Backend(format!("failed to locate self-test executable: {error}"))
    })?;
    let executable_parent = executable.parent().ok_or_else(|| {
        EngineError::Backend("self-test executable has no parent directory".to_string())
    })?;
    let toolchain_denied_write = executable_parent.join(format!(
        "kranz-appcontainer-must-not-write-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| {
        EngineError::Backend(format!("failed to bind self-test listener: {error}"))
    })?;
    let manifest = SelfTestManifest {
        executable: executable.clone(),
        toolchain_denied_write: toolchain_denied_write.clone(),
        worktree_write: worktree.join("allowed-worktree.txt"),
        scratch_write: scratch.join("allowed-scratch.txt"),
        outside_write: outside.join("must-not-write.txt"),
        authority_file,
        real_source,
        shared_git_marker,
        loopback_addr: listener.local_addr().map_err(|error| {
            EngineError::Backend(format!("failed to inspect self-test listener: {error}"))
        })?,
        receipt: scratch.join("receipt.json"),
    };
    std::fs::write(
        worktree.join(SELF_TEST_MANIFEST),
        serde_json::to_vec_pretty(&manifest).map_err(|error| {
            EngineError::Backend(format!("failed to serialize self-test manifest: {error}"))
        })?,
    )?;

    let inputs = crate::sandbox::SandboxInputs {
        enforce: crate::types::SandboxEnforce::FsNet,
        session_cwd: worktree.clone(),
        mission_dir: mission,
        tmpdir: scratch.clone(),
        extra_write: Vec::new(),
        egress: Vec::new(),
        validator_read_deny_roots: vec![real_checkout],
    };
    let env = crate::agent_env::sanitized_child_env(&scratch.join("home"), &[]);
    let before = snapshot_dacl(&worktree)?;
    let prepared = prepare_launch(
        &inputs,
        &executable,
        &[INTERNAL_HOSTILE_CHILD_ARG.to_string()],
        &env,
    )?;
    let PreparedLaunch {
        program,
        args,
        lease,
    } = prepared;
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(&worktree)
        .env_clear()
        .envs(&env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|error| {
            EngineError::Backend(format!("failed to spawn production helper: {error}"))
        })?;
    drop(lease);
    let after = snapshot_dacl(&worktree)?;
    if !output.status.success() {
        return Err(EngineError::Backend(format!(
            "production AppContainer helper failed with {:?}: stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut receipt: ProductionHostileReceipt =
        serde_json::from_slice(&std::fs::read(&manifest.receipt).map_err(|error| {
            EngineError::Backend(format!("failed to read production receipt: {error}"))
        })?)
        .map_err(|error| EngineError::Backend(format!("invalid production receipt: {error}")))?;
    receipt.dacl_restored = before.acl == after.acl;
    let _ = std::fs::remove_file(&toolchain_denied_write);
    let all_passed = receipt.token_is_appcontainer
        && receipt.toolchain_read
        && receipt.toolchain_write_denied
        && receipt.worktree_write
        && receipt.scratch_write
        && receipt.outside_write_denied
        && receipt.authority_read_denied
        && receipt.real_checkout_read_denied
        && receipt.shared_git_read
        && receipt.network_denied
        && receipt.dacl_restored;
    if !all_passed {
        return Err(EngineError::Backend(format!(
            "production hostile receipt contained a failed assertion: {receipt:?}"
        )));
    }
    serde_json::to_string(&receipt).map_err(|error| {
        EngineError::Backend(format!("failed to render production receipt: {error}"))
    })
}

pub fn internal_hostile_child_requested() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == INTERNAL_HOSTILE_CHILD_ARG)
}

pub fn run_internal_hostile_child() -> std::result::Result<(), String> {
    hostile_child().map_err(|error| error.to_string())
}

fn hostile_child() -> Result<()> {
    let manifest: SelfTestManifest = serde_json::from_slice(&std::fs::read(
        std::env::current_dir()?.join(SELF_TEST_MANIFEST),
    )?)
    .map_err(|error| EngineError::Backend(format!("invalid hostile-child manifest: {error}")))?;
    let receipt = ProductionHostileReceipt {
        token_is_appcontainer: is_appcontainer_process()?,
        toolchain_read: std::fs::metadata(&manifest.executable).is_ok(),
        toolchain_write_denied: std::fs::write(&manifest.toolchain_denied_write, "escape").is_err(),
        worktree_write: std::fs::write(&manifest.worktree_write, "allowed").is_ok(),
        scratch_write: std::fs::write(&manifest.scratch_write, "allowed").is_ok(),
        outside_write_denied: std::fs::write(&manifest.outside_write, "escape").is_err(),
        authority_read_denied: std::fs::read(&manifest.authority_file).is_err(),
        real_checkout_read_denied: std::fs::read(&manifest.real_source).is_err(),
        shared_git_read: std::fs::read_to_string(&manifest.shared_git_marker)
            .is_ok_and(|value| value == "git-readable"),
        network_denied: TcpStream::connect_timeout(
            &manifest.loopback_addr,
            std::time::Duration::from_secs(2),
        )
        .is_err(),
        // The parent fills this after dropping the ACL/profile lease.
        dacl_restored: false,
    };
    std::fs::write(
        &manifest.receipt,
        serde_json::to_vec_pretty(&receipt).map_err(|error| {
            EngineError::Backend(format!("failed to serialize hostile receipt: {error}"))
        })?,
    )?;
    Ok(())
}

pub fn internal_self_test_requested() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == INTERNAL_SELF_TEST_ARG)
}

/// True only for the private raw argv handled before clap/tracing initialization.
pub fn internal_launcher_requested() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == INTERNAL_LAUNCHER_ARG)
}

/// Entry point used by the `kranz` binary before ordinary CLI setup.
pub fn run_internal_launcher() -> std::result::Result<u32, String> {
    let plan_path = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .ok_or_else(|| "AppContainer helper missing its launch-plan path".to_string())?;
    let bytes = std::fs::read(&plan_path)
        .map_err(|error| format!("failed to read AppContainer launch plan: {error}"))?;
    // Remove before any hostile instruction runs. The child receives the plan's
    // arguments and environment, never the engine-only launch file.
    let _ = std::fs::remove_file(&plan_path);
    let plan: LaunchPlan = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid AppContainer launch plan: {error}"))?;
    run_plan(plan).map_err(|error| error.to_string())
}
