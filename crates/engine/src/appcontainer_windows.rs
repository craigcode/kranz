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
//! Filesystem authority is granted to a unique AppContainer package SID: per
//! agent session launch, or per resolved engine-gate posture across that
//! posture's contract commands. The parent retains a no-follow handle for every
//! DACL it changes and removes only that SID's ACEs when the launch/posture is
//! reaped or aborted;
//! removing an inheritable parent ACE also removes its inherited copies from
//! descendants.
//! An elevated host step owns the two well-known, non-inheriting drive-root
//! metadata ACEs Windows tools require and reapplies the documented
//! AppContainer descriptor to `\Device\Null` once per boot; the launcher
//! verifies both prerequisites read-only.
//! A bounded host-local mutex serializes those DACL read/modify/write batches,
//! so this composes safely across overlapping launches that share Git/toolchain
//! roots and restores the original descriptor exactly when no unrelated ACL
//! change occurred. The profile name is random and deleted with the same lease.
//! A process crash can leave an inert orphan SID ACE, but no other AppContainer
//! principal can use it.

use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, LocalFree, DUPLICATE_SAME_ACCESS,
    ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
    GetNamedSecurityInfoW, GetSecurityInfo, SetEntriesInAclW, SetNamedSecurityInfoW,
    SetSecurityInfo, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, SDDL_REVISION_1, SE_FILE_OBJECT,
    TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    AclSizeInformation, CreateWellKnownSid, DeleteAce, DeriveCapabilitySidsFromName, EqualSid,
    FreeSid, GetAce, GetAclInformation, GetKernelObjectSecurity, GetLengthSid,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSidIdentifierAuthority,
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, IsValidAcl,
    SetKernelObjectSecurity, TokenElevation, TokenIsAppContainer, WinBuiltinAnyPackageSid,
    WinCapabilityInternetClientSid, ACCESS_ALLOWED_ACE, ACCESS_DENIED_ACE, ACE_HEADER, ACL,
    ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
    GROUP_SECURITY_INFORMATION, INHERITED_ACE, LABEL_SECURITY_INFORMATION, NO_INHERITANCE,
    OBJECT_INHERIT_ACE, OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
    SE_DACL_PROTECTED, SID_AND_ATTRIBUTES, TOKEN_INFORMATION_CLASS, TOKEN_QUERY,
    UNPROTECTED_DACL_SECURITY_INFORMATION, WELL_KNOWN_SID_TYPE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, ReadFile, BY_HANDLE_FILE_INFORMATION, DELETE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_DATA, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
};
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ,
    FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows::Win32::System::SystemServices::SE_GROUP_ENABLED;
use windows::Win32::System::Threading::{
    CreateMutexW, CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, OpenProcessToken, ReleaseMutex,
    ResumeThread, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
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
const ROOT_METADATA_ACCESS_MASK: u32 = 0x0012_0088;
const NULL_DEVICE_ACCESS_MASK: u32 = 0x0012_01bf;
const REGISTRY_READ_CAPABILITY: &str = "registryRead";
// Source: Microsoft's current AppContainer host-preparation contract.
// https://github.com/microsoft/mxc/blob/main/docs/host-prep.md
const NULL_DEVICE_TARGET_SDDL: &str = "O:BAG:SYD:(A;;GRGWGX;;;WD)(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGX;;;RC)(A;;GRGWGX;;;AC)(A;;GRGWGX;;;S-1-15-2-2)S:(ML;;NW;;;LW)";
const ALL_RESTRICTED_APPLICATION_PACKAGES_SID: &str = "S-1-15-2-2";
const GATE_OVERHEAD_REPETITIONS: usize = 7;
const GATE_WORKLOAD_MILLIS: u64 = 5_000;
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
    /// Absolute root of the already-installed active rustup toolchain. The
    /// contained PATH prefers its real binaries over rustup's protected proxy,
    /// while `RUSTUP_TOOLCHAIN` prevents a fallback proxy launch from refreshing
    /// the operator-owned, read-only `RUSTUP_HOME`.
    #[serde(default)]
    rustup_toolchain: Option<PathBuf>,
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
    worktree_authority_files: Vec<PathBuf>,
    boundary_state_names: Vec<String>,
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
    pub worktree_git_read: bool,
    pub worktree_git_write_denied: bool,
    pub worktree_git_replace_denied: bool,
    pub boundary_state_write_denied: bool,
    pub scratch_write: bool,
    pub outside_write_denied: bool,
    pub authority_read_denied: bool,
    pub worktree_authority_read_denied: bool,
    pub worktree_authority_write_denied: bool,
    pub worktree_authority_create_denied: bool,
    pub worktree_authority_rename_denied: bool,
    pub ordinary_file_delete: bool,
    pub real_checkout_read_denied: bool,
    pub shared_git_read: bool,
    pub overlapping_lease_safe: bool,
    pub tampered_git_pointer_refused: bool,
    pub network_denied: bool,
    pub dacl_restored: bool,
    pub volume_root_dacl_restored: bool,
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

pub(crate) struct PreparedCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// One resolved Windows gate posture owns one disposable profile and its ACL
/// lease across all commands in that validation/final-gate context. Clones
/// held by in-flight wrapped commands keep cleanup from racing their child.
#[derive(Clone, Debug)]
pub(crate) struct AppContainerLaunchContext(Arc<Mutex<Option<AppContainerLease>>>);

#[derive(Debug)]
struct DaclSnapshot {
    path: PathBuf,
    acl: Option<Vec<u8>>,
    protected: bool,
    handle: OwnedHandle,
}

// File handles are kernel-object references whose access rights and lifetime
// are independent of the thread using them. The snapshot owns its handle and
// only performs synchronous Get/SetSecurityInfo calls during lease cleanup,
// so moving the whole snapshot with an async session is sound. Keep the
// narrower mutex guard non-Send: Win32 mutex ownership is thread-affine.
unsafe impl Send for DaclSnapshot {}

#[derive(Debug)]
struct BoundaryBaseline {
    originally_protected: bool,
    acl: Vec<u8>,
    _mapping: OwnedHandle,
}

// Only the kernel handle and an owned byte copy cross threads. Mapped views
// are temporary and accessed synchronously under DaclMutationGuard.
unsafe impl Send for BoundaryBaseline {}

struct BoundaryView(MEMORY_MAPPED_VIEW_ADDRESS);

impl Drop for BoundaryView {
    fn drop(&mut self) {
        let _ = unsafe { UnmapViewOfFile(self.0) };
    }
}

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
#[derive(Debug)]
pub(crate) struct AppContainerLease {
    profile_name: String,
    rustup_toolchain: Option<PathBuf>,
    original_dacls: Vec<DaclSnapshot>,
    snapshot_indices: BTreeMap<PathBuf, usize>,
    boundary_protection: BTreeMap<PathBuf, BoundaryBaseline>,
    applied_changes: HashSet<(PathBuf, u32, bool, AclMode)>,
    plan_paths: Vec<PathBuf>,
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
                    if let Err(error) = remove_sid_aces(snapshot, sid.0) {
                        #[cfg(test)]
                        eprintln!(
                            "AppContainer ACE cleanup {}: {error}",
                            snapshot.path.display()
                        );
                        tracing::error!(path = %snapshot.path.display(), error = %error,
                            "failed to remove AppContainer ACEs from a DACL");
                    }
                }
                // Parent grants are gone before an authority boundary can
                // inherit again. Other leases keep their own marker and pin.
                for snapshot in &self.original_dacls {
                    if let Some(original) = self
                        .boundary_protection
                        .get(&comparable_path(&snapshot.path))
                    {
                        if let Err(error) = restore_boundary_inheritance(
                            snapshot,
                            original.originally_protected,
                            &original.acl,
                        ) {
                            #[cfg(test)]
                            eprintln!(
                                "AppContainer inheritance cleanup {}: {error}",
                                snapshot.path.display()
                            );
                            tracing::error!(path = %snapshot.path.display(), error = %error,
                                "failed to restore AppContainer boundary inheritance");
                        }
                    }
                }
                // Retire the final shared baseline before releasing the
                // mutex, so the next lease starts from the restored DACL.
                self.boundary_protection.clear();
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
        for path in self.plan_paths.drain(..) {
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

struct LocalSidArray {
    sids: *mut PSID,
    count: u32,
}

impl Drop for LocalSidArray {
    fn drop(&mut self) {
        if self.sids.is_null() {
            return;
        }
        unsafe {
            for sid in std::slice::from_raw_parts(self.sids, self.count as usize) {
                if !sid.0.is_null() {
                    LocalFree(Some(HLOCAL(sid.0)));
                }
            }
            LocalFree(Some(HLOCAL(self.sids.cast())));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaunchCapability {
    RegistryRead,
    InternetClient,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

fn create_profile(allow_network: bool) -> Result<(String, OwnedSid)> {
    let profile_name = format!("kranz.production.{}", uuid::Uuid::new_v4().simple());
    let name = wide(&profile_name);
    let display = wide("Kranz contained process");
    let description = wide("Disposable Kranz AppContainer profile");
    let mut capability_storage = launch_capability_storage(allow_network)?;
    let capabilities = capability_attributes(&mut capability_storage);
    let sid = unsafe {
        CreateAppContainerProfile(
            PCWSTR(name.as_ptr()),
            PCWSTR(display.as_ptr()),
            PCWSTR(description.as_ptr()),
            Some(&capabilities),
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!("failed to create AppContainer profile: {error}"))
    })?;
    Ok((profile_name, OwnedSid(sid)))
}

fn snapshot_dacl(path: &Path) -> Result<DaclSnapshot> {
    snapshot_dacl_with_pin(path, false)
}

fn snapshot_dacl_with_pin(path: &Path, pin: bool) -> Result<DaclSnapshot> {
    let path_wide = wide(path.as_os_str());
    // Metadata-only opens do not participate in Windows sharing checks.
    // FILE_READ_DATA (FILE_LIST_DIRECTORY on directories) makes the retained
    // no-share-delete handle a real replacement pin; no contents are read.
    let access = READ_CONTROL.0 | WRITE_DAC.0 | if pin { FILE_READ_DATA.0 } else { 0 };
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            access,
            FILE_SHARE_READ
                | FILE_SHARE_WRITE
                | if pin {
                    Default::default()
                } else {
                    FILE_SHARE_DELETE
                },
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map(OwnedHandle)
    .map_err(|error| {
        // A drive root is the one path here an operator cannot fix by editing
        // a config: it needs the one-time elevated host preparation. Name that
        // command, otherwise the first Windows operator sees only
        // "Access is denied. (0x80070005)" and has nowhere to go.
        let remedy = if path.parent().is_none() {
            format!(
                "; run the one-time elevated host preparation first: \
                 kranz sandbox-prepare --target {}",
                path.display()
            )
        } else {
            String::new()
        };
        EngineError::Backend(format!(
            "failed to retain no-follow DACL capability for {}: {error}{remedy}",
            path.display()
        ))
    })?;
    snapshot_dacl_handle(path, handle, pin)
}

// `path` is a diagnostic label only. Cleanup must keep using the retained
// kernel object even if an ancestor has moved since preparation.
fn snapshot_dacl_handle(path: &Path, handle: OwnedHandle, pin: bool) -> Result<DaclSnapshot> {
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
    let mut control = 0u16;
    let mut revision = 0u32;
    unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }.map_err(
        |error| EngineError::Backend(format!("failed to inspect DACL protection: {error}")),
    )?;
    if pin {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle.0, &mut info) }.map_err(|error| {
            EngineError::Backend(format!("failed to inspect pinned boundary: {error}"))
        })?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(EngineError::Backend(format!(
                "AppContainer boundary {} is a reparse point",
                path.display()
            )));
        }
    }
    let acl = if acl.is_null() {
        None
    } else {
        let len = unsafe { (*acl).AclSize as usize };
        Some(unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), len) }.to_vec())
    };
    Ok(DaclSnapshot {
        path: path.to_path_buf(),
        acl,
        protected: control & SE_DACL_PROTECTED.0 != 0,
        handle,
    })
}

