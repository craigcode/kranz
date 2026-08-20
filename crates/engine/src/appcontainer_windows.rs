//! Stable Windows less-privileged AppContainer (LPAC) launcher used by
//! production session and gate paths.
//!
//! The engine starts a trusted copy of itself as a thin launcher. That helper
//! creates the prompt-injectable child suspended, assigns it to a kill-on-close
//! Job Object, applies `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, and only
//! then resumes it. The helper inherits the engine's already-cleared environment
//! and stdio pipes, so the existing async stream bounds and timeout machinery do
//! not need a second Windows-only implementation.
//!
//! Filesystem authority is granted to a unique per-launch AppContainer SID. The
//! parent retains a no-follow handle for every DACL it changes and removes only
//! that SID's ACEs when the wrapped process is reaped or aborted; removing an
//! inheritable parent ACE also removes its inherited copies from descendants.
//! A bounded host-local mutex serializes those DACL read/modify/write batches,
//! so this composes safely across overlapping launches that share Git/toolchain
//! roots and restores the original descriptor exactly when no unrelated ACL
//! change occurred. The profile name is random and deleted with the same lease.
//! A process crash can leave an inert orphan SID ACE, but no other AppContainer
//! principal can use it.

use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, LocalFree, DUPLICATE_SAME_ACCESS, GENERIC_READ, HANDLE, HLOCAL,
    WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, DENY_ACCESS, EXPLICIT_ACCESS_W,
    GRANT_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    AclSizeInformation, CreateWellKnownSid, DeleteAce, EqualSid, FreeSid, GetAce,
    GetAclInformation, GetTokenInformation, TokenIsAppContainer, WinBuiltinAnyPackageSid,
    WinCapabilityInternetClientSid, ACCESS_ALLOWED_ACE, ACCESS_DENIED_ACE, ACE_HEADER, ACL,
    ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, NO_INHERITANCE,
    OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    TOKEN_INFORMATION_CLASS, TOKEN_QUERY, WELL_KNOWN_SID_TYPE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, ReadFile, BY_HANDLE_FILE_INFORMATION, DELETE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
};
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::SystemServices::SE_GROUP_ENABLED;
use windows::Win32::System::Threading::{
    CreateMutexW, CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, OpenProcessToken, ReleaseMutex,
    ResumeThread, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

pub(crate) const INTERNAL_LAUNCHER_ARG: &str = "__kranz-appcontainer-launch";
pub(crate) const INTERNAL_SELF_TEST_ARG: &str = "__kranz-appcontainer-self-test";
pub(crate) const INTERNAL_GATE_SELF_TEST_ARG: &str = "__kranz-appcontainer-gate-self-test";
pub(crate) const INTERNAL_HOSTILE_CHILD_ARG: &str = "__kranz-appcontainer-hostile-child";
const PLAN_VERSION: u32 = 1;
const SELF_TEST_MANIFEST: &str = "kranz-appcontainer-production-self-test.json";
const DACL_MUTEX_NAME: &str = "Local\\Kranz.AppContainer.Dacl.v1";
const DACL_MUTEX_TIMEOUT_MS: u32 = 30_000;
const PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT: u32 = 1;
const GATE_OVERHEAD_REPETITIONS: usize = 7;
const GATE_OVERHEAD_TARGET_PERCENT: f64 = 10.0;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchPlan {
    version: u32,
    profile_name: String,
    executable: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    allow_network: bool,
    /// Search path for the AppContainer child. `where`/`cmd` fail with
    /// Access denied if PATH still lists Program Files and other
    /// ungranted directories, even after the toolchain files themselves
    /// have exact ACEs.
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelfTestManifest {
    executable: PathBuf,
    toolchain_denied_write: PathBuf,
    worktree_write: PathBuf,
    scratch_write: PathBuf,
    outside_write: PathBuf,
    broad_app_packages_write: PathBuf,
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
    pub all_application_packages_denied: bool,
    pub toolchain_read: bool,
    pub toolchain_write_denied: bool,
    pub worktree_write: bool,
    pub scratch_write: bool,
    pub outside_write_denied: bool,
    pub authority_read_denied: bool,
    pub real_checkout_read_denied: bool,
    pub shared_git_read: bool,
    pub overlapping_lease_safe: bool,
    pub tampered_git_pointer_refused: bool,
    pub network_denied: bool,
    pub dacl_restored: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GateTimingReceipt {
    command: String,
    repetitions: usize,
    off_samples_ms: Vec<f64>,
    appcontainer_samples_ms: Vec<f64>,
    off_median_ms: f64,
    appcontainer_median_ms: f64,
    overhead_ms: f64,
    overhead_percent: f64,
    within_target: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductionGateReceipt {
    host: crate::sandbox_windows::WindowsSandboxProbeReport,
    enforcement: &'static str,
    provider: &'static str,
    overhead_target_percent: f64,
    node: GateTimingReceipt,
    rust: GateTimingReceipt,
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
    handle: OwnedHandle,
}

// File handles are kernel-object references whose access rights and lifetime
// are independent of the thread using them. The snapshot owns its handle and
// only performs synchronous Get/SetSecurityInfo calls during lease cleanup,
// so moving the whole snapshot with an async session is sound. Keep the
// narrower mutex guard non-Send: Win32 mutex ownership is thread-affine.
unsafe impl Send for DaclSnapshot {}

#[derive(Debug)]
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct DaclMutationGuard {
    handle: OwnedHandle,
}

impl DaclMutationGuard {
    fn acquire() -> Result<Self> {
        let name = wide(DACL_MUTEX_NAME);
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .map(OwnedHandle)
            .map_err(|error| {
                EngineError::Backend(format!(
                    "failed to create/open the AppContainer DACL mutation mutex: {error}"
                ))
            })?;
        match unsafe { WaitForSingleObject(handle.0, DACL_MUTEX_TIMEOUT_MS) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self { handle }),
            WAIT_TIMEOUT => Err(EngineError::Backend(format!(
                "timed out after {DACL_MUTEX_TIMEOUT_MS}ms waiting for the AppContainer DACL mutation mutex"
            ))),
            status => Err(EngineError::Backend(format!(
                "waiting for the AppContainer DACL mutation mutex returned {status:?}"
            ))),
        }
    }
}

impl Drop for DaclMutationGuard {
    fn drop(&mut self) {
        if let Err(error) = unsafe { ReleaseMutex(self.handle.0) } {
            tracing::error!(%error, "failed to release AppContainer DACL mutation mutex");
        }
    }
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
        match (
            DaclMutationGuard::acquire(),
            derive_profile_sid(&self.profile_name),
        ) {
            (Ok(_guard), Ok(sid)) => {
                // Parent directories first: removing their inheritable
                // AppContainer ACEs makes Windows retract inherited copies
                // from existing children. Remove ONLY this random profile's
                // ACEs; replacing whole snapshots would race overlapping
                // launches that legitimately touch shared Git/toolchain roots.
                self.original_dacls
                    .sort_by_key(|entry| entry.path.components().count());
                for snapshot in &self.original_dacls {
                    if let Err(error) = remove_profile_aces(snapshot, sid.0) {
                        tracing::error!(path = %snapshot.path.display(), error = %error,
                            "failed to remove AppContainer ACEs from a DACL");
                    }
                }
            }
            (Err(error), _) => {
                tracing::error!(profile = %self.profile_name, error = %error,
                    "failed to lock AppContainer DACL cleanup");
            }
            (_, Err(error)) => {
                tracing::error!(profile = %self.profile_name, error = %error,
                    "failed to derive AppContainer SID for DACL cleanup");
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            READ_CONTROL.0 | WRITE_DAC.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map(OwnedHandle)
    .map_err(|error| {
        EngineError::Backend(format!(
            "failed to retain no-follow DACL capability for {}: {error}",
            path.display()
        ))
    })?;
    let mut acl: *mut ACL = null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    win32(unsafe {
        GetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut acl),
            None,
            Some(&mut descriptor),
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
        handle,
    })
}

fn remove_profile_aces(snapshot: &DaclSnapshot, sid: PSID) -> Result<()> {
    let mut current_acl: *mut ACL = null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    win32(unsafe {
        GetSecurityInfo(
            snapshot.handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut current_acl),
            None,
            Some(&mut descriptor),
        )
    })?;
    let _descriptor = LocalAllocation(HLOCAL(descriptor.0));
    if current_acl.is_null() {
        return Ok(());
    }

    let len = unsafe { (*current_acl).AclSize as usize };
    let mut acl = unsafe { std::slice::from_raw_parts(current_acl.cast::<u8>(), len) }.to_vec();
    let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
    let mut info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            acl_ptr,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|error| EngineError::Backend(format!("failed to inspect DACL ACEs: {error}")))?;

    // SetEntriesInAclW emits ordinary allow/deny ACEs for a SID trustee.
    // Walk backwards so DeleteAce indices remain valid, and leave every ACE
    // belonging to another concurrent launch or the operator untouched.
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    for index in (0..info.AceCount).rev() {
        let mut ace = null_mut();
        unsafe { GetAce(acl_ptr, index, &mut ace) }
            .map_err(|error| EngineError::Backend(format!("failed to read DACL ACE: {error}")))?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        let trustee = match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE => unsafe {
                &(*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart as *const u32
            },
            ACCESS_DENIED_ACE_TYPE => unsafe {
                &(*ace.cast::<ACCESS_DENIED_ACE>()).SidStart as *const u32
            },
            _ => continue,
        };
        if unsafe { EqualSid(PSID(trustee.cast_mut().cast()), sid) }.is_ok() {
            unsafe { DeleteAce(acl_ptr, index) }.map_err(|error| {
                EngineError::Backend(format!("failed to remove AppContainer DACL ACE: {error}"))
            })?;
        }
    }

    // DeleteAce leaves capacity in the ACL buffer. Compact the advertised
    // size to the bytes still in use so an uncontended add/remove cycle
    // recovers the original descriptor bytes instead of persisting slack.
    let mut final_info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            acl_ptr,
            (&mut final_info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|error| EngineError::Backend(format!("failed to size cleaned DACL: {error}")))?;
    let compact_len = u16::try_from(final_info.AclBytesInUse).map_err(|_| {
        EngineError::Backend("cleaned DACL exceeded the Win32 ACL size limit".to_string())
    })?;
    unsafe { (*acl_ptr).AclSize = compact_len };

    win32(unsafe {
        SetSecurityInfo(
            snapshot.handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(acl_ptr),
            None,
        )
    })
}

fn apply_acl_change(change: &AclChange, sid: PSID, handle: HANDLE) -> Result<()> {
    let mut old_acl: *mut ACL = null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    win32(unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut old_acl),
            None,
            Some(&mut descriptor),
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
        SetSecurityInfo(
            handle,
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

fn path_extensions(program: &Path, env: &HashMap<String, String>) -> Vec<String> {
    if program.extension().is_some() {
        vec![String::new()]
    } else {
        env_value_ci(env, "PATHEXT")
            .unwrap_or(".COM;.EXE;.BAT;.CMD")
            .split(';')
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }
}

fn find_on_path(program: &Path, env: &HashMap<String, String>) -> Option<PathBuf> {
    find_all_on_path(program, env).into_iter().next()
}

fn find_all_on_path(program: &Path, env: &HashMap<String, String>) -> Vec<PathBuf> {
    let has_path = program.components().count() > 1;
    let extensions = path_extensions(program, env);
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
    let mut found = Vec::new();
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
                found.push(candidate);
                break;
            }
        }
    }
    found
}

fn resolve_nonsystem_path_files(program: &Path, env: &HashMap<String, String>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for candidate in find_all_on_path(program, env) {
        push_entry_point(&mut files, &candidate);
    }
    files.sort();
    files.dedup();
    files
}

fn resolve_executable(program: &Path, env: &HashMap<String, String>) -> Result<PathBuf> {
    let candidate = find_on_path(program, env).ok_or_else(|| {
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

fn push_entry_point(files: &mut Vec<PathBuf>, path: &Path) {
    if !path.is_file() {
        return;
    }
    let Ok(canon) = std::fs::canonicalize(path) else {
        return;
    };
    if system_managed_path(&canon) {
        return;
    }
    files.push(canon);
}

fn push_npm_scripts(files: &mut Vec<PathBuf>, dir: &Path) {
    let npm_bin = dir.join("node_modules").join("npm").join("bin");
    for name in ["npm-cli.js", "npx-cli.js", "npm-prefix.js"] {
        push_entry_point(files, &npm_bin.join(name));
    }
}

fn volume_root(path: &Path) -> bool {
    path.components().all(|component| {
        matches!(
            component,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        )
    })
}

/// Parent directories CreateProcess must traverse to reach an entry-point
/// file. LPAC is not Users/Everyone/ALL APPLICATION PACKAGES, so a file ACE
/// is useless unless each ancestor also allows FILE_TRAVERSE.
fn ancestor_directories(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut current = path.parent();
    while let Some(dir) = current {
        if volume_root(dir) || system_managed_path(dir) {
            break;
        }
        out.push(dir.to_path_buf());
        current = dir.parent();
    }
    out
}

/// Exact Node/npm/Cargo files that must receive a direct (non-inheriting) RX
/// ACE. Hosted Windows toolchains mark `node.exe`, `npm.cmd`, npm CLI scripts,
/// and rustup proxies with `SE_DACL_PROTECTED`, so a parent-directory grant
/// never reaches them.
///
/// Source: https://learn.microsoft.com/windows/win32/secauthz/ace-inheritance
/// (`SE_DACL_PROTECTED` prevents a DACL from inheriting parent ACEs).
fn toolchain_entry_points(env: &HashMap<String, String>) -> Vec<PathBuf> {
    const NODE_SHIMS: &[&str] = &["npm.cmd", "npm", "npm.ps1", "npx.cmd", "npx", "npx.ps1"];
    const RUST_BINARIES: &[&str] = &[
        "cargo.exe",
        "rustc.exe",
        "rustdoc.exe",
        "rustup.exe",
        "cargo",
        "rustc",
        "rustdoc",
        "rustup",
    ];

    let mut files = Vec::new();
    for node in resolve_nonsystem_path_files(Path::new("node"), env) {
        push_entry_point(&mut files, &node);
        if let Some(dir) = node.parent() {
            for name in NODE_SHIMS {
                push_entry_point(&mut files, &dir.join(name));
            }
            push_npm_scripts(&mut files, dir);
        }
    }
    for npm in resolve_nonsystem_path_files(Path::new("npm"), env) {
        push_entry_point(&mut files, &npm);
        if let Some(dir) = npm.parent() {
            push_npm_scripts(&mut files, dir);
        }
    }
    for program in ["cargo", "rustc", "rustup"] {
        for exe in resolve_nonsystem_path_files(Path::new(program), env) {
            push_entry_point(&mut files, &exe);
        }
    }
    if let Some(rustup) = env_value_ci(env, "RUSTUP_HOME").map(PathBuf::from) {
        if let Ok(toolchains) = std::fs::read_dir(rustup.join("toolchains")) {
            for entry in toolchains.flatten() {
                let bin = entry.path().join("bin");
                for name in RUST_BINARIES {
                    push_entry_point(&mut files, &bin.join(name));
                }
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

fn contained_search_path(
    inputs: &crate::sandbox::SandboxInputs,
    executable: &Path,
    env: &HashMap<String, String>,
) -> Result<String> {
    let mut dirs = Vec::new();
    if let Some(root) = env_value_ci(env, "SystemRoot") {
        let root = PathBuf::from(root);
        dirs.push(root.join("System32"));
        dirs.push(root);
    }
    dirs.extend(crate::sandbox::write_allowlist(inputs));
    dirs.extend(read_roots(
        executable,
        &inputs.session_cwd,
        &inputs.mission_dir,
        env,
    )?);
    for file in toolchain_entry_points(env) {
        if let Some(parent) = file.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs.retain(|dir| dir.is_dir());
    dirs.sort();
    dirs.dedup();
    std::env::join_paths(&dirs)
        .map(|value| value.to_string_lossy().into_owned())
        .map_err(|error| {
            EngineError::Backend(format!("failed to build AppContainer PATH: {error}"))
        })
}

/// A single ACL grant slower than this is reported by path: it means the
/// root carries enough existing descendants that inheritance propagation,
/// not the launch itself, is the cost.
const SLOW_ACL_GRANT: std::time::Duration = std::time::Duration::from_secs(2);

const GIT_POINTER_MAX_BYTES: u64 = 4096;

fn read_small_regular_file(path: &Path, label: &str) -> Result<String> {
    let path_wide = wide(path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map(OwnedHandle)
    .map_err(|error| {
        EngineError::Backend(format!(
            "failed to open {label} {} without following reparse points: {error}",
            path.display()
        ))
    })?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle.0, &mut info) }.map_err(|error| {
        EngineError::Backend(format!(
            "failed to inspect open {label} {}: {error}",
            path.display()
        ))
    })?;
    let attributes = info.dwFileAttributes;
    let size = (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow);
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
        || size > GIT_POINTER_MAX_BYTES
    {
        return Err(EngineError::Backend(format!(
            "{label} {} must be a small no-follow regular file",
            path.display()
        )));
    }
    let mut bytes = vec![0u8; size as usize];
    let mut read = 0u32;
    if !bytes.is_empty() {
        unsafe { ReadFile(handle.0, Some(&mut bytes), Some(&mut read), None) }.map_err(
            |error| {
                EngineError::Backend(format!(
                    "failed to read open {label} {}: {error}",
                    path.display()
                ))
            },
        )?;
    }
    if read as usize != bytes.len() {
        return Err(EngineError::Backend(format!(
            "{label} {} changed while it was being read",
            path.display()
        )));
    }
    String::from_utf8(bytes).map_err(|error| {
        EngineError::Backend(format!(
            "{label} {} is not valid UTF-8: {error}",
            path.display()
        ))
    })
}

fn resolve_git_dir(root: &Path) -> Result<PathBuf> {
    let dot_git = root.join(".git");
    let metadata = std::fs::symlink_metadata(&dot_git).map_err(|error| {
        EngineError::Backend(format!("failed to inspect {}: {error}", dot_git.display()))
    })?;
    let candidate = if metadata.file_type().is_dir() {
        dot_git
    } else if metadata.file_type().is_file() {
        let pointer = read_small_regular_file(&dot_git, "Git worktree pointer")?;
        let mut nonempty = pointer.lines().filter(|line| !line.trim().is_empty());
        let line = nonempty.next().ok_or_else(|| {
            EngineError::Backend(format!(
                "Git worktree pointer {} is empty",
                dot_git.display()
            ))
        })?;
        if nonempty.next().is_some() {
            return Err(EngineError::Backend(format!(
                "Git worktree pointer {} contains unexpected extra records",
                dot_git.display()
            )));
        }
        let raw = line.strip_prefix("gitdir: ").ok_or_else(|| {
            EngineError::Backend(format!(
                "Git worktree pointer {} has an invalid record",
                dot_git.display()
            ))
        })?;
        let path = PathBuf::from(raw.trim());
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    } else {
        return Err(EngineError::Backend(format!(
            "Git metadata entry {} is not a no-follow file or directory",
            dot_git.display()
        )));
    };
    std::fs::canonicalize(&candidate).map_err(|error| {
        EngineError::Backend(format!(
            "failed to canonicalize Git directory {}: {error}",
            candidate.display()
        ))
    })
}

fn resolve_common_dir(git_dir: &Path) -> Result<PathBuf> {
    let commondir = git_dir.join("commondir");
    match std::fs::symlink_metadata(&commondir) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let raw = read_small_regular_file(&commondir, "Git common-dir pointer")?;
            let raw = raw.trim();
            if raw.is_empty() || raw.lines().count() != 1 {
                return Err(EngineError::Backend(format!(
                    "Git common-dir pointer {} is invalid",
                    commondir.display()
                )));
            }
            let path = PathBuf::from(raw);
            let path = if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            };
            std::fs::canonicalize(&path).map_err(|error| {
                EngineError::Backend(format!(
                    "failed to canonicalize Git common directory {}: {error}",
                    path.display()
                ))
            })
        }
        Ok(_) => Err(EngineError::Backend(format!(
            "Git common-dir pointer {} is not a no-follow regular file",
            commondir.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(git_dir.to_path_buf()),
        Err(error) => Err(EngineError::Backend(format!(
            "failed to inspect Git common-dir pointer {}: {error}",
            commondir.display()
        ))),
    }
}

fn trusted_git_read_root(cwd: &Path, mission_dir: &Path) -> Result<PathBuf> {
    let mission = std::fs::canonicalize(mission_dir).map_err(|error| {
        EngineError::Backend(format!(
            "failed to canonicalize mission metadata root {}: {error}",
            mission_dir.display()
        ))
    })?;
    let missions = mission
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("missions")));
    let kranz = missions
        .and_then(Path::parent)
        .filter(|path| path.file_name() == Some(OsStr::new(".kranz")));
    let repo = kranz.and_then(Path::parent).ok_or_else(|| {
        EngineError::Backend(format!(
            "mission metadata root {} is not under <repo>/.kranz/missions",
            mission.display()
        ))
    })?;
    let trusted_git_dir = resolve_git_dir(repo)?;
    let trusted_common = resolve_common_dir(&trusted_git_dir)?;
    let session_git_dir = resolve_git_dir(cwd)?;
    let session_common = resolve_common_dir(&session_git_dir)?;
    if session_common != trusted_common
        || !(session_git_dir == trusted_common || path_contains(&trusted_common, &session_git_dir))
    {
        return Err(EngineError::Backend(format!(
            "session Git metadata {} does not belong to trusted common directory {}; refusing before ACL mutation",
            session_git_dir.display(),
            trusted_common.display()
        )));
    }
    Ok(trusted_common)
}

fn read_roots(
    executable: &Path,
    cwd: &Path,
    mission_dir: &Path,
    env: &HashMap<String, String>,
) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    if let Some(parent) = executable.parent() {
        if !system_managed_path(parent) {
            roots.push(parent.to_path_buf());
        }
    }
    if let Some(path) = env_value_ci(env, "PATH") {
        roots.extend(std::env::split_paths(path).filter_map(|entry| {
            (entry.is_dir() && caller_owned_path(&entry, cwd))
                .then(|| std::fs::canonicalize(entry).ok())
                .flatten()
        }));
    }
    // Hosted Windows installs Node under C:\hostedtoolcache rather than the
    // operator profile, so the general caller-owned PATH rule above excludes
    // it. Grant each installation directory RX for inheriting children, then
    // add exact-file ACEs for node/npm/cargo entry points whose protected
    // DACLs do not inherit that directory grant. rustup's toolchain root is
    // added separately below. The recursive-root validator still refuses
    // either directory grant if it would cover Kranz authority or metadata.
    for program in ["node", "cargo"] {
        for executable in resolve_nonsystem_path_files(Path::new(program), env) {
            if let Some(parent) = executable.parent() {
                roots.push(parent.to_path_buf());
            }
        }
    }
    if let Some(rustup) = env_value_ci(env, "RUSTUP_HOME").map(PathBuf::from) {
        if rustup.is_dir() {
            if let Ok(root) = std::fs::canonicalize(rustup) {
                roots.push(root);
            }
        }
    }
    if let Some(cargo) = env_value_ci(env, "CARGO_HOME").map(PathBuf::from) {
        for name in ["bin", "registry", "git"] {
            let root = cargo.join(name);
            if root.exists() {
                if let Ok(root) = std::fs::canonicalize(root) {
                    roots.push(root);
                }
            }
        }
    }
    roots.push(trusted_git_read_root(cwd, mission_dir)?);
    roots.sort();
    roots.dedup();
    Ok(roots)
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

/// The spelling every containment comparison is made in.
/// [`std::fs::canonicalize`] returns VERBATIM paths (`\\?\C:\Program
/// Files\...`) while environment values, operator config, and mission paths
/// never carry that prefix, and Windows path comparison is case-insensitive
/// besides. Both differences answered containment questions WRONG: a
/// canonicalized entry point under `%PROGRAMFILES%` tested NO against
/// [`system_managed_path`], so every ancestor of a hosted toolchain took a
/// DACL change the OS charged ~90 seconds each, ten per launch (measured on
/// windows-latest, run 32322660181).
fn comparable_path(path: &Path) -> PathBuf {
    PathBuf::from(
        local_dos_path(path)
            .as_os_str()
            .to_string_lossy()
            .to_lowercase(),
    )
}

fn path_contains(root: &Path, child: &Path) -> bool {
    let root = comparable_path(root);
    let child = comparable_path(child);
    child == root || child.starts_with(&root)
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
    let read_roots = read_roots(executable, &inputs.session_cwd, &inputs.mission_dir, env)?;
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
    let traverse = FILE_GENERIC_EXECUTE.0;
    for path in toolchain_entry_points(env) {
        changes.push(AclChange {
            path: path.clone(),
            permissions: rx,
            inherit: false,
            mode: AclMode::Grant,
        });
        for ancestor in ancestor_directories(&path) {
            changes.push(AclChange {
                path: ancestor,
                permissions: traverse,
                inherit: false,
                mode: AclMode::Grant,
            });
        }
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
    // The child PATH is narrower than the host PATH and deliberately includes
    // declared workspace roots. Use that SAME path while discovering exact
    // Node/npm/Cargo entry points for ACL grants: a workspace-staged runtime
    // can carry a protected DACL, so an inheritable grant on its parent is not
    // proof that LPAC can execute the existing file. This does not grant a new
    // root; it only adds a direct RX ACE to a known tool name already inside
    // the contained search path.
    let path = contained_search_path(inputs, &executable, env)?;
    let mut acl_env = env.clone();
    acl_env.insert("PATH".to_string(), path.clone());
    let mut changes = acl_changes(inputs, &executable, &acl_env)?;
    // All DACL updates are read/modify/write operations. Serialize the batch
    // across Kranz processes so simultaneous prepare/drop paths cannot publish
    // stale ACL copies over one another on shared toolchain or Git roots.
    let _guard = DaclMutationGuard::acquire()?;
    // Key retained handles by physical Windows spelling too. A worktree can
    // be present as both `C:\...` and canonical `\\?\C:\...` changes with
    // different permissions, so the exact-change collapse below deliberately
    // keeps both operations. They must still share ONE cleanup capability:
    // removing the profile ACE repeatedly through alias handles can republish
    // a stale inherited DACL and make the next launch lose execute access.
    let mut seen = BTreeMap::new();
    // One ancestor directory is shared by many toolchain entry points, and a
    // read root can arrive in both verbatim and plain form. Applying the same
    // ACE twice is a no-op the OS still charges full price for, so collapse
    // exact repeats before touching a single descriptor.
    let mut applied = std::collections::HashSet::new();
    changes.retain(|change| {
        applied.insert((
            comparable_path(&change.path),
            change.permissions,
            change.inherit,
            change.mode,
        ))
    });
    for change in &changes {
        let key = comparable_path(&change.path);
        if !seen.contains_key(&key) {
            let index = lease.original_dacls.len();
            lease.original_dacls.push(snapshot_dacl(&change.path)?);
            seen.insert(key, index);
        }
    }
    for change in &changes {
        let key = comparable_path(&change.path);
        let snapshot = &lease.original_dacls[seen[&key]];
        let started = std::time::Instant::now();
        apply_acl_change(change, sid.0, snapshot.handle.0)?;
        // An inheritable ACE on a directory makes Windows propagate it to
        // every existing descendant, so one grant costs a full tree rewrite.
        // Name any root where that dominates the launch instead of letting a
        // wrapped command look mysteriously slow.
        let elapsed = started.elapsed();
        if elapsed >= SLOW_ACL_GRANT {
            tracing::warn!(
                path = %change.path.display(),
                inherit = change.inherit,
                elapsed_ms = elapsed.as_millis(),
                "AppContainer ACL grant propagated slowly; an inheritable ACE rewrites every \
                 descendant of this root"
            );
            eprintln!(
                "slow AppContainer ACL grant: path={} inherit={} elapsed_ms={}",
                change.path.display(),
                change.inherit,
                elapsed.as_millis()
            );
        }
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
        path: Some(path),
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
    fn new(
        security: &SECURITY_CAPABILITIES,
        handles: &[HANDLE],
        all_application_packages_policy: &u32,
    ) -> Result<Self> {
        let mut bytes = 0usize;
        let _ = unsafe { InitializeProcThreadAttributeList(None, 3, None, &mut bytes) };
        if bytes == 0 {
            return Err(EngineError::Backend(
                "AppContainer attribute-list sizing returned zero bytes".to_string(),
            ));
        }
        let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 3, None, &mut bytes) }.map_err(
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
        unsafe {
            UpdateProcThreadAttribute(
                result.list,
                0,
                PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY as usize,
                Some((all_application_packages_policy as *const u32).cast()),
                std::mem::size_of::<u32>(),
                None,
                None,
            )
        }
        .map_err(|error| {
            EngineError::Backend(format!(
                "failed to opt the AppContainer out of ALL APPLICATION PACKAGES: {error}"
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

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE, label: &str) -> Result<Vec<u8>> {
    let mut bytes = 0u32;
    let _ = unsafe { CreateWellKnownSid(kind, None, None, &mut bytes) };
    if bytes == 0 {
        return Err(EngineError::Backend(format!(
            "{label} SID sizing returned zero bytes"
        )));
    }
    let mut storage = vec![0u8; bytes as usize];
    unsafe {
        CreateWellKnownSid(
            kind,
            None,
            Some(PSID(storage.as_mut_ptr().cast())),
            &mut bytes,
        )
    }
    .map_err(|error| EngineError::Backend(format!("failed to build {label} SID: {error}")))?;
    Ok(storage)
}

fn internet_capability() -> Result<Vec<u8>> {
    well_known_sid(WinCapabilityInternetClientSid, "internetClient capability")
}

fn quote_arg(value: &OsStr, force_quotes: bool) -> Vec<u16> {
    let source: Vec<u16> = value.encode_wide().collect();
    // Match std::process::Command's CreateProcessW encoding: arguments only
    // need outer quotes when empty or containing whitespace. Always quoting
    // switches such as `/C` changes cmd.exe's special parsing of the command
    // tail (`"/C" "set ...&& ..."` retains the tail's opening quote).
    let quote = force_quotes
        || source.is_empty()
        || source
            .iter()
            .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16);
    let mut out = Vec::with_capacity(source.len() + usize::from(quote) * 2);
    if quote {
        out.push(b'"' as u16);
    }
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
    out.extend(std::iter::repeat_n(
        b'\\' as u16,
        slashes * if quote { 2 } else { 1 },
    ));
    if quote {
        out.push(b'"' as u16);
    }
    out
}

fn command_line(executable: &Path, args: &[String]) -> Vec<u16> {
    let mut out = quote_arg(executable.as_os_str(), true);
    for arg in args {
        out.push(b' ' as u16);
        out.extend(quote_arg(OsStr::new(arg), false));
    }
    out.push(0);
    out
}

/// Convert a canonical local-drive path (`\\?\D:\...`) into the ordinary DOS
/// spelling accepted by cmd.exe as a current directory. Keep UNC/device paths
/// untouched: collapsing `\\?\UNC\...` would change their meaning.
fn local_dos_path(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(std::path::Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    let std::path::Prefix::VerbatimDisk(disk) = prefix.kind() else {
        return path.to_path_buf();
    };
    let mut normalized = PathBuf::from(format!("{}:\\", char::from(disk).to_ascii_uppercase()));
    for component in components {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => normalized.push(".."),
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::Prefix(_) => return path.to_path_buf(),
        }
    }
    normalized
}

fn drive_current_directory_variable(cwd: &Path) -> Option<(OsString, OsString)> {
    let cwd = local_dos_path(cwd);
    let mut components = cwd.components();
    let prefix = match components.next()? {
        std::path::Component::Prefix(prefix) => prefix,
        _ => return None,
    };
    let disk = match prefix.kind() {
        std::path::Prefix::Disk(disk) => disk,
        _ => return None,
    };
    Some((
        OsString::from(format!("={}:", char::from(disk).to_ascii_uppercase())),
        cwd.into_os_string(),
    ))
}

fn environment_block(cwd: &Path, path: Option<&str>) -> Result<Vec<u16>> {
    let mut values = BTreeMap::<String, (OsString, OsString)>::new();
    for (key, value) in std::env::vars_os() {
        let folded = key.to_string_lossy().to_ascii_uppercase();
        values.insert(folded, (key, value));
    }
    if let Some(path) = path {
        values.insert(
            "PATH".to_string(),
            (OsString::from("PATH"), OsString::from(path)),
        );
    }
    // When a caller supplies an environment block, CreateProcessW does not
    // propagate the special per-drive current-directory variables (`=C:`,
    // `=D:`, ...). cmd.exe needs the entry for a cross-drive launch (the
    // hosted runner executes cmd.exe from C: with the gate worktree on D:),
    // otherwise process creation fails with ERROR_ENVVAR_NOT_FOUND (203).
    // Microsoft requires these pseudo variables to be added and sorted with
    // the rest of the explicit block.
    if let Some((key, value)) = drive_current_directory_variable(cwd) {
        let folded = key.to_string_lossy().to_ascii_uppercase();
        values.insert(folded, (key, value));
    }
    let mut out = Vec::new();
    for (_folded, (key, value)) in values {
        let key: Vec<u16> = key.encode_wide().collect();
        let value: Vec<u16> = value.encode_wide().collect();
        let drive_letter = key.get(1).is_some_and(|unit| {
            (*unit >= b'A' as u16 && *unit <= b'Z' as u16)
                || (*unit >= b'a' as u16 && *unit <= b'z' as u16)
        });
        let drive_current_directory =
            key.len() == 3 && key[0] == b'=' as u16 && drive_letter && key[2] == b':' as u16;
        if key.is_empty()
            || key.contains(&0)
            || (key.contains(&(b'=' as u16)) && !drive_current_directory)
            || value.contains(&0)
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
    // LPAC opts out of the broad ALL APPLICATION PACKAGES principal. This is
    // required for an allowlist boundary: a regular AppContainer can still
    // access operator resources whose DACL grants that shared group.
    let all_application_packages_policy = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
    let attributes = AttributeList::new(&security, &inherited, &all_application_packages_policy)?;

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.0;
    startup.StartupInfo.hStdOutput = stdout.0;
    startup.StartupInfo.hStdError = stderr.0;
    startup.lpAttributeList = attributes.list;
    let application = wide(plan.executable.as_os_str());
    // `canonicalize` yields a verbatim local-drive path on Windows. The Win32
    // API accepts that spelling, but cmd.exe classifies `\\?\C:\...` as a UNC
    // current directory, falls back to the Windows directory, and runs the
    // gate in the wrong place. Normalize only the local-drive prefix at this
    // final process boundary.
    let process_cwd = local_dos_path(&plan.cwd);
    let cwd = wide(process_cwd.as_os_str());
    let mut line = command_line(&plan.executable, &plan.args);
    let environment = environment_block(&process_cwd, plan.path.as_deref())?;
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

fn token_flag(class: TOKEN_INFORMATION_CLASS, label: &str) -> Result<bool> {
    let mut access_handle = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut access_handle) }
        .map_err(|error| EngineError::Backend(format!("failed to open child token: {error}")))?;
    let access_handle = OwnedHandle(access_handle);
    let mut value = 0u32;
    let mut returned = 0u32;
    unsafe {
        GetTokenInformation(
            access_handle.0,
            class,
            Some((&mut value as *mut u32).cast()),
            std::mem::size_of::<u32>() as u32,
            &mut returned,
        )
    }
    .map_err(|error| EngineError::Backend(format!("failed to inspect child {label}: {error}")))?;
    if returned as usize != std::mem::size_of::<u32>() {
        return Err(EngineError::Backend(format!(
            "{label} returned an unexpected byte count"
        )));
    }
    Ok(value != 0)
}

fn is_appcontainer_process() -> Result<bool> {
    token_flag(TokenIsAppContainer, "TokenIsAppContainer")
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
/// Windows CI invoke this private entry point; the protected receipt is the
/// release gate for the enabled production resolver.
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
    let toolchain = root.0.join("toolchain");
    // Keep this under the read-only toolchain root. The disposable profile's
    // SID therefore permits traversal/read but not write; the extra broad
    // grant is the only authority a regular AppContainer would have to write.
    let broad_app_packages = toolchain.join("broad-app-packages");
    let trusted_git = repo.join(".git");
    let worktree_git = trusted_git.join("worktrees").join("self-test");
    for path in [
        &mission,
        &worktree,
        &scratch,
        &outside,
        &real_checkout,
        &toolchain,
        &broad_app_packages,
        &worktree_git,
        &kranz_dir,
    ] {
        std::fs::create_dir_all(path).map_err(|error| {
            EngineError::Backend(format!("failed to create {}: {error}", path.display()))
        })?;
    }
    // Make the sibling root deliberately accessible to the broad principal
    // carried by regular AppContainers. The hostile write can stay denied
    // only if the production child is actually LPAC and opts out of that
    // ambient group; a plain AppContainer must fail this receipt.
    {
        let broad_acl = snapshot_dacl(&broad_app_packages)?;
        let mut any_package = well_known_sid(WinBuiltinAnyPackageSid, "ALL APPLICATION PACKAGES")?;
        let broad_appcontainer_grant = AclChange {
            path: broad_app_packages.clone(),
            permissions: FILE_GENERIC_READ.0
                | FILE_GENERIC_WRITE.0
                | FILE_GENERIC_EXECUTE.0
                | FILE_DELETE_CHILD.0
                | DELETE.0,
            inherit: true,
            mode: AclMode::Grant,
        };
        let _guard = DaclMutationGuard::acquire()?;
        apply_acl_change(
            &broad_appcontainer_grant,
            PSID(any_package.as_mut_ptr().cast()),
            broad_acl.handle.0,
        )?;
    }
    let authority_file = kranz_dir.join("serve.token");
    std::fs::write(&authority_file, "must-not-cross")?;
    let real_source = real_checkout.join("source.rs");
    std::fs::write(&real_source, "must-not-read")?;
    let shared_git_marker = trusted_git.join("inspection-marker");
    std::fs::write(&shared_git_marker, "git-readable")?;
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", worktree_git.display()),
    )?;
    std::fs::write(worktree_git.join("commondir"), "../..\n")?;

    let source_executable = std::env::current_exe().map_err(|error| {
        EngineError::Backend(format!("failed to locate self-test executable: {error}"))
    })?;
    // Exercise the production launcher without recursively changing the ACL
    // of Cargo's large target directory. Normal Node/Rust toolchain coverage
    // belongs to the phase-5 gate/overhead receipt; this hostile fixture needs
    // only a real executable in a small read/execute-only toolchain root.
    let executable = toolchain.join("kranz-appcontainer-self-test.exe");
    std::fs::copy(&source_executable, &executable).map_err(|error| {
        EngineError::Backend(format!(
            "failed to copy self-test executable from {} to {}: {error}",
            source_executable.display(),
            executable.display()
        ))
    })?;
    let toolchain_denied_write = toolchain.join(format!(
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
        broad_app_packages_write: broad_app_packages.join("must-not-write.txt"),
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
    let mut env = crate::agent_env::sanitized_child_env(&scratch.join("home"), &[]);
    // This phase-4 fixture is not a Cargo contract. Remove the real toolchain
    // roots from its otherwise production-shaped cleared environment so the
    // proof changes ACLs only under its disposable root. The phase-5 receipt
    // separately owns normal Rust/Node gates and their measured overhead.
    env.remove("CARGO_HOME");
    env.remove("RUSTUP_HOME");
    env.remove("NPM_CONFIG_CACHE");
    if let Some(system_root) = env_value_ci(&env, "SystemRoot").map(ToOwned::to_owned) {
        env.insert(
            "PATH".to_string(),
            PathBuf::from(system_root)
                .join("System32")
                .to_string_lossy()
                .into_owned(),
        );
    } else {
        env.remove("PATH");
    }
    let before = snapshot_dacl(&worktree)?;
    // Both leases touch the same worktree, toolchain, scratch, and Git roots.
    // Dropping the first must remove only its own SID, leaving the second
    // launch functional; dropping the second must recover the exact baseline.
    let overlapping = prepare_launch(
        &inputs,
        &executable,
        &[INTERNAL_HOSTILE_CHILD_ARG.to_string()],
        &env,
    )?;
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
    drop(overlapping.lease);
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
    receipt.overlapping_lease_safe = true;
    receipt.dacl_restored = before.acl == after.acl;
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", outside.display()),
    )?;
    receipt.tampered_git_pointer_refused = prepare_launch(
        &inputs,
        &executable,
        &[INTERNAL_HOSTILE_CHILD_ARG.to_string()],
        &env,
    )
    .is_err();
    let _ = std::fs::remove_file(&toolchain_denied_write);
    let all_passed = receipt.token_is_appcontainer
        && receipt.all_application_packages_denied
        && receipt.toolchain_read
        && receipt.toolchain_write_denied
        && receipt.worktree_write
        && receipt.scratch_write
        && receipt.outside_write_denied
        && receipt.authority_read_denied
        && receipt.real_checkout_read_denied
        && receipt.shared_git_read
        && receipt.overlapping_lease_safe
        && receipt.tampered_git_pointer_refused
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

fn run_gate_sample(
    worktree: &Path,
    command: &str,
    marker: &str,
    policy: &crate::command_exec::MergeGatePolicy,
    appcontainer: bool,
    sample: &str,
) -> Result<f64> {
    let started = std::time::Instant::now();
    let (code, output) = if appcontainer {
        crate::command_exec::run_bounded_gate_command_sandboxed_with_code(worktree, command, policy)
    } else {
        let (ok, output) = crate::command_exec::run_bounded_gate_command(worktree, command);
        (Some(i32::from(!ok)), output)
    };
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let posture = if appcontainer {
        "AppContainer"
    } else {
        "unwrapped"
    };
    // Progress goes to stderr as each sample retires. The receipt itself is
    // the child's stdout, so a caller can still parse it; without this line a
    // slow or wedged sample is invisible until the whole self test returns,
    // which is exactly when a CI step timeout has already killed it.
    eprintln!(
        "gate sample: posture={posture} {sample} elapsed_ms={elapsed_ms:.0} command={command}"
    );
    if code != Some(0) || !output.contains(marker) {
        let diagnostics = appcontainer
            .then(|| gate_failure_diagnostics(worktree, policy))
            .unwrap_or_default();
        return Err(EngineError::Backend(format!(
            "{posture} normal gate {sample} failed with exit {code:?} or omitted {marker}: \
             {output:?}{diagnostics}"
        )));
    }
    Ok(elapsed_ms)
}

fn gate_failure_diagnostics(
    worktree: &Path,
    policy: &crate::command_exec::MergeGatePolicy,
) -> String {
    let mut diagnostics = String::from("; bounded AppContainer diagnostics:");
    for (label, command) in [
        ("cmd", "echo kranz-cmd-probe"),
        ("where-node", "where node"),
        ("node-version", "node --version"),
        ("where-npm", "where npm"),
        ("npm-version", "npm --version"),
        ("node-script", "node node-gate.js"),
    ] {
        let (code, output) = crate::command_exec::run_bounded_gate_command_sandboxed_with_code(
            worktree, command, policy,
        );
        diagnostics.push_str(&format!(" {label}=({code:?}, {output:?})"));
    }
    diagnostics
}

fn median_ms(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

fn measure_gate(
    worktree: &Path,
    command: &str,
    marker: &str,
    policy: &crate::command_exec::MergeGatePolicy,
) -> Result<GateTimingReceipt> {
    // Warm both postures before retaining samples. Alternate their order so
    // runner drift does not systematically favor either side.
    run_gate_sample(worktree, command, marker, policy, false, "warm-up")?;
    run_gate_sample(worktree, command, marker, policy, true, "warm-up")?;

    let mut off_samples_ms = Vec::with_capacity(GATE_OVERHEAD_REPETITIONS);
    let mut appcontainer_samples_ms = Vec::with_capacity(GATE_OVERHEAD_REPETITIONS);
    for index in 0..GATE_OVERHEAD_REPETITIONS {
        if index % 2 == 0 {
            off_samples_ms.push(run_gate_sample(
                worktree,
                command,
                marker,
                policy,
                false,
                &format!("sample {}", index + 1),
            )?);
            appcontainer_samples_ms.push(run_gate_sample(
                worktree,
                command,
                marker,
                policy,
                true,
                &format!("sample {}", index + 1),
            )?);
        } else {
            appcontainer_samples_ms.push(run_gate_sample(
                worktree,
                command,
                marker,
                policy,
                true,
                &format!("sample {}", index + 1),
            )?);
            off_samples_ms.push(run_gate_sample(
                worktree,
                command,
                marker,
                policy,
                false,
                &format!("sample {}", index + 1),
            )?);
        }
    }
    let off_median_ms = median_ms(&off_samples_ms);
    let appcontainer_median_ms = median_ms(&appcontainer_samples_ms);
    let overhead_ms = appcontainer_median_ms - off_median_ms;
    let overhead_percent = overhead_ms / off_median_ms * 100.0;
    Ok(GateTimingReceipt {
        command: command.to_string(),
        repetitions: GATE_OVERHEAD_REPETITIONS,
        off_samples_ms,
        appcontainer_samples_ms,
        off_median_ms,
        appcontainer_median_ms,
        overhead_ms,
        overhead_percent,
        within_target: overhead_percent <= GATE_OVERHEAD_TARGET_PERCENT,
    })
}

/// Exercise ordinary Node and Rust contract commands through the exact
/// production merge-gate wrapper, then retain interleaved warm-cache timing
/// samples against the byte-identical unwrapped runner. This complements the
/// hostile receipt above: neither proof substitutes for the other.
pub fn run_production_gate_self_test() -> std::result::Result<String, String> {
    production_gate_self_test().map_err(|error| error.to_string())
}

fn production_gate_self_test() -> Result<String> {
    let root = SelfTestRoot::create()?;
    let repo = root.0.join("repo");
    let trusted_git = repo.join(".git");
    let worktree_git = trusted_git.join("worktrees").join("phase-5-gate");
    let mission = repo
        .join(".kranz")
        .join("missions")
        .join("m-production-gate-self-test");
    let worktree = root.0.join("worktree");
    let rust_src = worktree.join("rust-gate").join("src");
    for path in [&worktree_git, &mission, &worktree, &rust_src] {
        std::fs::create_dir_all(path).map_err(|error| {
            EngineError::Backend(format!("failed to create {}: {error}", path.display()))
        })?;
    }
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", worktree_git.display()),
    )?;
    std::fs::write(worktree_git.join("commondir"), "../..\n")?;
    std::fs::write(repo.join(".kranz").join("serve.token"), "must-not-cross")?;

    // GitHub's hosted Node image lives under a host-owned protected toolcache.
    // The hostile receipt above intentionally proves that LPAC cannot execute
    // arbitrary host resources merely because the operator can. Stage the
    // exact runtime once inside this disposable worktree so phase 5 measures
    // the production wrapper around an ordinary Node command without widening
    // the runner's host-toolcache ACLs. A real mission can make the same
    // workspace-contract choice for a tool whose host ACL is not LPAC-ready.
    let ambient: HashMap<String, String> = std::env::vars().collect();
    let node_source = find_on_path(Path::new("node"), &ambient).ok_or_else(|| {
        EngineError::Backend("normal-gate receipt could not resolve node on PATH".to_string())
    })?;
    let node = worktree.join("node.exe");
    let mut source = std::fs::File::open(&node_source).map_err(|error| {
        EngineError::Backend(format!(
            "failed to open Node runtime {} for staging: {error}",
            node_source.display()
        ))
    })?;
    // Create and stream rather than CopyFile: the new file must inherit the
    // disposable worktree DACL, never preserve a protected host-toolcache
    // descriptor that the LPAC token cannot satisfy.
    let mut staged = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&node)
        .map_err(|error| {
            EngineError::Backend(format!(
                "failed to create staged Node runtime {}: {error}",
                node.display()
            ))
        })?;
    std::io::copy(&mut source, &mut staged).map_err(|error| {
        EngineError::Backend(format!(
            "failed to stream Node runtime {} into {}: {error}",
            node_source.display(),
            node.display()
        ))
    })?;
    staged.flush().map_err(|error| {
        EngineError::Backend(format!(
            "failed to flush staged Node runtime {}: {error}",
            node.display()
        ))
    })?;
    // Windows maps an executable image with FILE_SHARE_READ | FILE_SHARE_DELETE,
    // so a surviving write handle makes the loader fail the spawn with
    // ERROR_SHARING_VIOLATION ("the process cannot access the file because it
    // is being used by another process") instead of running the gate. Close
    // both staging handles before the first sample executes the staged runtime.
    drop(staged);
    drop(source);
    std::fs::write(
        worktree.join("node-gate.js"),
        r#"const assert = require('node:assert/strict');
let checksum = 0;
for (let i = 0; i < 100000; i += 1) checksum = (checksum + i) >>> 0;
assert.equal(checksum, 704982704);
setTimeout(() => console.log('kranz-node-gate-ok'), 750);
"#,
    )?;
    std::fs::write(
        worktree.join("rust-gate").join("Cargo.toml"),
        "[package]\nname = \"kranz-windows-gate-receipt\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    std::fs::write(
        rust_src.join("lib.rs"),
        r#"#[cfg(test)]
mod tests {
    #[test]
    fn normal_rust_gate() {
        let values: Vec<u64> = (0..100_000).collect();
        assert_eq!(values.iter().sum::<u64>(), 4_999_950_000);
        std::thread::sleep(std::time::Duration::from_millis(750));
        println!("kranz-rust-gate-ok");
    }
}
"#,
    )?;

    let policy = crate::command_exec::MergeGatePolicy {
        sandbox: crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::FsNet,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: Vec::new(),
            egress: Vec::new(),
        },
        mission_dir: mission,
    };
    eprintln!(
        "gate self test: staged runtime at {}; measuring the node gate",
        node.display()
    );
    let node = measure_gate(
        &worktree,
        r#".\node.exe node-gate.js"#,
        "kranz-node-gate-ok",
        &policy,
    )?;
    eprintln!("gate self test: node gate retired; measuring the rust gate");
    let rust = measure_gate(
        &worktree,
        "cargo test --quiet --manifest-path rust-gate/Cargo.toml -- --nocapture",
        "kranz-rust-gate-ok",
        &policy,
    )?;
    let receipt = ProductionGateReceipt {
        host: crate::sandbox_windows::probe(),
        enforcement: "fs+net",
        provider: "process/AppContainer-LPAC",
        overhead_target_percent: GATE_OVERHEAD_TARGET_PERCENT,
        node,
        rust,
    };
    let rendered = serde_json::to_string(&receipt).map_err(|error| {
        EngineError::Backend(format!("failed to render normal-gate receipt: {error}"))
    })?;
    if !receipt.node.within_target || !receipt.rust.within_target {
        return Err(EngineError::Backend(format!(
            "AppContainer normal-gate overhead exceeded the {GATE_OVERHEAD_TARGET_PERCENT:.1}% target: {rendered}"
        )));
    }
    Ok(rendered)
}

pub fn internal_gate_self_test_requested() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == INTERNAL_GATE_SELF_TEST_ARG)
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
        // Windows Server 2025 returns ERROR_INVALID_PARAMETER for the
        // TokenIsLessPrivilegedAppContainer information class even though it
        // accepts the process-creation opt-out attribute. Prove the security
        // property directly: this path grants ALL APPLICATION PACKAGES, so a
        // regular AppContainer can write it while an LPAC cannot.
        all_application_packages_denied: std::fs::write(
            &manifest.broad_app_packages_write,
            "escape",
        )
        .is_err(),
        toolchain_read: std::fs::metadata(&manifest.executable).is_ok(),
        toolchain_write_denied: std::fs::write(&manifest.toolchain_denied_write, "escape").is_err(),
        worktree_write: std::fs::write(&manifest.worktree_write, "allowed").is_ok(),
        scratch_write: std::fs::write(&manifest.scratch_write, "allowed").is_ok(),
        outside_write_denied: std::fs::write(&manifest.outside_write, "escape").is_err(),
        authority_read_denied: std::fs::read(&manifest.authority_file).is_err(),
        real_checkout_read_denied: std::fs::read(&manifest.real_source).is_err(),
        shared_git_read: std::fs::read_to_string(&manifest.shared_git_marker)
            .is_ok_and(|value| value == "git-readable"),
        // The parent records overlap and pointer assertions after this child
        // exits; neither can be observed from inside the hostile process.
        overlapping_lease_safe: false,
        tampered_git_pointer_refused: false,
        // LPAC can deny Winsock initialization itself (WSAStartup returns
        // WSASYSCALLFAILURE on hosted Windows Server) before std can return an
        // io::Error from connect_timeout. Both outcomes prove this child
        // cannot reach even the parent-owned loopback listener; only an
        // established connection fails the receipt.
        network_denied: match std::panic::catch_unwind(|| {
            TcpStream::connect_timeout(&manifest.loopback_addr, std::time::Duration::from_secs(2))
        }) {
            Ok(Ok(_)) => false,
            Ok(Err(_)) | Err(_) => true,
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_environment_carries_the_gate_drive_current_directory() {
        for cwd in [
            Path::new(r"D:\gate\worktree"),
            Path::new(r"\\?\D:\gate\worktree"),
        ] {
            let (key, value) = drive_current_directory_variable(cwd)
                .expect("a drive-qualified Windows path has a pseudo environment variable");
            assert_eq!(key, OsString::from("=D:"));
            assert_eq!(value, OsString::from(r"D:\gate\worktree"));
        }
        assert!(drive_current_directory_variable(Path::new(r"\\server\share\gate")).is_none());
    }

    #[test]
    fn containment_comparisons_see_through_verbatim_prefixes_and_case() {
        let program_files =
            PathBuf::from(std::env::var_os("PROGRAMFILES").expect("Windows sets PROGRAMFILES"));
        let canonical = std::fs::canonicalize(&program_files)
            .expect("the Program Files root canonicalizes")
            .join("nodejs")
            .join("node.exe");
        // The canonical form is what push_entry_point actually tests, and it
        // is the form that used to answer NO here.
        assert!(system_managed_path(&canonical));
        assert!(system_managed_path(&program_files.join("nodejs")));
        assert!(path_contains(
            Path::new(r"C:\Program Files"),
            Path::new(r"c:\program files\nodejs")
        ));
        // A sibling that merely shares a name prefix is still outside.
        assert!(!path_contains(
            Path::new(r"C:\Program Files"),
            Path::new(r"C:\Program Files Extra\tool.exe")
        ));
        let mut snapshots = BTreeMap::new();
        snapshots.insert(comparable_path(Path::new(r"C:\Gate\worktree")), 1);
        snapshots.insert(comparable_path(Path::new(r"\\?\c:\gate\worktree")), 2);
        assert_eq!(
            snapshots.len(),
            1,
            "one physical DACL must have one cleanup capability"
        );
    }

    #[test]
    fn process_current_directory_strips_only_a_verbatim_disk_prefix() {
        assert_eq!(
            local_dos_path(Path::new(r"\\?\D:\gate\worktree")),
            Path::new(r"D:\gate\worktree")
        );
        assert_eq!(
            local_dos_path(Path::new(r"\\?\UNC\server\share\gate")),
            Path::new(r"\\?\UNC\server\share\gate")
        );
    }

    #[test]
    fn command_line_leaves_cmd_switch_unquoted_and_quotes_command_tail() {
        let encoded = command_line(
            Path::new(r"C:\Windows\System32\cmd.exe"),
            &["/C".to_string(), "set X=1&& echo ok".to_string()],
        );
        let rendered = String::from_utf16(&encoded[..encoded.len() - 1])
            .expect("the command line is valid UTF-16");
        assert_eq!(
            rendered,
            r#""C:\Windows\System32\cmd.exe" /C "set X=1&& echo ok""#
        );
    }

    #[test]
    fn toolchain_entry_points_include_protected_node_npm_and_cargo_files() {
        let root = tempfile::tempdir().expect("temp toolchain root");
        let node_dir = root.path().join("node");
        let npm_bin = node_dir.join("node_modules").join("npm").join("bin");
        let cargo_dir = root.path().join("cargo");
        let rustc_bin = root
            .path()
            .join("rustup")
            .join("toolchains")
            .join("stable-x86_64-pc-windows-msvc")
            .join("bin");
        std::fs::create_dir_all(&npm_bin).expect("npm bin");
        std::fs::create_dir_all(&cargo_dir).expect("cargo dir");
        std::fs::create_dir_all(&rustc_bin).expect("rustc bin");
        for name in ["node.exe", "npm.cmd", "npx.cmd"] {
            std::fs::write(node_dir.join(name), "").expect("node shim");
        }
        std::fs::write(npm_bin.join("npm-cli.js"), "").expect("npm-cli.js");
        std::fs::write(cargo_dir.join("cargo.exe"), "").expect("cargo.exe");
        std::fs::write(rustc_bin.join("rustc.exe"), "").expect("rustc.exe");

        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            std::env::join_paths([&node_dir, &cargo_dir])
                .expect("PATH")
                .to_string_lossy()
                .into_owned(),
        );
        env.insert("PATHEXT".to_string(), ".COM;.EXE;.BAT;.CMD".to_string());
        env.insert(
            "RUSTUP_HOME".to_string(),
            root.path().join("rustup").to_string_lossy().into_owned(),
        );

        let names: Vec<String> = toolchain_entry_points(&env)
            .into_iter()
            .filter_map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        for expected in [
            "node.exe",
            "npm.cmd",
            "npx.cmd",
            "npm-cli.js",
            "cargo.exe",
            "rustc.exe",
        ] {
            assert!(
                names.iter().any(|name| name.eq_ignore_ascii_case(expected)),
                "missing {expected} in {names:?}"
            );
        }
    }

    #[test]
    fn ancestor_directories_stop_before_the_volume_root() {
        let file = Path::new(r"\\?\C:\hostedtoolcache\windows\node\20.0.0\x64\node.exe");
        let ancestors = ancestor_directories(file);
        assert!(
            ancestors
                .iter()
                .any(|path| path.file_name() == Some(std::ffi::OsStr::new("x64"))),
            "{ancestors:?}"
        );
        assert!(
            ancestors
                .iter()
                .any(|path| path.file_name() == Some(std::ffi::OsStr::new("hostedtoolcache"))),
            "{ancestors:?}"
        );
        assert!(
            ancestors.iter().all(|path| !volume_root(path)),
            "{ancestors:?}"
        );
        assert!(volume_root(Path::new(r"\\?\C:\")));
        assert!(volume_root(Path::new(r"C:\")));
    }

    #[test]
    fn environment_block_replaces_path_with_the_contained_search_path() {
        let encoded = environment_block(
            Path::new(r"D:\gate\worktree"),
            Some(r"C:\Windows\System32;D:\node"),
        )
        .expect("environment block");
        let text = String::from_utf16(&encoded).expect("the environment block is valid UTF-16");
        let path = text
            .split('\0')
            .find(|entry| entry.to_ascii_uppercase().starts_with("PATH="));
        assert_eq!(path, Some(r"PATH=C:\Windows\System32;D:\node"));
    }
}