fn dacl_has_root_metadata_ace(acl: *mut ACL, sid: PSID) -> Result<bool> {
    if acl.is_null() {
        return Ok(false);
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|error| EngineError::Backend(format!("failed to inspect drive-root DACL: {error}")))?;

    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    for index in 0..info.AceCount {
        let mut raw = null_mut();
        unsafe { GetAce(acl, index, &mut raw) }.map_err(|error| {
            EngineError::Backend(format!("failed to read drive-root DACL ACE: {error}"))
        })?;
        let header = unsafe { &*raw.cast::<ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0 {
            continue;
        }
        let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Mask != ROOT_METADATA_ACCESS_MASK {
            continue;
        }
        let trustee = PSID((&ace.SidStart as *const u32).cast_mut().cast());
        if unsafe { EqualSid(trustee, sid) }.is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, PartialEq, Eq)]
enum RootMetadataAceState {
    Missing,
    Exact,
    Conflicting { mask: u32, flags: u8 },
}

fn root_metadata_ace_state(acl: *mut ACL, sid: PSID) -> Result<RootMetadataAceState> {
    if acl.is_null() {
        return Err(EngineError::Backend(
            "refusing AppContainer host preparation on a drive root with a null DACL".to_string(),
        ));
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|error| EngineError::Backend(format!("failed to inspect drive-root DACL: {error}")))?;

    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    let mut found_exact = false;
    for index in 0..info.AceCount {
        let mut raw = null_mut();
        unsafe { GetAce(acl, index, &mut raw) }.map_err(|error| {
            EngineError::Backend(format!("failed to read drive-root DACL ACE: {error}"))
        })?;
        let header = unsafe { &*raw.cast::<ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags & INHERITED_ACE.0 as u8 != 0
        {
            continue;
        }
        let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
        let trustee = PSID((&ace.SidStart as *const u32).cast_mut().cast());
        if unsafe { EqualSid(trustee, sid) }.is_err() {
            continue;
        }
        if ace.Mask == ROOT_METADATA_ACCESS_MASK && ace.Header.AceFlags == 0 {
            found_exact = true;
        } else {
            return Ok(RootMetadataAceState::Conflicting {
                mask: ace.Mask,
                flags: ace.Header.AceFlags,
            });
        }
    }
    Ok(if found_exact {
        RootMetadataAceState::Exact
    } else {
        RootMetadataAceState::Missing
    })
}

fn validate_host_preparation_root(root: &Path) -> Result<()> {
    let rendered = root.to_string_lossy();
    let bytes = rendered.as_bytes();
    if bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        Ok(())
    } else {
        Err(EngineError::Backend(format!(
            "AppContainer host preparation target must be a literal local drive root (X:\\): {}",
            root.display()
        )))
    }
}

fn apply_named_root_metadata_ace(root: &Path, sid: PSID, label: &str) -> Result<bool> {
    eprintln!(
        "AppContainer host preparation: inspecting {label} on {}",
        root.display()
    );
    let root_wide = wide(root.as_os_str());
    let mut old_acl: *mut ACL = null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    win32(unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(root_wide.as_ptr()),
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
    eprintln!(
        "AppContainer host preparation: inspected {label} on {}",
        root.display()
    );
    match root_metadata_ace_state(old_acl, sid)? {
        RootMetadataAceState::Exact => {
            eprintln!(
                "AppContainer host preparation: {label} is already exact on {}",
                root.display()
            );
            return Ok(false);
        }
        RootMetadataAceState::Conflicting { mask, flags } => {
            return Err(EngineError::Backend(format!(
                "{} already has a conflicting explicit allow ACE for {label}: mask=0x{mask:08x}, flags=0x{flags:02x}; refusing to merge rights",
                root.display()
            )));
        }
        RootMetadataAceState::Missing => {}
    }

    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: ROOT_METADATA_ACCESS_MASK,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: PWSTR(sid.0.cast()),
            ..Default::default()
        },
    };
    let mut new_acl: *mut ACL = null_mut();
    win32(unsafe { SetEntriesInAclW(Some(&[entry]), Some(old_acl), &mut new_acl) })?;
    if new_acl.is_null() {
        return Err(EngineError::Backend(
            "SetEntriesInAclW returned a null drive-root DACL".to_string(),
        ));
    }
    let _new_acl = LocalAllocation(HLOCAL(new_acl.cast()));
    eprintln!(
        "AppContainer host preparation: applying {label} on {}",
        root.display()
    );
    win32(unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(root_wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl),
            None,
        )
    })?;
    eprintln!(
        "AppContainer host preparation: applied {label} on {}",
        root.display()
    );
    Ok(true)
}

/// Persistent, elevated host setup for one local drive root. This deliberately
/// uses Microsoft's `GetNamedSecurityInfoW` -> `SetEntriesInAclW` ->
/// `SetNamedSecurityInfoW` sequence rather than the managed `Set-Acl` path,
/// which can walk the drive's descendant tree even for non-inheriting ACEs.
pub(crate) fn prepare_appcontainer_host(root: &Path) -> Result<bool> {
    validate_host_preparation_root(root)?;
    eprintln!(
        "AppContainer host preparation: validated {}",
        root.display()
    );
    if !token_flag(TokenElevation, "TokenElevation")? {
        return Err(EngineError::Backend(
            "AppContainer host preparation requires an elevated Windows token; relaunch PowerShell as Administrator"
                .to_string(),
        ));
    }
    eprintln!(
        "AppContainer host preparation: elevation verified for {}",
        root.display()
    );
    let _guard = DaclMutationGuard::acquire()?;
    eprintln!(
        "AppContainer host preparation: mutation lock acquired for {}",
        root.display()
    );
    let mut any_package = string_sid("S-1-15-2-1", "ALL APPLICATION PACKAGES")?;
    let mut restricted = string_sid(
        ALL_RESTRICTED_APPLICATION_PACKAGES_SID,
        "ALL RESTRICTED APPLICATION PACKAGES",
    )?;
    eprintln!(
        "AppContainer host preparation: package SIDs built for {}",
        root.display()
    );
    let changed_any = apply_named_root_metadata_ace(
        root,
        PSID(any_package.as_mut_ptr().cast()),
        "ALL APPLICATION PACKAGES (S-1-15-2-1)",
    )?;
    let changed_restricted = apply_named_root_metadata_ace(
        root,
        PSID(restricted.as_mut_ptr().cast()),
        "ALL RESTRICTED APPLICATION PACKAGES (S-1-15-2-2)",
    )?;
    eprintln!(
        "AppContainer host preparation: verifying {}",
        root.display()
    );
    verify_volume_root_prepared(root)?;
    Ok(changed_any || changed_restricted)
}

/// Apply the same metadata-only ACEs to the PROFILE PARENT (`C:\Users`).
///
/// The target is DERIVED from `USERPROFILE`, never operator-supplied: this
/// grants on a SYSTEM-owned directory shared by every AppContainer on the
/// machine, so the only reachable target is the one Windows itself defines.
///
/// Needed because module resolution `lstat`s each ancestor. Node's
/// `realpathSync` walks up to `C:\Users` and fails `EPERM` without
/// `FILE_READ_ATTRIBUTES` there, so every Rust/Node merge gate under a normal
/// profile-hosted repository failed closed. Bypass-traverse is not enough: it
/// permits passing THROUGH a directory, not stat-ing the directory itself.
///
/// The mask is the same non-inheriting `0x00120088` used on the drive root —
/// read attributes, read EA, read control, synchronize. It confers no
/// directory listing, no file content, and no write.
#[cfg(windows)]
pub(crate) fn prepare_appcontainer_profile_parent() -> Result<(PathBuf, bool)> {
    let parent = profile_parent().ok_or_else(|| {
        EngineError::Backend(
            "USERPROFILE is unset or has no parent, so the AppContainer profile parent cannot be derived"
                .to_string(),
        )
    })?;
    if volume_root(&parent) {
        // A profile at `C:\craig` would make the parent the drive root, which
        // the drive-root path already covers. Never double-apply.
        return Ok((parent, false));
    }
    if !parent.is_dir() {
        return Err(EngineError::Backend(format!(
            "derived AppContainer profile parent {} is not a directory",
            parent.display()
        )));
    }
    if !token_flag(TokenElevation, "TokenElevation")? {
        return Err(EngineError::Backend(
            "AppContainer profile-parent preparation requires an elevated Windows token; relaunch PowerShell as Administrator"
                .to_string(),
        ));
    }
    let _guard = DaclMutationGuard::acquire()?;
    let mut any_package = string_sid("S-1-15-2-1", "ALL APPLICATION PACKAGES")?;
    let mut restricted = string_sid(
        ALL_RESTRICTED_APPLICATION_PACKAGES_SID,
        "ALL RESTRICTED APPLICATION PACKAGES",
    )?;
    let changed_any = apply_named_root_metadata_ace(
        &parent,
        PSID(any_package.as_mut_ptr().cast()),
        "ALL APPLICATION PACKAGES (S-1-15-2-1)",
    )?;
    let changed_restricted = apply_named_root_metadata_ace(
        &parent,
        PSID(restricted.as_mut_ptr().cast()),
        "ALL RESTRICTED APPLICATION PACKAGES (S-1-15-2-2)",
    )?;
    verify_volume_root_prepared(&parent)?;
    Ok((parent, changed_any || changed_restricted))
}

fn open_null_device(write: bool) -> Result<OwnedHandle> {
    let mut desired_access = GENERIC_READ.0 | READ_CONTROL.0;
    if write {
        desired_access |= WRITE_DAC.0 | WRITE_OWNER.0;
    }
    let path = wide(r"\\.\NUL");
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            desired_access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map(OwnedHandle)
    .map_err(|error| {
        EngineError::Backend(format!(
            "failed to open \\Device\\Null for AppContainer host {}: {error}",
            if write { "preparation" } else { "verification" }
        ))
    })
}

fn read_kernel_dacl(handle: HANDLE) -> Result<Vec<u8>> {
    let mut needed = 0u32;
    if unsafe { GetKernelObjectSecurity(handle, DACL_SECURITY_INFORMATION.0, None, 0, &mut needed) }
        .is_err()
    {
        let error = unsafe { GetLastError() };
        if error != ERROR_INSUFFICIENT_BUFFER {
            return Err(EngineError::Backend(format!(
                "failed to size \\Device\\Null security descriptor: {error:?}"
            )));
        }
    }
    if needed == 0 {
        return Err(EngineError::Backend(
            "\\Device\\Null returned an empty security descriptor".to_string(),
        ));
    }
    let mut bytes = vec![0u8; needed as usize];
    let mut written = 0u32;
    unsafe {
        GetKernelObjectSecurity(
            handle,
            DACL_SECURITY_INFORMATION.0,
            Some(PSECURITY_DESCRIPTOR(bytes.as_mut_ptr().cast())),
            needed,
            &mut written,
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!(
            "failed to read \\Device\\Null security descriptor: {error}"
        ))
    })?;
    bytes.truncate(written as usize);
    Ok(bytes)
}

fn dacl_has_null_device_ace(acl: *mut ACL, sid: PSID) -> Result<bool> {
    if acl.is_null() {
        return Ok(false);
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!("failed to inspect \\Device\\Null DACL: {error}"))
    })?;

    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    for index in 0..info.AceCount {
        let mut raw = null_mut();
        unsafe { GetAce(acl, index, &mut raw) }.map_err(|error| {
            EngineError::Backend(format!("failed to read \\Device\\Null DACL ACE: {error}"))
        })?;
        let header = unsafe { &*raw.cast::<ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0 {
            continue;
        }
        let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
        let trustee = PSID((&ace.SidStart as *const u32).cast_mut().cast());
        if ace.Mask == NULL_DEVICE_ACCESS_MASK && unsafe { EqualSid(trustee, sid) }.is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn verify_null_device_prepared() -> Result<()> {
    let handle = open_null_device(false)?;
    let mut descriptor = read_kernel_dacl(handle.0)?;
    let descriptor = PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast());
    let mut present = BOOL(0);
    let mut defaulted = BOOL(0);
    let mut acl: *mut ACL = null_mut();
    unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
        .map_err(|error| {
            EngineError::Backend(format!("failed to locate \\Device\\Null DACL: {error}"))
        })?;
    if !present.as_bool() {
        return Err(EngineError::Backend(
            "\\Device\\Null has no DACL; refusing AppContainer launch".to_string(),
        ));
    }
    let mut any_package = well_known_sid(WinBuiltinAnyPackageSid, "ALL APPLICATION PACKAGES")?;
    let mut restricted = string_sid(
        ALL_RESTRICTED_APPLICATION_PACKAGES_SID,
        "ALL RESTRICTED APPLICATION PACKAGES",
    )?;
    let any_package_present = dacl_has_null_device_ace(acl, PSID(any_package.as_mut_ptr().cast()))?;
    let restricted_present = dacl_has_null_device_ace(acl, PSID(restricted.as_mut_ptr().cast()))?;
    if any_package_present && restricted_present {
        return Ok(());
    }

    Err(EngineError::Backend(
        "AppContainer host preparation is missing the required \\Device\\Null package ACEs; run scripts/prepare-windows-appcontainer.ps1 once from elevated PowerShell after each reboot"
            .to_string(),
    ))
}

/// Reapply the Windows AppContainer null-device descriptor once per boot.
/// The kernel resets this object at restart; without the two package ACEs,
/// ordinary tools that open `NUL` during startup fail with access denied.
pub(crate) fn prepare_appcontainer_null_device() -> Result<()> {
    if !token_flag(TokenElevation, "TokenElevation")? {
        return Err(EngineError::Backend(
            "AppContainer null-device preparation requires an elevated Windows token; relaunch PowerShell as Administrator"
                .to_string(),
        ));
    }
    let handle = open_null_device(true)?;
    let sddl = wide(NULL_DEVICE_TARGET_SDDL);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!(
            "failed to parse the trusted \\Device\\Null security descriptor: {error}"
        ))
    })?;
    if descriptor.0.is_null() {
        return Err(EngineError::Backend(
            "Windows returned a null parsed \\Device\\Null security descriptor".to_string(),
        ));
    }
    let _descriptor = LocalAllocation(HLOCAL(descriptor.0));
    let info = OWNER_SECURITY_INFORMATION
        | GROUP_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | LABEL_SECURITY_INFORMATION;
    unsafe { SetKernelObjectSecurity(handle.0, info, descriptor) }.map_err(|error| {
        EngineError::Backend(format!(
            "failed to prepare \\Device\\Null for AppContainer tools: {error}"
        ))
    })?;
    verify_null_device_prepared()
}

/// Common Windows tools inspect the local drive root before user code starts.
/// LPAC's restricted-package group must already have Microsoft's minimal,
/// non-inheriting metadata ACE there. This check is read-only and deliberately
/// separate from the unprivileged launcher; host-wide preparation belongs to
/// an elevated, auditable operator step.
fn verify_volume_root_prepared(root: &Path) -> Result<()> {
    let root_wide = wide(root.as_os_str());
    let handle = unsafe {
        CreateFileW(
            PCWSTR(root_wide.as_ptr()),
            READ_CONTROL.0,
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
            "failed to inspect AppContainer host preparation on {}: {error}",
            root.display()
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
    let mut any_package = well_known_sid(WinBuiltinAnyPackageSid, "ALL APPLICATION PACKAGES")?;
    let mut restricted = string_sid(
        ALL_RESTRICTED_APPLICATION_PACKAGES_SID,
        "ALL RESTRICTED APPLICATION PACKAGES",
    )?;
    let any_package_present =
        dacl_has_root_metadata_ace(acl, PSID(any_package.as_mut_ptr().cast()))?;
    let restricted_present = dacl_has_root_metadata_ace(acl, PSID(restricted.as_mut_ptr().cast()))?;
    if any_package_present && restricted_present {
        return Ok(());
    }

    let mut missing = Vec::new();
    if !any_package_present {
        missing.push("S-1-15-2-1");
    }
    if !restricted_present {
        missing.push(ALL_RESTRICTED_APPLICATION_PACKAGES_SID);
    }
    Err(EngineError::Backend(format!(
        "AppContainer host preparation is missing the exact non-inheriting 0x{ROOT_METADATA_ACCESS_MASK:08x} metadata ACE for {} on {}; run scripts/prepare-windows-appcontainer.ps1 -Target '{}' once from elevated PowerShell",
        missing.join(" and "),
        root.display(),
        root.display()
    )))
}

/// Read a path's DACL bytes WITHOUT asking for `WRITE_DAC`.
///
/// [`snapshot_dacl`] requests `READ_CONTROL | WRITE_DAC` because a lease will
/// later restore what it changed. The volume root is never one of those paths:
/// [`ancestor_directories`] deliberately stops before it, and the ordinary
/// launcher only calls [`verify_volume_root_prepared`], which is read-only.
/// The self-test's before/after volume-root comparison is likewise pure
/// evidence — it never restores — so asking for `WRITE_DAC` there demanded an
/// elevated token for a read, and that alone made the M7 receipt unobtainable
/// without elevation on an otherwise correctly prepared host.
fn read_dacl_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    let path_wide = wide(path.as_os_str());
    // SAFETY: `path_wide` is a live NUL-terminated UTF-16 buffer. READ_CONTROL
    // alone is enough to read a security descriptor; the backup/reparse flags
    // match the mutating path so both observe the same object.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            READ_CONTROL.0,
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
            "failed to read the DACL of {}: {error}",
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
    if acl.is_null() {
        return Ok(None);
    }
    let len = unsafe { (*acl).AclSize as usize };
    Ok(Some(
        unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), len) }.to_vec(),
    ))
}

fn remove_sid_aces(snapshot: &DaclSnapshot, sid: PSID) -> Result<()> {
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
    apply_acl_change_security(change, sid, handle, DACL_SECURITY_INFORMATION)
}

fn apply_acl_change_security(
    change: &AclChange,
    sid: PSID,
    handle: HANDLE,
    security: OBJECT_SECURITY_INFORMATION,
) -> Result<()> {
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
            security,
            None,
            None,
            Some(new_acl),
            None,
        )
    })
}

// A non-inheriting, disposable-profile marker records the original protection
// flag for every overlapping lease. It grants no rights. The final lease can
// restore inheritance even when it did not create the boundary; a crashed
// lease leaves it protected, just as orphan profile grants remain inert.
const BOUNDARY_MARKER: u32 = WRITE_DAC.0 | WRITE_OWNER.0;
// Ordinary children inherit DELETE on themselves. DELETE_CHILD on a parent
// would also authorize replacement of sealed children that lack DELETE.
const WRITABLE_ROOT_ACCESS: u32 =
    FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0 | FILE_GENERIC_EXECUTE.0 | DELETE.0;

fn appcontainer_sid(sid: PSID) -> bool {
    unsafe { (*GetSidIdentifierAuthority(sid)).Value == [0, 0, 0, 0, 0, 15] }
}

fn boundary_marker(entry: &ACCESS_ALLOWED_ACE) -> Option<bool> {
    let header = &entry.Header;
    if header.AceType != 1
        || header.AceFlags != 0
        || ![BOUNDARY_MARKER, BOUNDARY_MARKER | DELETE.0].contains(&entry.Mask)
    {
        return None;
    }
    let sid = PSID((&entry.SidStart as *const u32).cast_mut().cast());
    if appcontainer_sid(sid)
        && unsafe { *GetSidSubAuthorityCount(sid) == 8 && *GetSidSubAuthority(sid, 0) == 2 }
    {
        Some(entry.Mask & DELETE.0 == 0)
    } else {
        None
    }
}

fn inspect_boundary_acl(snapshot: &DaclSnapshot, allowed: u32) -> Result<Option<bool>> {
    let bytes = snapshot.acl.as_ref().ok_or_else(|| {
        EngineError::Backend(format!(
            "AppContainer boundary {} has a null DACL",
            snapshot.path.display()
        ))
    })?;
    let acl = bytes.as_ptr().cast::<ACL>();
    let mut original = None;
    for index in 0..unsafe { (*acl).AceCount } {
        let mut ace = null_mut();
        unsafe { GetAce(acl, u32::from(index), &mut ace) }.map_err(|error| {
            EngineError::Backend(format!("failed to inspect boundary ACE: {error}"))
        })?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType > 1 {
            return Err(EngineError::Backend(format!(
                "AppContainer boundary {} has an unsupported ACE type {}",
                snapshot.path.display(),
                header.AceType
            )));
        }
        let entry = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = PSID((&entry.SidStart as *const u32).cast_mut().cast());
        if header.AceType == 0 && appcontainer_sid(sid) && entry.Mask & !allowed != 0 {
            return Err(EngineError::Backend(format!(
                "AppContainer boundary {} already grants package/capability access beyond its protected rights",
                snapshot.path.display()
            )));
        }
        if let Some(value) = boundary_marker(entry) {
            if original.is_some_and(|previous| previous != value) {
                return Err(EngineError::Backend(
                    "conflicting AppContainer boundary lease markers".to_string(),
                ));
            }
            original = Some(value);
        }
    }
    Ok(original)
}

const BOUNDARY_STATE_HEADER: usize = 16;
const BOUNDARY_STATE_SIZE: usize = BOUNDARY_STATE_HEADER + u16::MAX as usize;

fn boundary_state_name(snapshot: &DaclSnapshot) -> Result<String> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(snapshot.handle.0, &mut info) }.map_err(|error| {
        EngineError::Backend(format!("failed to identify boundary object: {error}"))
    })?;
    if info.nFileIndexHigh == 0 && info.nFileIndexLow == 0 {
        return Err(EngineError::Backend(
            "AppContainer boundary has no stable file identity".to_string(),
        ));
    }
    Ok(format!(
        "Local\\Kranz.AppContainer.Boundary.v1.{:08x}.{:08x}{:08x}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    ))
}

// SetSecurityInfo converts inherited ACEs into explicit entries when a DACL
// is protected. Keep their original provenance across processes and lease
// cleanup order. The pagefile-backed object lives until the last lease closes
// its non-inheritable handle; no worker-readable filesystem sidecar is used.
fn retain_boundary_baseline(
    snapshot: &DaclSnapshot,
    prior: Option<bool>,
) -> Result<BoundaryBaseline> {
    let name = wide(OsStr::new(&boundary_state_name(snapshot)?));
    let mapping = unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            BOUNDARY_STATE_SIZE as u32,
            PCWSTR(name.as_ptr()),
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!("failed to retain boundary baseline: {error}"))
    })?;
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let mapping = OwnedHandle(mapping);
    if existed != prior.is_some() {
        return Err(EngineError::Backend(
            "AppContainer boundary baseline and lease markers disagree; orphan ACL recovery is required"
                .to_string(),
        ));
    }
    let access = if existed {
        FILE_MAP_READ
    } else {
        FILE_MAP_WRITE
    };
    let view = unsafe { MapViewOfFile(mapping.0, access, 0, 0, BOUNDARY_STATE_SIZE) };
    if view.Value.is_null() {
        return Err(EngineError::Backend(format!(
            "failed to map boundary baseline: {}",
            windows::core::Error::from_thread()
        )));
    }
    let view = BoundaryView(view);
    let (originally_protected, acl) = if existed {
        let bytes =
            unsafe { std::slice::from_raw_parts(view.0.Value.cast::<u8>(), BOUNDARY_STATE_SIZE) };
        let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        if &bytes[..4] != b"KAC1"
            || bytes[4] > 1
            || len < std::mem::size_of::<ACL>()
            || len > BOUNDARY_STATE_SIZE - BOUNDARY_STATE_HEADER
            || Some(bytes[4] != 0) != prior
        {
            return Err(EngineError::Backend(
                "invalid boundary baseline header".to_string(),
            ));
        }
        let acl = bytes[BOUNDARY_STATE_HEADER..BOUNDARY_STATE_HEADER + len].to_vec();
        if usize::from(u16::from_le_bytes([acl[2], acl[3]])) != len
            || !unsafe { IsValidAcl(acl.as_ptr().cast::<ACL>()) }.as_bool()
        {
            return Err(EngineError::Backend(
                "invalid boundary baseline DACL".to_string(),
            ));
        }
        (bytes[4] != 0, acl)
    } else {
        let acl = snapshot
            .acl
            .as_ref()
            .expect("boundary inspection rejects null DACLs");
        let mut bytes = vec![0u8; BOUNDARY_STATE_HEADER + acl.len()];
        bytes[..4].copy_from_slice(b"KAC1");
        bytes[4] = u8::from(snapshot.protected);
        bytes[8..12].copy_from_slice(&(acl.len() as u32).to_le_bytes());
        bytes[BOUNDARY_STATE_HEADER..].copy_from_slice(acl);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), view.0.Value.cast::<u8>(), bytes.len());
        }
        (snapshot.protected, acl.clone())
    };
    Ok(BoundaryBaseline {
        originally_protected,
        acl,
        _mapping: mapping,
    })
}

fn restore_inherited_ace_flags(current: &mut [u8], original: &[u8]) -> Result<()> {
    let inherited = INHERITED_ACE.0 as u8;
    let entries = |bytes: &[u8]| -> Result<Vec<(usize, usize)>> {
        let acl = bytes.as_ptr().cast::<ACL>();
        if bytes.len() < std::mem::size_of::<ACL>()
            || usize::from(u16::from_le_bytes([bytes[2], bytes[3]])) != bytes.len()
            || !unsafe { IsValidAcl(acl) }.as_bool()
        {
            return Err(EngineError::Backend("invalid restoration DACL".to_string()));
        }
        let mut result = Vec::new();
        for index in 0..unsafe { (*acl).AceCount } {
            let mut ace = null_mut();
            unsafe { GetAce(acl, u32::from(index), &mut ace) }.map_err(|error| {
                EngineError::Backend(format!("failed to inspect restoration ACE: {error}"))
            })?;
            let offset = ace as usize - bytes.as_ptr() as usize;
            let size = usize::from(unsafe { (*ace.cast::<ACE_HEADER>()).AceSize });
            result.push((offset, size));
        }
        Ok(result)
    };
    let current_entries = entries(current)?;
    let mut original_entries = entries(original)?;
    // Preserve original explicit entries before matching inherited copies,
    // including a redundant explicit rule identical to its inherited peer.
    original_entries.sort_by_key(|(offset, _)| original[offset + 1] & inherited != 0);
    let mut matched = HashSet::new();
    for (offset, size) in original_entries {
        let old = &original[offset..offset + size];
        for &(current_offset, current_size) in &current_entries {
            if current_size != size || matched.contains(&current_offset) {
                continue;
            }
            let now = &current[current_offset..current_offset + size];
            if old[0] == now[0]
                && old[1] & !inherited == now[1] & !inherited
                && old[2..] == now[2..]
            {
                matched.insert(current_offset);
                if old[1] & inherited != 0 {
                    current[current_offset + 1] |= inherited;
                }
                break;
            }
        }
    }
    Ok(())
}

fn prepare_acl_boundary(
    lease: &mut AppContainerLease,
    sid: PSID,
    path: &Path,
    allowed: u32,
) -> Result<()> {
    let key = comparable_path(path);
    if lease.boundary_protection.contains_key(&key) {
        return Ok(());
    }
    // Pin the object throughout the lease in addition to excluding package
    // delete rights on both this object and its writable parent.
    let snapshot = snapshot_dacl_with_pin(path, true)?;
    let prior = inspect_boundary_acl(&snapshot, allowed)?;
    if prior.is_some() && !snapshot.protected {
        return Err(EngineError::Backend(
            "AppContainer boundary lost its active inheritance protection".to_string(),
        ));
    }
    let original = prior.unwrap_or(snapshot.protected);
    if path.is_dir() {
        let mut directories = vec![path.to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory)? {
                let child = entry?.path();
                // A child junction could point back into the writable tree,
                // and pre-existing broad package grants could bypass the
                // namespace through a known child path. Reject both shapes.
                let child_acl = snapshot_dacl_with_pin(&child, true)?;
                inspect_boundary_acl(&child_acl, allowed)?;
                if child.is_dir() {
                    directories.push(child);
                }
            }
        }
    }
    let baseline = retain_boundary_baseline(&snapshot, prior)?;
    let index = lease.original_dacls.len();
    let handle = snapshot.handle.0;
    lease.original_dacls.push(snapshot);
    lease.snapshot_indices.insert(key.clone(), index);
    lease.boundary_protection.insert(key, baseline);
    apply_acl_change_security(
        &AclChange {
            path: path.to_path_buf(),
            permissions: BOUNDARY_MARKER | if original { 0 } else { DELETE.0 },
            inherit: false,
            mode: AclMode::Deny,
        },
        sid,
        handle,
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
    )?;
    if allowed != 0 {
        apply_acl_change(
            &AclChange {
                path: path.to_path_buf(),
                permissions: allowed,
                inherit: false,
                mode: AclMode::Grant,
            },
            sid,
            handle,
        )?;
    }
    Ok(())
}

fn restore_boundary_inheritance(
    snapshot: &DaclSnapshot,
    originally_protected: bool,
    original_acl: &[u8],
) -> Result<()> {
    let process = unsafe { GetCurrentProcess() };
    let mut duplicate = HANDLE::default();
    unsafe {
        DuplicateHandle(
            process,
            snapshot.handle.0,
            process,
            &mut duplicate,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .map_err(|error| {
        EngineError::Backend(format!("failed to retain boundary cleanup handle: {error}"))
    })?;
    let current = snapshot_dacl_handle(&snapshot.path, OwnedHandle(duplicate), false)?;
    // Read-only gitlink grants from other profiles are legitimate while their
    // marker is present; namespace grants were refused at preparation.
    if inspect_boundary_acl(&current, FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0)?.is_some()
        || originally_protected
    {
        return Ok(());
    }
    let mut acl = current
        .acl
        .as_ref()
        .expect("inspection rejects a null DACL")
        .clone();
    restore_inherited_ace_flags(&mut acl, original_acl)?;
    win32(unsafe {
        SetSecurityInfo(
            snapshot.handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(acl.as_ptr().cast::<ACL>()),
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

/// Resolve rustup's active Cargo to an already-installed standard toolchain
/// before entering LPAC. The contained child receives the resulting absolute
/// toolchain root through `RUSTUP_TOOLCHAIN`; rustup then multiplexes without
/// refreshing a channel or writing its operator-owned home. A missing/custom
/// toolchain leaves the environment unchanged and lets the eventual Rust
/// command fail normally rather than blocking unrelated Node-only launches.
fn resolve_installed_rustup_toolchain(
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Option<PathBuf> {
    find_on_path(Path::new("cargo"), env)?;
    let rustup = find_on_path(Path::new("rustup"), env)?;
    let rustup_home = env_value_ci(env, "RUSTUP_HOME").map(PathBuf::from)?;
    let output = std::process::Command::new(&rustup)
        .args(["which", "cargo"])
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        tracing::warn!(
            rustup = %rustup.display(),
            status = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "could not pin the active installed rustup toolchain for AppContainer"
        );
        return None;
    }
    let cargo = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let cargo = std::fs::canonicalize(&cargo).ok()?;
    let toolchains = std::fs::canonicalize(rustup_home.join("toolchains")).ok()?;
    if !path_contains(&toolchains, &cargo) {
        tracing::warn!(
            cargo = %cargo.display(),
            toolchains = %toolchains.display(),
            "active rustup Cargo is a custom toolchain outside RUSTUP_HOME; leaving it unpinned"
        );
        return None;
    }
    let bin = cargo.parent()?;
    if !bin
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("bin"))
    {
        return None;
    }
    bin.parent().map(local_dos_path)
}

/// Materialize the package-private temp path Windows substitutes for an
/// AppContainer child. Kranz redirects LOCALAPPDATA into the writable session
/// scratch, while `CreateAppContainerProfile` runs in the trusted parent's
/// ambient profile and therefore cannot create this redirected tree itself.
///
/// Source: https://learn.microsoft.com/windows/win32/secauthz/implementing-an-appcontainer
/// (`TEMP`/`TMP` become `<LOCALAPPDATA>/Packages/<profile>/AC/Temp`).
fn prepare_redirected_profile_temp(
    profile_name: &str,
    write_roots: &[PathBuf],
    env: &HashMap<String, String>,
) -> Result<PathBuf> {
    let profile = Path::new(profile_name);
    if profile.components().count() != 1
        || !matches!(
            profile.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(EngineError::Backend(format!(
            "invalid AppContainer profile name {profile_name:?}"
        )));
    }
    let local_app_data = env_value_ci(env, "LOCALAPPDATA").ok_or_else(|| {
        EngineError::Backend(
            "AppContainer launch requires redirected LOCALAPPDATA inside a writable root"
                .to_string(),
        )
    })?;
    let local_app_data = std::fs::canonicalize(local_app_data).map_err(|error| {
        EngineError::Backend(format!(
            "failed to canonicalize redirected AppContainer LOCALAPPDATA {local_app_data}: {error}"
        ))
    })?;
    let allowed = write_roots.iter().any(|root| {
        std::fs::canonicalize(root)
            .ok()
            .is_some_and(|root| path_contains(&root, &local_app_data))
    });
    if !allowed {
        return Err(EngineError::Backend(format!(
            "redirected AppContainer LOCALAPPDATA {} is outside the writable sandbox roots",
            local_app_data.display()
        )));
    }
    let temp = local_app_data
        .join("Packages")
        .join(profile_name)
        .join("AC")
        .join("Temp");
    std::fs::create_dir_all(&temp).map_err(|error| {
        EngineError::Backend(format!(
            "failed to create redirected AppContainer temp {}: {error}",
            temp.display()
        ))
    })?;
    Ok(temp)
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

/// Local drive root that a Windows tool can probe during startup. Node,
/// Cargo, and cmd all ask for root metadata even when every executable and
/// input lives below an explicitly granted directory. Keep this narrower
/// than `volume_root`: mutating a UNC/share root is outside Kranz's local
/// process-provider contract.
fn local_volume_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| {
            let mut components = ancestor.components();
            let Some(std::path::Component::Prefix(prefix)) = components.next() else {
                return false;
            };
            matches!(
                prefix.kind(),
                std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
            ) && components.all(|component| matches!(component, std::path::Component::RootDir))
        })
        .map(local_dos_path)
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

fn ordered_search_path_dirs(
    mut dirs: Vec<PathBuf>,
    preferred_rust_bin: Option<PathBuf>,
) -> Vec<PathBuf> {
    dirs.retain(|dir| dir.is_dir());
    dirs.sort();
    dirs.dedup();
    if let Some(preferred) = preferred_rust_bin.filter(|bin| bin.is_dir()) {
        let preferred_key = comparable_path(&preferred);
        dirs.retain(|dir| comparable_path(dir) != preferred_key);
        dirs.insert(0, preferred);
    }
    dirs
}

fn contained_search_path(
    inputs: &crate::sandbox::SandboxInputs,
    executable: &Path,
    env: &HashMap<String, String>,
    rustup_toolchain: Option<&Path>,
) -> Result<String> {
    let preferred_rust_bin =
        rustup_toolchain.map(|toolchain| local_dos_path(&toolchain.join("bin")));
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
        rustup_toolchain,
    )?);
    for file in toolchain_entry_points(env) {
        if let Some(parent) = file.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    let dirs = ordered_search_path_dirs(dirs, preferred_rust_bin);
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
    rustup_toolchain: Option<&Path>,
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
    // Hosted rustup toolchain roots can protect their DACL from inheriting the
    // grant on RUSTUP_HOME. Add the already-selected root itself so its
    // existing descendants receive an explicit read/execute-only grant.
    if let Some(root) = rustup_toolchain
        .filter(|toolchain| toolchain.is_dir())
        .and_then(|toolchain| std::fs::canonicalize(toolchain).ok())
    {
        roots.push(root);
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
    if [
        "SystemRoot",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMW6432",
    ]
    .iter()
    .any(|name| path_under_env(path, name))
    {
        return true;
    }
    // The PROFILE PARENT (`C:\Users`) belongs in this list too. It is
    // SYSTEM-owned and carries no app-package ACE, exactly like
    // `%PROGRAMFILES%`, so an unelevated operator cannot lease it — and every
    // Windows developer keeps repositories, `CARGO_HOME` and `RUSTUP_HOME`
    // beneath it. Walking into it made `ancestor_directories` demand
    // WRITE_DAC on `C:\Users` and the production GATE WRAP fail closed with
    // "Access is denied. (0x80070005)" for any non-elevated run.
    //
    // The traverse grant it was reaching for is not needed: bypass-traverse
    // (SeChangeNotifyPrivilege, held by Everyone including AppContainers)
    // already lets the child reach paths underneath. The phase-4 hostile
    // receipt demonstrates this directly — it reads and writes under
    // `%TEMP%`, i.e. below `C:\Users`, with no ACE anywhere on `C:\Users`.
    //
    // Hosted CI never reached this: its workspace and temp live on `D:\a\...`,
    // whose walk stops at the volume root, and its runner is elevated anyway.
    profile_parent().is_some_and(|parent| comparable_path(path) == comparable_path(&parent))
}

/// `C:\Users` — the directory holding every user profile, not the profile.
fn profile_parent() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .and_then(|profile| profile.parent().map(Path::to_path_buf))
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
    shared_git: &Path,
) -> Result<()> {
    let cwd = crate::sandbox::absolutize(&inputs.session_cwd);
    let session_authority = cwd.join(".kranz");
    let session_files = crate::sandbox::kranz_authority_entries(&session_authority).files;
    let gitlink = cwd.join(".git");
    let sealed_file = |root: &Path, protected: &Path| {
        comparable_path(root) == comparable_path(&cwd)
            && (session_files
                .iter()
                .any(|path| comparable_path(path) == comparable_path(protected))
                || (gitlink.is_file() && path_contains(&gitlink, protected)))
    };
    let mut read_denies = crate::sandbox::authority_read_deny_paths(inputs)
        .into_iter()
        .map(|path| (path, false))
        .collect::<Vec<_>>();
    read_denies.extend(
        crate::sandbox::authority_read_deny_dirs(inputs)
            .into_iter()
            .map(|path| (path, true)),
    );
    read_denies.extend(
        crate::sandbox::validator_read_deny_entries(inputs)
            .into_iter()
            .map(|entry| (entry.path, entry.is_dir)),
    );
    for root in write_roots.iter().chain(read_roots) {
        if path_contains(&session_authority, root) {
            return Err(EngineError::Backend(format!(
                "AppContainer grant {} is inside the protected worktree .kranz directory",
                root.display()
            )));
        }
        for (protected, directory) in &read_denies {
            if (path_contains(root, protected) || (*directory && path_contains(protected, root)))
                && !(comparable_path(root) == comparable_path(&cwd)
                    && session_files
                        .iter()
                        .any(|path| comparable_path(path) == comparable_path(protected)))
            {
                return Err(EngineError::Backend(format!(
                    "AppContainer recursive grant {} overlaps protected read path {}; refusing before ACL mutation",
                    root.display(), protected.display()
                )));
            }
        }
    }
    let authority = crate::sandbox::authority_write_denies(inputs);
    let mission = crate::sandbox::mission_write_denies(inputs);
    let git = crate::sandbox::git_metadata_write_denies(inputs);
    let mut write_denies = authority
        .files
        .into_iter()
        .chain(mission.files)
        .chain(git.files)
        .map(|path| (path, false))
        .collect::<Vec<_>>();
    write_denies.extend(
        authority
            .dirs
            .into_iter()
            .chain(mission.control_dirs)
            .chain(git.dirs)
            .chain(crate::sandbox::cargo_cache_write_deny_paths())
            .chain(std::iter::once(shared_git.to_path_buf()))
            .map(|path| (path, true)),
    );
    for root in write_roots {
        // The runs directory contains engine-owned transcripts, but private
        // scratch directories below it remain writable. Never grant its root.
        if mission
            .runs_dirs
            .iter()
            .any(|runs| path_contains(root, runs))
            || path_contains(root, &crate::sandbox::absolutize(&inputs.mission_dir))
        {
            return Err(EngineError::Backend(format!(
                "AppContainer writable root {} covers engine-owned mission metadata",
                root.display()
            )));
        }
        for (protected, directory) in &write_denies {
            if (path_contains(root, protected) || (*directory && path_contains(protected, root)))
                && !sealed_file(root, protected)
            {
                return Err(EngineError::Backend(format!(
                    "AppContainer writable root {} overlaps protected write path {}; refusing before ACL mutation",
                    root.display(), protected.display()
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
    rustup_toolchain: Option<&Path>,
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
    let read_roots = read_roots(
        executable,
        &inputs.session_cwd,
        &inputs.mission_dir,
        env,
        rustup_toolchain,
    )?;
    let shared_git = trusted_git_read_root(&inputs.session_cwd, &inputs.mission_dir)?;
    validate_recursive_roots(inputs, &write_roots, &read_roots, &shared_git)?;

    // Windows ACLs need an existing object. Reserve the namespace only after
    // validating that this is an isolated worktree, never the primary tree.
    // Keep an empty directory for the worktree lifetime: removing it when one
    // overlapping lease ends would reopen the other lease's future-file gap.
    let session_authority = crate::sandbox::absolutize(&inputs.session_cwd).join(".kranz");
    match std::fs::create_dir(&session_authority) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    use std::os::windows::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(&session_authority)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(EngineError::Backend(format!(
            "AppContainer worktree authority namespace {} must be a no-follow directory",
            session_authority.display()
        )));
    }

    let toolchain_entries = toolchain_entry_points(env);
    let mut changes = Vec::new();
    let rx = FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0;
    for root in &write_roots {
        changes.push(AclChange {
            path: root.clone(),
            permissions: WRITABLE_ROOT_ACCESS,
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
    for path in toolchain_entries {
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
    // The worktree namespace and gitlink are sealed before root grants.
    // Package-SID deny ACEs alone cannot subtract inherited LPAC access.
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
    // Authority write deny (2026-09-01 adversarial audit, H2 + H11): the
    // repo config, the serve tokens, the lint vocabulary, the repo-level
    // engine stores, and every SIBLING mission dir are write-denied, not
    // only read-denied. Windows already denied READS of the authority set
    // above (`deny_all`); this closes the write half for the wider set the
    // process tier now shares. The positive namespace boundary and grant
    // overlap checks above also exclude stores created after launch.
    let authority_writes = crate::sandbox::authority_write_denies(inputs);
    for path in authority_writes.files {
        if path.exists() {
            changes.push(AclChange {
                path,
                permissions: deny_write,
                inherit: false,
                mode: AclMode::Deny,
            });
        }
    }
    for path in authority_writes.dirs {
        if path.is_dir() {
            changes.push(AclChange {
                path,
                permissions: deny_write,
                inherit: true,
                mode: AclMode::Deny,
            });
        }
    }
    // `.git` metadata write deny (2026-09-01 adversarial audit, H3 support):
    // the hook and config surface that turns the engine's next unhardened
    // `git commit` into host execution, plus the worktree gitlink. Narrow by
    // design; the positive root policy already keeps shared metadata
    // read-only on this provider.
    let git_writes = crate::sandbox::git_metadata_write_denies(inputs);
    for path in git_writes.files {
        // Resolve short (8.3) aliases as well as verbatim paths. A generic
        // deny on the sealed gitlink would merge with its lease marker,
        // hiding the marker from overlapping-lease cleanup.
        let path = crate::sandbox::absolutize(&path);
        // A `.git` DIRECTORY is skipped here: the non-inheriting deny would
        // still be a deny on the container itself, and a directory node deny
        // that Windows evaluates on child creation would close the index the
        // worker legitimately writes. Only the gitlink FILE form and the
        // named config files are denied.
        if path.is_file()
            && comparable_path(&path)
                != comparable_path(&crate::sandbox::absolutize(&inputs.session_cwd).join(".git"))
        {
            changes.push(AclChange {
                path,
                permissions: deny_write,
                inherit: false,
                mode: AclMode::Deny,
            });
        }
    }
    for path in git_writes.dirs {
        if path.is_dir() {
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

fn new_lease(profile_name: String, rustup_toolchain: Option<PathBuf>) -> AppContainerLease {
    AppContainerLease {
        profile_name,
        rustup_toolchain,
        original_dacls: Vec::new(),
        snapshot_indices: BTreeMap::new(),
        boundary_protection: BTreeMap::new(),
        applied_changes: HashSet::new(),
        plan_paths: Vec::new(),
    }
}

pub(crate) fn new_launch_context() -> AppContainerLaunchContext {
    AppContainerLaunchContext(Arc::new(Mutex::new(None)))
}

/// Build the trusted helper command using the profile/ACL lease owned by one
/// resolved gate posture. The first command creates the disposable profile;
/// later commands reuse its SID and skip ACL mutations already held by the
/// lease. This makes setup once-per-resolution, matching the other process
/// providers and the measurement contract in `command_exec`.
pub(crate) fn prepare_launch_in_context(
    context: &AppContainerLaunchContext,
    inputs: &crate::sandbox::SandboxInputs,
    program: &Path,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<PreparedCommand> {
    let mut slot = context.0.lock().map_err(|_| {
        EngineError::Backend("AppContainer gate launch context mutex was poisoned".to_string())
    })?;
    if slot.is_none() {
        let rustup_toolchain = resolve_installed_rustup_toolchain(&inputs.session_cwd, env);
        let (profile_name, _sid) =
            create_profile(inputs.enforce == crate::types::SandboxEnforce::Fs)?;
        *slot = Some(new_lease(profile_name, rustup_toolchain));
    }
    let lease = slot
        .as_mut()
        .expect("AppContainer gate lease was initialized above");
    let sid = derive_profile_sid(&lease.profile_name)?;
    prepare_launch_for_lease(lease, sid.0, inputs, program, args, env)
}

/// Build the trusted helper command and apply a unique session launch's
/// temporary ACLs. Agent sessions retain their one-launch lease unchanged;
/// engine-run gates use [`prepare_launch_in_context`] to share one profile
/// only inside a single already-resolved gate posture.
pub(crate) fn prepare_launch(
    inputs: &crate::sandbox::SandboxInputs,
    program: &Path,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<PreparedLaunch> {
    let rustup_toolchain = resolve_installed_rustup_toolchain(&inputs.session_cwd, env);
    let (profile_name, sid) = create_profile(inputs.enforce == crate::types::SandboxEnforce::Fs)?;
    let mut lease = new_lease(profile_name, rustup_toolchain);
    let prepared = prepare_launch_for_lease(&mut lease, sid.0, inputs, program, args, env)?;
    Ok(PreparedLaunch {
        program: prepared.program,
        args: prepared.args,
        lease,
    })
}

fn prepare_launch_for_lease(
    lease: &mut AppContainerLease,
    sid: PSID,
    inputs: &crate::sandbox::SandboxInputs,
    program: &Path,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<PreparedCommand> {
    std::fs::create_dir_all(&inputs.tmpdir).map_err(|error| {
        EngineError::Backend(format!(
            "failed to create AppContainer private scratch {}: {error}",
            inputs.tmpdir.display()
        ))
    })?;
    let write_roots = crate::sandbox::write_allowlist(inputs);
    prepare_redirected_profile_temp(&lease.profile_name, &write_roots, env)?;
    let executable = resolve_executable(program, env)?;
    // The child PATH is narrower than the host PATH and deliberately includes
    // declared workspace roots. Use that SAME path while discovering exact
    // Node/npm/Cargo entry points for ACL grants: a workspace-staged runtime
    // can carry a protected DACL, so an inheritable grant on its parent is not
    // proof that LPAC can execute the existing file. This does not grant a new
    // root; it only adds a direct RX ACE to a known tool name already inside
    // the contained search path.
    let path = contained_search_path(inputs, &executable, env, lease.rustup_toolchain.as_deref())?;
    let mut acl_env = env.clone();
    acl_env.insert("PATH".to_string(), path.clone());
    let toolchain_entries = toolchain_entry_points(&acl_env);
    let mut volume_roots = std::iter::once(executable.as_path())
        .chain(toolchain_entries.iter().map(PathBuf::as_path))
        .filter_map(local_volume_root)
        .collect::<Vec<_>>();
    volume_roots.sort();
    volume_roots.dedup();
    verify_null_device_prepared()?;
    for root in &volume_roots {
        verify_volume_root_prepared(root)?;
    }
    let mut changes = acl_changes(
        inputs,
        &executable,
        &acl_env,
        lease.rustup_toolchain.as_deref(),
    )?;
    // All DACL updates are read/modify/write operations. Serialize the batch
    // across Kranz processes so simultaneous prepare/drop paths cannot publish
    // stale ACL copies over one another on shared toolchain or Git roots.
    let _guard = DaclMutationGuard::acquire()?;
    let cwd = crate::sandbox::absolutize(&inputs.session_cwd);
    // A pre-existing package/capability DELETE_CHILD grant on the parent
    // can replace sealed objects regardless of their own delete rights.
    inspect_boundary_acl(&snapshot_dacl(&cwd)?, WRITABLE_ROOT_ACCESS)?;
    let authority = cwd.join(".kranz");
    prepare_acl_boundary(lease, sid, &authority, 0)?;
    let gitlink = cwd.join(".git");
    if gitlink.is_file() {
        prepare_acl_boundary(
            lease,
            sid,
            &gitlink,
            FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0,
        )?;
    }
    // Key retained handles by physical Windows spelling too. A worktree can
    // be present as both `C:\...` and canonical `\\?\C:\...` changes with
    // different permissions, so the exact-change collapse below deliberately
    // keeps both operations. They must still share ONE retained handle:
    // removing the profile ACE repeatedly through alias handles can republish
    // a stale inherited DACL and make the next launch lose execute access.
    // One ancestor directory is shared by many toolchain entry points, and a
    // read root can arrive in both verbatim and plain form. Applying the same
    // ACE twice is a no-op the OS still charges full price for, so collapse
    // exact repeats before touching a single descriptor.
    let mut applied = HashSet::new();
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
        if !lease.snapshot_indices.contains_key(&key) {
            let index = lease.original_dacls.len();
            lease.original_dacls.push(snapshot_dacl(&change.path)?);
            lease.snapshot_indices.insert(key, index);
        }
    }
    for change in &changes {
        let key = comparable_path(&change.path);
        let applied_key = (key.clone(), change.permissions, change.inherit, change.mode);
        if lease.applied_changes.contains(&applied_key) {
            continue;
        }
        let handle = lease.original_dacls[lease.snapshot_indices[&key]].handle.0;
        let started = std::time::Instant::now();
        apply_acl_change(change, sid, handle)?;
        lease.applied_changes.insert(applied_key);
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
        profile_name: lease.profile_name.clone(),
        executable,
        args: args.to_vec(),
        cwd: crate::sandbox::absolutize(&inputs.session_cwd),
        // fs keeps ordinary outbound access; fs+net is a hard-offline
        // AppContainer (no network capabilities). A proxy-only environment is
        // never treated as a boundary.
        allow_network: inputs.enforce == crate::types::SandboxEnforce::Fs,
        path: Some(path),
        rustup_toolchain: lease.rustup_toolchain.clone(),
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
    lease.plan_paths.push(plan_path.clone());
    serde_json::to_writer(&mut file, &plan).map_err(|error| {
        EngineError::Backend(format!(
            "failed to serialize AppContainer launch plan: {error}"
        ))
    })?;
    file.flush().map_err(|error| {
        EngineError::Backend(format!("failed to flush AppContainer launch plan: {error}"))
    })?;
    let helper = std::env::current_exe().map_err(|error| {
        EngineError::Backend(format!(
            "failed to locate Kranz AppContainer helper: {error}"
        ))
    })?;
    Ok(PreparedCommand {
        program: helper,
        args: vec![
            INTERNAL_LAUNCHER_ARG.to_string(),
            plan_path.to_string_lossy().into_owned(),
        ],
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
            // Job setup can fail before the suspended child is assigned.
            // Closing handles alone would leave that process alive forever.
            // After normal completion/job teardown this is an inert retry.
            let _ = TerminateProcess(self.process, 1);
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

fn string_sid(value: &str, label: &str) -> Result<Vec<u8>> {
    let value = wide(value);
    let mut sid = PSID::default();
    unsafe { ConvertStringSidToSidW(PCWSTR(value.as_ptr()), &mut sid) }
        .map_err(|error| EngineError::Backend(format!("failed to build {label} SID: {error}")))?;
    let allocation = LocalAllocation(HLOCAL(sid.0));
    let len = unsafe { GetLengthSid(sid) } as usize;
    if len == 0 {
        return Err(EngineError::Backend(format!("{label} SID had zero length")));
    }
    let storage = unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), len) }.to_vec();
    drop(allocation);
    Ok(storage)
}

fn internet_capability() -> Result<Vec<u8>> {
    well_known_sid(WinCapabilityInternetClientSid, "internetClient capability")
}

fn named_capability(name: &str) -> Result<Vec<u8>> {
    let name_wide = wide(name);
    let mut group_sids = null_mut();
    let mut group_count = 0u32;
    let mut capability_sids = null_mut();
    let mut capability_count = 0u32;
    let derived = unsafe {
        DeriveCapabilitySidsFromName(
            PCWSTR(name_wide.as_ptr()),
            &mut group_sids,
            &mut group_count,
            &mut capability_sids,
            &mut capability_count,
        )
    };
    let _group_sids = LocalSidArray {
        sids: group_sids,
        count: group_count,
    };
    let capability_sids = LocalSidArray {
        sids: capability_sids,
        count: capability_count,
    };
    derived.map_err(|error| {
        EngineError::Backend(format!("failed to derive {name} capability SID: {error}"))
    })?;
    if capability_sids.count != 1 || capability_sids.sids.is_null() {
        return Err(EngineError::Backend(format!(
            "{name} capability derivation returned {} SIDs; expected exactly one",
            capability_sids.count
        )));
    }
    let sid = unsafe { *capability_sids.sids };
    let len = unsafe { GetLengthSid(sid) } as usize;
    if len == 0 {
        return Err(EngineError::Backend(format!(
            "{name} capability SID had zero length"
        )));
    }
    Ok(unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), len) }.to_vec())
}

fn launch_capability_policy(allow_network: bool) -> Vec<LaunchCapability> {
    let mut capabilities = vec![LaunchCapability::RegistryRead];
    if allow_network {
        capabilities.push(LaunchCapability::InternetClient);
    }
    capabilities
}

fn launch_capability_storage(allow_network: bool) -> Result<Vec<Vec<u8>>> {
    launch_capability_policy(allow_network)
        .into_iter()
        .map(|capability| match capability {
            LaunchCapability::RegistryRead => named_capability(REGISTRY_READ_CAPABILITY),
            LaunchCapability::InternetClient => internet_capability(),
        })
        .collect()
}

fn capability_attributes(storage: &mut [Vec<u8>]) -> Vec<SID_AND_ATTRIBUTES> {
    storage
        .iter_mut()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: PSID(sid.as_mut_ptr().cast()),
            Attributes: SE_GROUP_ENABLED as u32,
        })
        .collect()
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

/// True for the documented per-drive current-directory pseudo variable (`=C:`),
/// the ONE `=`-prefixed name an explicit environment block may carry.
fn is_drive_current_directory_key(key: &OsStr) -> bool {
    let units: Vec<u16> = key.encode_wide().collect();
    is_drive_current_directory_units(&units)
}

fn is_drive_current_directory_units(units: &[u16]) -> bool {
    let drive_letter = units.get(1).is_some_and(|unit| {
        (*unit >= b'A' as u16 && *unit <= b'Z' as u16)
            || (*unit >= b'a' as u16 && *unit <= b'z' as u16)
    });
    units.len() == 3 && units[0] == b'=' as u16 && drive_letter && units[2] == b':' as u16
}

/// Windows exposes pseudo variables whose names contain `=`: the per-drive
/// current-directory entries (`=C:`) and cmd.exe's `=ExitCode` /
/// `=ExitCodeAscii`, which it sets after the first command of a session.
/// A name containing `=` cannot be encoded in an explicit environment block,
/// and `CreateProcessW` never propagates these into one anyway — so DROP them
/// while inheriting rather than refusing the whole launch. Before this filter,
/// running `kranz` from any cmd.exe prompt made every enforced Windows session
/// fail closed on `=ExitCode`; pwsh does not set it, so CI never saw it.
/// The per-drive entry is kept, and the one for `cwd` is re-added deliberately
/// below.
fn inheritable_environment_key(key: &OsStr) -> bool {
    !key.encode_wide().any(|unit| unit == b'=' as u16) || is_drive_current_directory_key(key)
}

fn environment_block(
    cwd: &Path,
    path: Option<&str>,
    rustup_toolchain: Option<&Path>,
) -> Result<Vec<u16>> {
    let mut values = BTreeMap::<String, (OsString, OsString)>::new();
    for (key, value) in std::env::vars_os() {
        if !inheritable_environment_key(&key) {
            continue;
        }
        let folded = key.to_string_lossy().to_ascii_uppercase();
        values.insert(folded, (key, value));
    }
    if let Some(path) = path {
        values.insert(
            "PATH".to_string(),
            (OsString::from("PATH"), OsString::from(path)),
        );
    }
    if let Some(toolchain) = rustup_toolchain {
        values.insert(
            "RUSTUP_TOOLCHAIN".to_string(),
            (
                OsString::from("RUSTUP_TOOLCHAIN"),
                toolchain.as_os_str().to_os_string(),
            ),
        );
        values.insert(
            "RUSTUP_AUTO_INSTALL".to_string(),
            (OsString::from("RUSTUP_AUTO_INSTALL"), OsString::from("0")),
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
        let drive_current_directory = is_drive_current_directory_units(&key);
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
    let mut capability_storage = launch_capability_storage(plan.allow_network)?;
    let mut capabilities = capability_attributes(&mut capability_storage);
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
    let environment = environment_block(
        &process_cwd,
        plan.path.as_deref(),
        plan.rustup_toolchain.as_deref(),
    )?;
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
        .map_err(|error| EngineError::Backend(format!("failed to open process token: {error}")))?;
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
    .map_err(|error| EngineError::Backend(format!("failed to inspect process {label}: {error}")))?;
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

// Inspect only disposable fixture paths and the disposable profile SID. Keep
// this evidence in failures so native ACL propagation and overlap regressions
// can be distinguished from a child's access-check behavior.
fn self_test_acl_diagnostics(paths: &[PathBuf], profile: &str, stage: &str) -> Result<()> {
    let sid = derive_profile_sid(profile)?;
    for path in paths {
        if !path.exists() {
            continue;
        }
        let snapshot = snapshot_dacl(path)?;
        let mut entries = Vec::new();
        if let Some(acl) = &snapshot.acl {
            let acl = acl.as_ptr().cast::<ACL>();
            for index in 0..unsafe { (*acl).AceCount } {
                let mut ace = null_mut();
                unsafe { GetAce(acl, u32::from(index), &mut ace) }.map_err(|error| {
                    EngineError::Backend(format!("failed to inspect fixture ACE: {error}"))
                })?;
                let header = unsafe { &*ace.cast::<ACE_HEADER>() };
                if header.AceType > 1 {
                    continue;
                }
                // Ordinary allow and deny ACEs have the same mask/SID layout.
                let entry = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                let trustee = PSID((&entry.SidStart as *const u32).cast_mut().cast());
                if unsafe { EqualSid(trustee, sid.0) }.is_ok() {
                    entries.push((header.AceType, header.AceFlags, entry.Mask));
                }
            }
        }
        eprintln!(
            "fixture ACL: stage={stage} path={} protected={} profile_aces(type,flags,mask)={entries:?}",
            path.display(), snapshot.protected
        );
    }
    Ok(())
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
    let worktree_authority = worktree.join(".kranz");
    std::fs::create_dir(&worktree_authority)?;
    let worktree_authority_files =
        crate::sandbox::kranz_authority_entries(&worktree_authority).files;
    // Two files predate the ACL lease; two appear only after both overlapping
    // leases are prepared. Both sets must remain unreadable and immutable.
    for path in &worktree_authority_files[..2] {
        std::fs::write(path, "must-not-cross")?;
    }
    let authority_before = snapshot_dacl(&worktree_authority)?;
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
    let gitlink_before = snapshot_dacl(&worktree.join(".git"))?;

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
        worktree_authority_files,
        boundary_state_names: vec![
            boundary_state_name(&authority_before)?,
            boundary_state_name(&gitlink_before)?,
        ],
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
    let volume_root = local_volume_root(&executable).ok_or_else(|| {
        EngineError::Backend(format!(
            "self-test executable {} is not on a local Windows volume",
            executable.display()
        ))
    })?;
    // Read-only: this pair is evidence, never restored (see read_dacl_bytes).
    let volume_root_before = read_dacl_bytes(&volume_root)?;
    // Both leases touch the same worktree, toolchain, scratch, and Git roots.
    // Dropping the first must remove only its own SID, leaving the second
    // launch functional; dropping the second must recover the exact baseline.
    let overlapping = prepare_launch(
        &inputs,
        &executable,
        &[INTERNAL_HOSTILE_CHILD_ARG.to_string()],
        &env,
    )?;
    let diagnostic_paths = std::iter::once(worktree.clone())
        .chain(std::iter::once(worktree_authority.clone()))
        .chain(std::iter::once(worktree.join(".git")))
        .chain(manifest.worktree_authority_files.iter().cloned())
        .collect::<Vec<_>>();
    self_test_acl_diagnostics(
        &diagnostic_paths,
        &overlapping.lease.profile_name,
        "first-prepared",
    )?;
    let prepared = prepare_launch(
        &inputs,
        &executable,
        &[INTERNAL_HOSTILE_CHILD_ARG.to_string()],
        &env,
    )?;
    self_test_acl_diagnostics(
        &diagnostic_paths,
        &prepared.lease.profile_name,
        "second-prepared",
    )?;
    // Positive control: the named baselines exist and the trusted parent can
    // open them for writing before the contained child attempts the same.
    for name in &manifest.boundary_state_names {
        let name = wide(OsStr::new(name));
        for access in [FILE_MAP_WRITE.0, WRITE_DAC.0, WRITE_OWNER.0] {
            let _handle = unsafe { OpenFileMappingW(access, false, PCWSTR(name.as_ptr())) }
                .map(OwnedHandle)
                .map_err(|error| {
                    EngineError::Backend(format!("baseline access control failed: {error}"))
                })?;
        }
    }
    let PreparedLaunch {
        program,
        args,
        lease,
    } = prepared;
    for path in &manifest.worktree_authority_files[2..] {
        std::fs::write(path, "must-not-cross")?;
    }
    drop(overlapping.lease);
    self_test_acl_diagnostics(&diagnostic_paths, &lease.profile_name, "first-reaped")?;
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
    let volume_root_after = read_dacl_bytes(&volume_root)?;
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
    let authority_after = snapshot_dacl(&worktree_authority)?;
    let gitlink_after = snapshot_dacl(&worktree.join(".git"))?;
    let root_restored = before.acl == after.acl && before.protected == after.protected;
    let authority_restored = authority_before.acl == authority_after.acl
        && authority_before.protected == authority_after.protected;
    let gitlink_restored = gitlink_before.acl == gitlink_after.acl
        && gitlink_before.protected == gitlink_after.protected;
    eprintln!("fixture DACL restoration: root={root_restored} authority={authority_restored} gitlink={gitlink_restored}");
    receipt.dacl_restored = root_restored && authority_restored && gitlink_restored;
    if !receipt.dacl_restored {
        for (before, after) in [
            (&authority_before, &authority_after),
            (&gitlink_before, &gitlink_after),
        ] {
            eprintln!(
                "fixture restoration detail: path={} before_protected={} after_protected={} before_acl={:?} after_acl={:?}",
                before.path.display(), before.protected, after.protected, before.acl, after.acl
            );
        }
    }
    receipt.volume_root_dacl_restored = volume_root_before == volume_root_after;
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
        && receipt.worktree_git_read
        && receipt.worktree_git_write_denied
        && receipt.worktree_git_replace_denied
        && receipt.boundary_state_write_denied
        && receipt.scratch_write
        && receipt.outside_write_denied
        && receipt.authority_read_denied
        && receipt.worktree_authority_read_denied
        && receipt.worktree_authority_write_denied
        && receipt.worktree_authority_create_denied
        && receipt.worktree_authority_rename_denied
        && receipt.ordinary_file_delete
        && receipt.real_checkout_read_denied
        && receipt.shared_git_read
        && receipt.overlapping_lease_safe
        && receipt.tampered_git_pointer_refused
        && receipt.network_denied
        && receipt.dacl_restored
        && receipt.volume_root_dacl_restored;
    if !all_passed {
        return Err(EngineError::Backend(format!(
            "production hostile receipt contained a failed assertion: {receipt:?}; child diagnostics: {}",
            String::from_utf8_lossy(&output.stderr)
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
    env: &HashMap<String, String>,
    sandbox: &crate::command_exec::GateSandbox,
    appcontainer: bool,
    sample: &str,
) -> Result<f64> {
    let started = std::time::Instant::now();
    let (code, output) = if appcontainer {
        crate::command_exec::run_bounded_gate_command_resolved_with_code(
            worktree, command, env, sandbox,
        )
    } else {
        crate::command_exec::run_bounded_gate_command_resolved_with_code(
            worktree,
            command,
            env,
            &crate::command_exec::GateSandbox::Disabled,
        )
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
        let diagnostics = if appcontainer {
            gate_failure_diagnostics(worktree, env, sandbox)
        } else {
            String::new()
        };
        return Err(EngineError::Backend(format!(
            "{posture} normal gate {sample} failed with exit {code:?} or omitted {marker}: \
             {output:?}{diagnostics}"
        )));
    }
    Ok(elapsed_ms)
}

fn gate_failure_diagnostics(
    worktree: &Path,
    env: &HashMap<String, String>,
    sandbox: &crate::command_exec::GateSandbox,
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
        let (code, output) = crate::command_exec::run_bounded_gate_command_resolved_with_code(
            worktree, command, env, sandbox,
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
    env: &HashMap<String, String>,
    sandbox: &crate::command_exec::GateSandbox,
) -> Result<GateTimingReceipt> {
    // Warm both postures before retaining samples. Alternate their order so
    // runner drift does not systematically favor either side.
    run_gate_sample(worktree, command, marker, env, sandbox, false, "warm-up")?;
    run_gate_sample(worktree, command, marker, env, sandbox, true, "warm-up")?;

    let mut off_samples_ms = Vec::with_capacity(GATE_OVERHEAD_REPETITIONS);
    let mut appcontainer_samples_ms = Vec::with_capacity(GATE_OVERHEAD_REPETITIONS);
    for index in 0..GATE_OVERHEAD_REPETITIONS {
        if index % 2 == 0 {
            off_samples_ms.push(run_gate_sample(
                worktree,
                command,
                marker,
                env,
                sandbox,
                false,
                &format!("sample {}", index + 1),
            )?);
            appcontainer_samples_ms.push(run_gate_sample(
                worktree,
                command,
                marker,
                env,
                sandbox,
                true,
                &format!("sample {}", index + 1),
            )?);
        } else {
            appcontainer_samples_ms.push(run_gate_sample(
                worktree,
                command,
                marker,
                env,
                sandbox,
                true,
                &format!("sample {}", index + 1),
            )?);
            off_samples_ms.push(run_gate_sample(
                worktree,
                command,
                marker,
                env,
                sandbox,
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

/// Exercise ordinary Node and Rust contract commands through the exact bounded
/// gate executor and one resolved validation/final-gate posture, then retain
/// interleaved warm-cache timing samples against the byte-identical unwrapped
/// runner. This complements the hostile receipt above: neither proof
/// substitutes for the other.
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
        format!(
            r#"const assert = require('node:assert/strict');
let checksum = 0;
for (let i = 0; i < 100000; i += 1) checksum = (checksum + i) >>> 0;
assert.equal(checksum, 704982704);
setTimeout(() => console.log('kranz-node-gate-ok'), {GATE_WORKLOAD_MILLIS});
"#
        ),
    )?;
    std::fs::write(
        worktree.join("rust-gate").join("Cargo.toml"),
        "[package]\nname = \"kranz-windows-gate-receipt\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    std::fs::write(
        rust_src.join("lib.rs"),
        format!(
            r#"#[cfg(test)]
mod tests {{
    #[test]
    fn normal_rust_gate() {{
        let values: Vec<u64> = (0..100_000).collect();
        assert_eq!(values.iter().sum::<u64>(), 4_999_950_000);
        std::thread::sleep(std::time::Duration::from_millis({GATE_WORKLOAD_MILLIS}));
        println!("kranz-rust-gate-ok");
    }}
}}
"#
        ),
    )?;

    let sandbox_config = crate::types::SandboxConfig {
        enforce: crate::types::SandboxEnforce::FsNet,
        provider: crate::types::SandboxProvider::Process,
        image: None,
        extra_write: Vec::new(),
        egress: Vec::new(),
    };
    // Production validation and final-gate batches resolve one posture and
    // run all contract assertions through it. Keep the same stable scratch,
    // cleared env, profile, and ACL lease here: the first wrapped warm-up owns
    // one-time preparation; retained samples measure per-command overhead.
    let gate_home = root.0.join("gate-home");
    std::fs::create_dir_all(gate_home.join("tmp"))?;
    let cargo_home = crate::agent_env::cache_only_cargo_home(&gate_home);
    if !cargo_home.is_dir() {
        return Err(EngineError::Backend(format!(
            "normal-gate receipt could not create cache-only Cargo home {}",
            cargo_home.display()
        )));
    }
    let mut gate_env = crate::command_exec::sanitized_gate_env();
    gate_env.insert("CARGO_HOME".to_string(), cargo_home.display().to_string());
    crate::agent_env::redirect_windows_profile_env(&mut gate_env, &gate_home);
    let sandbox = crate::command_exec::resolve_gate_sandbox(
        &sandbox_config,
        &worktree,
        &mission,
        &gate_home,
        &gate_home,
    )?
    .sandbox;
    eprintln!(
        "gate self test: staged runtime at {}; measuring the node gate",
        node.display()
    );
    let node = measure_gate(
        &worktree,
        r#".\node.exe node-gate.js"#,
        "kranz-node-gate-ok",
        &gate_env,
        &sandbox,
    )?;
    eprintln!("gate self test: node gate retired; measuring the rust gate");
    let rust = measure_gate(
        &worktree,
        "cargo test --quiet --manifest-path rust-gate/Cargo.toml -- --nocapture",
        "kranz-rust-gate-ok",
        &gate_env,
        &sandbox,
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
    let worktree_authority = std::env::current_dir()?.join(".kranz");
    let ordinary_file = std::env::current_dir()?.join("ordinary-delete-control");
    let gitlink = std::env::current_dir()?.join(".git");
    let replacement_gitlink = std::env::current_dir()?.join("replacement-gitlink");
    std::fs::write(&replacement_gitlink, "gitdir: forged")?;
    let authority_probes = manifest
        .worktree_authority_files
        .iter()
        .map(|path| {
            let read_error = std::fs::read(path).err();
            let write_error = std::fs::write(path, "escape").err();
            eprintln!(
                "fixture authority probe: path={} read_error={read_error:?} write_error={write_error:?}",
                path.display()
            );
            (read_error, write_error)
        })
        .collect::<Vec<_>>();
    let gitlink_read =
        std::fs::read_to_string(&gitlink).is_ok_and(|value| value.starts_with("gitdir: "));
    let gitlink_write_error = std::fs::write(&gitlink, "gitdir: forged").err();
    let gitlink_replace_error = std::fs::rename(&replacement_gitlink, &gitlink).err();
    eprintln!("fixture gitlink probe: write_error={gitlink_write_error:?} replace_error={gitlink_replace_error:?}");
    let boundary_state_denials = manifest
        .boundary_state_names
        .iter()
        .flat_map(|name| {
            [FILE_MAP_WRITE.0, WRITE_DAC.0, WRITE_OWNER.0].map(|access| {
                let name = wide(OsStr::new(name));
                let result = unsafe { OpenFileMappingW(access, false, PCWSTR(name.as_ptr())) }
                    .map(OwnedHandle);
                eprintln!(
                    "fixture boundary state access={access:?}: {:?}",
                    result.as_ref().err()
                );
                result.is_err()
            })
        })
        .collect::<Vec<_>>();
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
        worktree_git_read: gitlink_read,
        worktree_git_write_denied: gitlink_write_error
            .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied),
        worktree_git_replace_denied: gitlink_replace_error.is_some(),
        boundary_state_write_denied: !boundary_state_denials.is_empty()
            && boundary_state_denials.iter().all(|denied| *denied),
        scratch_write: std::fs::write(&manifest.scratch_write, "allowed").is_ok(),
        outside_write_denied: std::fs::write(&manifest.outside_write, "escape").is_err(),
        authority_read_denied: std::fs::read(&manifest.authority_file).is_err(),
        worktree_authority_read_denied: authority_probes.iter().all(|(read_error, _)| {
            read_error
                .as_ref()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
        }),
        worktree_authority_write_denied: authority_probes.iter().all(|(_, write_error)| {
            write_error
                .as_ref()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
        }),
        worktree_authority_create_denied: std::fs::write(
            worktree_authority.join("new-authority"),
            "escape",
        )
        .is_err(),
        worktree_authority_rename_denied: std::fs::rename(
            &worktree_authority,
            worktree_authority.with_file_name("moved-authority"),
        )
        .is_err(),
        ordinary_file_delete: std::fs::write(&ordinary_file, "allowed").is_ok()
            && std::fs::remove_file(&ordinary_file).is_ok(),
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
        volume_root_dacl_restored: false,
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
    fn recursive_grants_reserve_only_the_isolated_worktree_authority_namespace() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("worktree");
        let mission = root.path().join("repo/.kranz/missions/m-test");
        let scratch = root.path().join("scratch");
        for path in [&cwd, &mission, &scratch] {
            std::fs::create_dir_all(path).unwrap();
        }
        let inputs = crate::sandbox::SandboxInputs {
            enforce: crate::types::SandboxEnforce::FsNet,
            session_cwd: cwd.clone(),
            mission_dir: mission,
            tmpdir: scratch.clone(),
            extra_write: Vec::new(),
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        };
        std::fs::write(cwd.join(".git"), "gitdir: trusted").unwrap();
        let shared_git = root.path().join("repo/.git");
        validate_recursive_roots(&inputs, &[cwd.clone(), scratch], &[], &shared_git).unwrap();
        for forbidden in [
            root.path().to_path_buf(),
            root.path().join("repo"),
            cwd.join(".kranz"),
            cwd.join(".kranz/nested"),
        ] {
            assert!(validate_recursive_roots(
                &inputs,
                std::slice::from_ref(&forbidden),
                &[],
                &shared_git
            )
            .is_err());
            assert!(validate_recursive_roots(&inputs, &[], &[forbidden], &shared_git).is_err());
        }
        for forbidden in [
            root.path().join("repo/.kranz/queue/nested"),
            shared_git.clone(),
            shared_git.join("objects"),
            inputs.mission_dir.join("runs"),
        ]
        .into_iter()
        .chain(
            crate::sandbox::cargo_cache_write_deny_paths()
                .into_iter()
                .map(|path| path.join("nested")),
        ) {
            assert!(validate_recursive_roots(&inputs, &[forbidden], &[], &shared_git).is_err());
        }
        let real_checkout = root.path().join("real-checkout");
        let source = real_checkout.join("src");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        let mut validator = inputs.clone();
        validator.validator_read_deny_roots.push(real_checkout);
        for forbidden in [source.clone(), source.join("nested")] {
            assert!(validate_recursive_roots(
                &validator,
                std::slice::from_ref(&forbidden),
                &[],
                &shared_git
            )
            .is_err());
            assert!(validate_recursive_roots(&validator, &[], &[forbidden], &shared_git).is_err());
        }
        assert!(
            !cwd.join(".kranz").exists(),
            "validation must not create paths"
        );
    }

    #[test]
    fn gitlink_aliases_never_receive_generic_metadata_denies() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("worktree");
        let mission = root.path().join("repo/.kranz/missions/m-test");
        let gitdir = root.path().join("repo/.git/worktrees/test");
        let scratch = root.path().join("scratch");
        let toolchain = root.path().join("toolchain");
        let alias = root.path().join("alias");
        for path in [&cwd, &mission, &gitdir, &scratch, &toolchain, &alias] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(cwd.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let executable = toolchain.join("probe.exe");
        std::fs::write(&executable, "fixture").unwrap();
        let inputs = crate::sandbox::SandboxInputs {
            enforce: crate::types::SandboxEnforce::FsNet,
            session_cwd: alias.join("../worktree"),
            mission_dir: mission,
            tmpdir: scratch,
            extra_write: Vec::new(),
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        };
        let changes = acl_changes(&inputs, &executable, &HashMap::new(), None).unwrap();
        let gitlink = comparable_path(&crate::sandbox::absolutize(&cwd.join(".git")));
        assert!(changes.iter().all(|change| {
            comparable_path(&crate::sandbox::absolutize(&change.path)) != gitlink
        }));
        assert!(changes.iter().any(|change| {
            change.mode == AclMode::Grant
                && comparable_path(&change.path)
                    == comparable_path(&crate::sandbox::absolutize(&cwd))
                && change.permissions & DELETE.0 != 0
                && change.permissions & FILE_DELETE_CHILD.0 == 0
        }));
    }

    #[test]
    fn boundary_parent_rejects_package_delete_child_grants() {
        let root = tempfile::tempdir().unwrap();
        let snapshot = snapshot_dacl(root.path()).unwrap();
        let mut any_package =
            well_known_sid(WinBuiltinAnyPackageSid, "ALL APPLICATION PACKAGES").unwrap();
        let sid = PSID(any_package.as_mut_ptr().cast());
        let _guard = DaclMutationGuard::acquire().unwrap();
        for permissions in [WRITABLE_ROOT_ACCESS, FILE_DELETE_CHILD.0] {
            apply_acl_change(
                &AclChange {
                    path: root.path().to_path_buf(),
                    permissions,
                    inherit: true,
                    mode: AclMode::Grant,
                },
                sid,
                snapshot.handle.0,
            )
            .unwrap();
            let current = snapshot_dacl(root.path()).unwrap();
            assert_eq!(
                inspect_boundary_acl(&current, WRITABLE_ROOT_ACCESS).is_ok(),
                permissions == WRITABLE_ROOT_ACCESS
            );
        }
    }

    #[test]
    fn boundary_baseline_survives_a_late_joiner_and_preserves_explicit_rules() {
        let root = tempfile::tempdir().unwrap();
        let authority = root.path().join(".kranz");
        let expected = root.path().join("expected");
        std::fs::create_dir(&authority).unwrap();
        std::fs::create_dir(&expected).unwrap();
        let mut system = string_sid("S-1-5-18", "SYSTEM").unwrap();
        let mut guests = string_sid("S-1-5-32-546", "Guests").unwrap();
        let explicit = |path: &Path, sid, permissions, mode| {
            let snapshot = snapshot_dacl(path).unwrap();
            let _guard = DaclMutationGuard::acquire().unwrap();
            apply_acl_change(
                &AclChange {
                    path: path.to_path_buf(),
                    permissions,
                    inherit: true,
                    mode,
                },
                sid,
                snapshot.handle.0,
            )
            .unwrap();
        };
        // A redundant explicit SYSTEM rule must stay explicit even though
        // its inherited peer is temporarily converted to the same shape.
        for path in [&authority, &expected] {
            explicit(
                path,
                PSID(system.as_mut_ptr().cast()),
                windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS.0,
                AclMode::Grant,
            );
        }
        explicit(
            &expected,
            PSID(guests.as_mut_ptr().cast()),
            FILE_GENERIC_WRITE.0,
            AclMode::Deny,
        );
        let baseline = snapshot_dacl(&authority).unwrap();
        let acl = baseline.acl.as_ref().unwrap().as_ptr().cast::<ACL>();
        let mut system_inheritance = Vec::new();
        for index in 0..unsafe { (*acl).AceCount } {
            let mut ace = null_mut();
            unsafe { GetAce(acl, u32::from(index), &mut ace) }.unwrap();
            let entry = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = PSID((&entry.SidStart as *const u32).cast_mut().cast());
            if unsafe { EqualSid(sid, PSID(system.as_mut_ptr().cast())) }.is_ok() {
                system_inheritance.push(entry.Header.AceFlags & INHERITED_ACE.0 as u8 != 0);
            }
        }
        assert!(system_inheritance.contains(&false));
        assert!(system_inheritance.contains(&true));
        let expected = snapshot_dacl(&expected).unwrap();
        let prepare = || {
            let (name, sid) = create_profile(false).unwrap();
            let mut lease = new_lease(name, None);
            let _guard = DaclMutationGuard::acquire().unwrap();
            prepare_acl_boundary(&mut lease, sid.0, &authority, 0).unwrap();
            assert_eq!(
                lease.boundary_protection.values().next().unwrap().acl,
                *baseline.acl.as_ref().unwrap()
            );
            lease
        };
        let first = prepare();
        let second = prepare();
        drop(first);
        // Preserve an unrelated explicit operator edit made during a lease.
        explicit(
            &authority,
            PSID(guests.as_mut_ptr().cast()),
            FILE_GENERIC_WRITE.0,
            AclMode::Deny,
        );
        let third = prepare();
        drop(second);
        assert!(snapshot_dacl(&authority).unwrap().protected);
        drop(third);
        let after = snapshot_dacl(&authority).unwrap();
        assert_eq!(after.protected, expected.protected);
        assert_eq!(after.acl, expected.acl);
    }

    #[test]
    fn boundary_marker_without_shared_baseline_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let authority = root.path().join(".kranz");
        std::fs::create_dir(&authority).unwrap();
        let (name, sid) = create_profile(false).unwrap();
        let mut orphan = new_lease(name, None);
        {
            let _guard = DaclMutationGuard::acquire().unwrap();
            prepare_acl_boundary(&mut orphan, sid.0, &authority, 0).unwrap();
            // Model the last original owner crashing: its volatile baseline
            // disappears but its protected marker remains on the object.
            orphan.boundary_protection.clear();
        }
        let (name, sid) = create_profile(false).unwrap();
        let mut next = new_lease(name, None);
        let _guard = DaclMutationGuard::acquire().unwrap();
        assert!(prepare_acl_boundary(&mut next, sid.0, &authority, 0).is_err());
        assert!(snapshot_dacl(&authority).unwrap().protected);
    }

    #[test]
    fn sealed_acl_boundaries_survive_either_lease_order_and_restore_inheritance() {
        for originally_protected in [false, true] {
            for reverse in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let authority = root.path().join(".kranz");
                let gitlink = root.path().join(".git");
                std::fs::create_dir(&authority).unwrap();
                std::fs::write(authority.join("serve.token"), "protected").unwrap();
                std::fs::write(&gitlink, "gitdir: example").unwrap();
                if originally_protected {
                    for path in [&authority, &gitlink] {
                        let snapshot = snapshot_dacl(path).unwrap();
                        let acl = snapshot.acl.as_ref().unwrap();
                        win32(unsafe {
                            SetSecurityInfo(
                                snapshot.handle.0,
                                SE_FILE_OBJECT,
                                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                                None,
                                None,
                                Some(acl.as_ptr().cast::<ACL>()),
                                None,
                            )
                        })
                        .unwrap();
                    }
                }
                let authority_before = snapshot_dacl(&authority).unwrap();
                let gitlink_before = snapshot_dacl(&gitlink).unwrap();
                let root_before = snapshot_dacl(root.path()).unwrap();
                let prepare = || {
                    let (name, sid) = create_profile(false).unwrap();
                    let mut lease = new_lease(name, None);
                    let _guard = DaclMutationGuard::acquire().unwrap();
                    prepare_acl_boundary(&mut lease, sid.0, &authority, 0).unwrap();
                    prepare_acl_boundary(
                        &mut lease,
                        sid.0,
                        &gitlink,
                        FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0,
                    )
                    .unwrap();
                    let snapshot = snapshot_dacl(root.path()).unwrap();
                    apply_acl_change(
                        &AclChange {
                            path: root.path().to_path_buf(),
                            permissions: WRITABLE_ROOT_ACCESS,
                            inherit: true,
                            mode: AclMode::Grant,
                        },
                        sid.0,
                        snapshot.handle.0,
                    )
                    .unwrap();
                    lease.original_dacls.push(snapshot);
                    lease
                };
                let first = prepare();
                let second = prepare();
                std::fs::write(authority.join("config.json"), "late authority").unwrap();
                let remaining = if reverse {
                    drop(second);
                    first
                } else {
                    drop(first);
                    second
                };
                let protected = snapshot_dacl(&authority).unwrap();
                assert!(protected.protected);
                assert_eq!(
                    inspect_boundary_acl(&protected, 0).unwrap(),
                    Some(originally_protected)
                );
                inspect_boundary_acl(&snapshot_dacl(&authority.join("config.json")).unwrap(), 0)
                    .unwrap();
                assert!(std::fs::rename(&authority, root.path().join("moved")).is_err());
                assert!(std::fs::rename(&gitlink, root.path().join("moved-gitlink")).is_err());
                drop(remaining);
                for before in [&authority_before, &gitlink_before, &root_before] {
                    let after = snapshot_dacl(&before.path).unwrap();
                    if before.protected != after.protected || before.acl != after.acl {
                        eprintln!(
                            "boundary restoration mismatch: before={before:?} after={after:?}"
                        );
                    }
                    assert_eq!(
                        before.protected,
                        after.protected,
                        "{}",
                        before.path.display()
                    );
                    assert_eq!(
                        before.acl,
                        after.acl,
                        "{} originally_protected={originally_protected} reverse={reverse}",
                        before.path.display()
                    );
                }
                // Releasing the final lease also releases replacement pins.
                std::fs::rename(&authority, root.path().join("moved")).unwrap();
                std::fs::rename(&gitlink, root.path().join("moved-gitlink")).unwrap();
            }
        }
    }

    #[test]
    fn suspended_child_is_reaped_when_setup_exits_before_job_assignment() {
        let program =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
        let application = wide(program.as_os_str());
        let mut line = command_line(&program, &["/C".to_string(), "exit 0".to_string()]);
        let environment = [0u16, 0u16];
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of_val(&startup.StartupInfo) as u32;
        let mut process_info = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                Some(environment.as_ptr().cast()),
                PCWSTR::null(),
                &startup.StartupInfo,
                &mut process_info,
            )
        }
        .unwrap();
        let handles = ProcessHandles {
            process: process_info.hProcess,
            thread: process_info.hThread,
        };
        let process = unsafe { GetCurrentProcess() };
        let mut duplicate = HANDLE::default();
        unsafe {
            DuplicateHandle(
                process,
                handles.process,
                process,
                &mut duplicate,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .unwrap();
        let wait_handle = OwnedHandle(duplicate);
        assert_eq!(
            unsafe { WaitForSingleObject(wait_handle.0, 0) },
            WAIT_TIMEOUT
        );
        // Mirrors an early return on CreateJobObject/AssignProcess failure.
        drop(handles);
        assert_eq!(
            unsafe { WaitForSingleObject(wait_handle.0, 5_000) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn boundary_inheritance_cleanup_follows_retained_object_after_rename() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join(".git");
        let moved = root.path().join("moved-gitlink");
        std::fs::write(&original, "gitdir: example").unwrap();
        // Share-delete makes the handle stale by name while keeping the actual
        // object alive. The production pin is additional protection, not an
        // excuse for cleanup to recover its authority from a fresh pathname.
        let before = snapshot_dacl(&original).unwrap();
        let acl = before.acl.as_ref().unwrap();
        win32(unsafe {
            SetSecurityInfo(
                before.handle.0,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(acl.as_ptr().cast::<ACL>()),
                None,
            )
        })
        .unwrap();
        std::fs::rename(&original, &moved).unwrap();
        assert!(!original.exists());
        restore_boundary_inheritance(&before, before.protected, before.acl.as_ref().unwrap())
            .unwrap();
        let after = snapshot_dacl(&moved).unwrap();
        assert_eq!(before.acl, after.acl);
        assert_eq!(before.protected, after.protected);
    }

    #[test]
    fn sealed_authority_rejects_preexisting_package_grants_in_descendants() {
        let root = tempfile::tempdir().unwrap();
        let authority = root.path().join(".kranz");
        std::fs::create_dir(&authority).unwrap();
        let file = authority.join("serve.token");
        std::fs::write(&file, "protected").unwrap();
        let (name, sid) = create_profile(false).unwrap();
        let mut lease = new_lease(name, None);
        let before = snapshot_dacl(&authority).unwrap();
        let file_acl = snapshot_dacl(&file).unwrap();
        let mut any_package =
            well_known_sid(WinBuiltinAnyPackageSid, "ALL APPLICATION PACKAGES").unwrap();
        let _guard = DaclMutationGuard::acquire().unwrap();
        apply_acl_change(
            &AclChange {
                path: file,
                permissions: FILE_GENERIC_READ.0,
                inherit: false,
                mode: AclMode::Grant,
            },
            PSID(any_package.as_mut_ptr().cast()),
            file_acl.handle.0,
        )
        .unwrap();
        assert!(prepare_acl_boundary(&mut lease, sid.0, &authority, 0).is_err());
        let after = snapshot_dacl(&authority).unwrap();
        assert_eq!(before.acl, after.acl);
        assert_eq!(before.protected, after.protected);
    }

    #[test]
    fn host_preparation_accepts_only_literal_local_drive_roots() {
        for root in [r"C:\", r"d:\"] {
            validate_host_preparation_root(Path::new(root)).unwrap();
        }
        for rejected in [r"C:", r"C:\Windows", r"\\server\share", r"C:\\", ""] {
            assert!(validate_host_preparation_root(Path::new(rejected)).is_err());
        }
    }

    #[test]
    fn null_device_descriptor_grants_both_appcontainer_package_groups() {
        assert!(NULL_DEVICE_TARGET_SDDL.contains(";;;AC)"));
        assert!(NULL_DEVICE_TARGET_SDDL.contains(";;;S-1-15-2-2)"));
        assert_eq!(NULL_DEVICE_ACCESS_MASK, 0x0012_01bf);
    }

    #[test]
    fn lpac_capability_policy_keeps_registry_read_and_network_explicit() {
        assert_eq!(
            launch_capability_policy(false),
            vec![LaunchCapability::RegistryRead]
        );
        assert_eq!(
            launch_capability_policy(true),
            vec![
                LaunchCapability::RegistryRead,
                LaunchCapability::InternetClient
            ]
        );
    }

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

    fn populate_toolchain_fixture(
        node_dir: &Path,
        npm_bin: &Path,
        cargo_dir: &Path,
        rustc_bin: &Path,
    ) {
        const ATTEMPTS: u32 = 4;
        for attempt in 1..=ATTEMPTS {
            let populate = || -> std::io::Result<()> {
                std::fs::create_dir_all(npm_bin)?;
                std::fs::create_dir_all(cargo_dir)?;
                std::fs::create_dir_all(rustc_bin)?;
                for name in ["node.exe", "npm.cmd", "npx.cmd"] {
                    std::fs::write(node_dir.join(name), "")?;
                }
                std::fs::write(npm_bin.join("npm-cli.js"), "")?;
                std::fs::write(cargo_dir.join("cargo.exe"), "")?;
                std::fs::write(rustc_bin.join("rustc.exe"), "")?;
                Ok(())
            };
            match populate() {
                Ok(()) => return,
                // Two hosted Windows full-suite runs returned
                // ERROR_PATH_NOT_FOUND while building this mock tree after
                // the live LPAC receipt, while identical runs passed. Fixture
                // setup is not the contract under test, so rebuild the whole
                // fixture only for that transient shape; all other errors
                // remain immediate.
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound && attempt < ATTEMPTS =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10 * u64::from(attempt)));
                }
                Err(error) => panic!(
                    "failed to populate toolchain fixture on attempt {attempt}/{ATTEMPTS}: {error}"
                ),
            }
        }
        unreachable!("the final fixture attempt returns or panics");
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
        populate_toolchain_fixture(&node_dir, &npm_bin, &cargo_dir, &rustc_bin);

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
    fn installed_rust_toolchain_bin_leads_the_contained_search_path() {
        let root = tempfile::tempdir().expect("temp toolchain root");
        let proxy_bin = root.path().join("cargo-home").join("bin");
        let toolchain = root.path().join("rustup").join("toolchains").join("stable");
        let toolchain_bin = toolchain.join("bin");
        std::fs::create_dir_all(&proxy_bin).expect("proxy bin");
        std::fs::create_dir_all(&toolchain_bin).expect("toolchain bin");

        let dirs = ordered_search_path_dirs(
            vec![proxy_bin.clone(), toolchain_bin.clone(), proxy_bin],
            Some(toolchain_bin.clone()),
        );

        assert_eq!(
            comparable_path(dirs.first().expect("preferred bin")),
            comparable_path(&toolchain_bin)
        );
        assert_eq!(
            dirs.iter()
                .filter(|dir| comparable_path(dir) == comparable_path(&toolchain_bin))
                .count(),
            1
        );
    }

    #[test]
    fn redirected_appcontainer_temp_is_materialized_only_under_a_writable_root() {
        let root = tempfile::tempdir().expect("temp profile root");
        let scratch = root.path().join("scratch");
        let local_app_data = scratch.join("home").join("AppData").join("Local");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&local_app_data).expect("scratch LOCALAPPDATA");
        std::fs::create_dir_all(&outside).expect("outside LOCALAPPDATA");

        let mut env = HashMap::new();
        env.insert(
            "localappdata".to_string(),
            local_app_data.to_string_lossy().into_owned(),
        );
        let temp = prepare_redirected_profile_temp(
            "kranz.production.receipt",
            std::slice::from_ref(&scratch),
            &env,
        )
        .expect("redirected AppContainer temp");
        assert_eq!(
            temp,
            std::fs::canonicalize(&local_app_data)
                .expect("canonical LOCALAPPDATA")
                .join("Packages")
                .join("kranz.production.receipt")
                .join("AC")
                .join("Temp")
        );
        assert!(temp.is_dir());

        env.insert(
            "LOCALAPPDATA".to_string(),
            outside.to_string_lossy().into_owned(),
        );
        env.remove("localappdata");
        let error = prepare_redirected_profile_temp(
            "kranz.production.receipt",
            std::slice::from_ref(&scratch),
            &env,
        )
        .expect_err("outside LOCALAPPDATA must fail closed");
        assert!(error
            .to_string()
            .contains("outside the writable sandbox roots"));
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
    fn local_volume_root_normalizes_disk_paths_and_refuses_unc_shares() {
        assert_eq!(
            local_volume_root(Path::new(r"\\?\C:\hostedtoolcache\windows\node\node.exe")),
            Some(PathBuf::from(r"C:\"))
        );
        assert_eq!(
            local_volume_root(Path::new(r"D:\gate\worktree\node.exe")),
            Some(PathBuf::from(r"D:\"))
        );
        assert_eq!(
            local_volume_root(Path::new(r"\\server\share\node.exe")),
            None
        );
    }

    /// Regression: cmd.exe sets `=ExitCode` after its first command, so every
    /// enforced Windows session launched from a cmd prompt used to fail closed
    /// with "cleared AppContainer environment contains an invalid key/value".
    /// pwsh does not set it, which is why hosted CI never reproduced this.
    #[test]
    fn cmd_exe_pseudo_variables_are_dropped_while_drive_entries_survive() {
        for dropped in ["=ExitCode", "=ExitCodeAscii", "=::", "a=b"] {
            assert!(
                !inheritable_environment_key(OsStr::new(dropped)),
                "{dropped} must not be inherited into an explicit block"
            );
        }
        for kept in ["=C:", "=d:", "PATH", "USERPROFILE", ""] {
            assert!(
                inheritable_environment_key(OsStr::new(kept)),
                "{kept} must survive inheritance"
            );
        }
        assert!(is_drive_current_directory_key(OsStr::new("=Z:")));
        assert!(!is_drive_current_directory_key(OsStr::new("=ExitCode")));
        assert!(!is_drive_current_directory_key(OsStr::new("=1:")));
    }

    #[test]
    fn environment_block_replaces_path_with_the_contained_search_path() {
        let encoded = environment_block(
            Path::new(r"D:\gate\worktree"),
            Some(r"C:\Windows\System32;D:\node"),
            Some(Path::new(
                r"C:\Users\runner\.rustup\toolchains\stable-x86_64-pc-windows-msvc",
            )),
        )
        .expect("environment block");
        let text = String::from_utf16(&encoded).expect("the environment block is valid UTF-16");
        let path = text
            .split('\0')
            .find(|entry| entry.to_ascii_uppercase().starts_with("PATH="));
        assert_eq!(path, Some(r"PATH=C:\Windows\System32;D:\node"));
        assert!(text.split('\0').any(|entry| entry
            == r"RUSTUP_TOOLCHAIN=C:\Users\runner\.rustup\toolchains\stable-x86_64-pc-windows-msvc"));
        assert!(text
            .split('\0')
            .any(|entry| entry == "RUSTUP_AUTO_INSTALL=0"));
    }
}
