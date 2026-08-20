//! Windows native-containment capability probe and AppContainer/LPAC receipts.
//!
//! Microsoft's `Experimental_CreateProcessInSandbox` contract is experimental
//! and its required `SandboxSpec.fbs` schema is not publicly available.
//! Guessing that security-critical wire format would turn API presence into a
//! false safety claim. The probe therefore records only host/API capability.
//!
//! The Windows-only tests exercise both the regular-AppContainer primitive
//! fixture and the stable production LPAC launcher against disposable
//! directories. The production path is integrated with cleared environment
//! construction, authority masks, bounded output, validator read denial, gate
//! wrapping, and Job Object supervision.
//!
//! DLL discovery follows Microsoft's documented pattern exactly: load
//! `processmodel.dll` from System32 only, then resolve the experimental export
//! dynamically. The restricted search scope prevents a worker-controlled DLL
//! on the current directory or `PATH` from spoofing capability evidence.

use serde::Serialize;
use std::path::Path;

pub const PROCESS_MODEL_DLL: &str = "processmodel.dll";
pub const PROCESS_SANDBOX_EXPORT: &str = "Experimental_CreateProcessInSandbox";
pub const EXPERIMENTAL_SPEC_VERSION: &str = "0.1.0";
pub const DLL_SEARCH_SCOPE: &str = "system32-only";

/// Kernel version observed without the manifest-sensitive `GetVersionEx`
/// compatibility behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

impl std::fmt::Display for WindowsVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.build)
    }
}

/// What the host proved about Microsoft's experimental process-sandbox API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExperimentalApiStatus {
    NotWindows,
    DllUnavailable,
    ExportUnavailable,
    ExperimentalApiAvailable,
}

impl ExperimentalApiStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotWindows => "not-windows",
            Self::DllUnavailable => "dll-unavailable",
            Self::ExportUnavailable => "export-unavailable",
            Self::ExperimentalApiAvailable => "experimental-api-available",
        }
    }
}

/// Stable, secret-free report suitable for CLI JSON and CI evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsSandboxProbeReport {
    pub host_os: &'static str,
    pub windows_version: Option<WindowsVersion>,
    pub dll: &'static str,
    pub export: &'static str,
    pub dll_search_scope: &'static str,
    pub experimental_spec_version: &'static str,
    pub api_status: ExperimentalApiStatus,
    /// HRESULT from the System32-only DLL load, when that load failed.
    pub load_error_hresult: Option<i32>,
    /// True when this build ships a production Windows containment backend.
    pub production_enabled: bool,
    pub decision: &'static str,
}

impl WindowsSandboxProbeReport {
    pub fn experimental_api_available(&self) -> bool {
        self.api_status == ExperimentalApiStatus::ExperimentalApiAvailable
    }

    pub fn render_text(&self) -> String {
        let version = self
            .windows_version
            .map(|version| version.to_string())
            .unwrap_or_else(|| "unavailable".to_string());
        let load_error = self
            .load_error_hresult
            .map(|code| format!("0x{:08x}", code as u32))
            .unwrap_or_else(|| "none".to_string());
        format!(
            "Windows native containment probe\n\
             host OS: {}\n\
             Windows version: {version}\n\
             DLL: {} ({})\n\
             export: {}\n\
             API status: {}\n\
             load error HRESULT: {load_error}\n\
             experimental spec: {}\n\
             production enabled: {}\n\
             decision: {}\n",
            self.host_os,
            self.dll,
            self.dll_search_scope,
            self.export,
            self.api_status.as_str(),
            self.experimental_spec_version,
            self.production_enabled,
            self.decision,
        )
    }
}

/// Probe the current host without creating an AppContainer profile, changing
/// ACLs, or spawning a child. Absence is a supported result.
pub fn probe() -> WindowsSandboxProbeReport {
    platform::probe()
}

/// Apply and verify the persistent metadata-only AppContainer host ACEs on a
/// literal local drive root. This mutation is Windows-only and requires an
/// elevated token; ordinary launch paths never call it.
#[cfg(windows)]
pub fn prepare_appcontainer_host(root: &Path) -> std::result::Result<bool, String> {
    crate::appcontainer_windows::prepare_appcontainer_host(root).map_err(|error| error.to_string())
}

#[cfg(not(windows))]
pub fn prepare_appcontainer_host(_root: &Path) -> std::result::Result<bool, String> {
    Err("AppContainer host preparation is available only on Windows".to_string())
}

/// Private production-launcher dispatch used by the `kranz` binary before
/// ordinary CLI initialization. Kept on this public platform module so the
/// engine's Windows-only unsafe implementation remains crate-private.
#[cfg(windows)]
pub fn internal_launcher_requested() -> bool {
    crate::appcontainer_windows::internal_launcher_requested()
}

#[cfg(windows)]
pub fn run_internal_launcher() -> std::result::Result<u32, String> {
    crate::appcontainer_windows::run_internal_launcher()
}

#[cfg(windows)]
pub fn internal_self_test_requested() -> bool {
    crate::appcontainer_windows::internal_self_test_requested()
}

#[cfg(windows)]
pub fn run_production_hostile_self_test() -> std::result::Result<String, String> {
    crate::appcontainer_windows::run_production_hostile_self_test()
}

#[cfg(windows)]
pub fn internal_gate_self_test_requested() -> bool {
    crate::appcontainer_windows::internal_gate_self_test_requested()
}

#[cfg(windows)]
pub fn run_production_gate_self_test() -> std::result::Result<String, String> {
    crate::appcontainer_windows::run_production_gate_self_test()
}

#[cfg(windows)]
pub fn internal_hostile_child_requested() -> bool {
    crate::appcontainer_windows::internal_hostile_child_requested()
}

#[cfg(windows)]
pub fn run_internal_hostile_child() -> std::result::Result<(), String> {
    crate::appcontainer_windows::run_internal_hostile_child()
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::core::{s, w};
    use windows::Win32::Foundation::{FreeLibrary, HMODULE};
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(version: *mut OSVERSIONINFOW) -> i32;
    }

    struct Library(HMODULE);

    impl Drop for Library {
        fn drop(&mut self) {
            // SAFETY: this guard exclusively owns the module handle returned
            // by LoadLibraryExW and releases it exactly once.
            let _ = unsafe { FreeLibrary(self.0) };
        }
    }

    fn windows_version() -> Option<WindowsVersion> {
        let mut version = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: RtlGetVersion writes exactly the OSVERSIONINFOW structure
        // whose initialized size is supplied above. A negative NTSTATUS is a
        // supported unknown-version result, never a reason to enable anything.
        let status = unsafe { RtlGetVersion(&mut version) };
        (status >= 0).then_some(WindowsVersion {
            major: version.dwMajorVersion,
            minor: version.dwMinorVersion,
            build: version.dwBuildNumber,
        })
    }

    pub(super) fn probe() -> WindowsSandboxProbeReport {
        let version = windows_version();
        // SAFETY: the literal is NUL-terminated and the System32-only flag is
        // Microsoft's documented loading pattern for this experimental API.
        let library = match unsafe {
            LoadLibraryExW(w!("processmodel.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
        } {
            Ok(module) => Library(module),
            Err(error) => {
                return WindowsSandboxProbeReport {
                    host_os: "windows",
                    windows_version: version,
                    dll: PROCESS_MODEL_DLL,
                    export: PROCESS_SANDBOX_EXPORT,
                    dll_search_scope: DLL_SEARCH_SCOPE,
                    experimental_spec_version: EXPERIMENTAL_SPEC_VERSION,
                    api_status: ExperimentalApiStatus::DllUnavailable,
                    load_error_hresult: Some(error.code().0),
                    production_enabled: true,
                    decision: "stable LPAC enforcement is enabled; the experimental System32 DLL is unavailable and unused",
                };
            }
        };

        // SAFETY: `library` is a live System32 module handle and the export
        // name is a NUL-terminated ASCII literal. We test presence only and
        // never transmute or call the experimental function pointer.
        let export =
            unsafe { GetProcAddress(library.0, s!("Experimental_CreateProcessInSandbox")) };
        let (api_status, decision) = if export.is_some() {
            (
                ExperimentalApiStatus::ExperimentalApiAvailable,
                "stable LPAC enforcement is enabled; the experimental API is detected but unused",
            )
        } else {
            (
                ExperimentalApiStatus::ExportUnavailable,
                "stable LPAC enforcement is enabled; processmodel.dll does not export the unused experimental API",
            )
        };
        WindowsSandboxProbeReport {
            host_os: "windows",
            windows_version: version,
            dll: PROCESS_MODEL_DLL,
            export: PROCESS_SANDBOX_EXPORT,
            dll_search_scope: DLL_SEARCH_SCOPE,
            experimental_spec_version: EXPERIMENTAL_SPEC_VERSION,
            api_status,
            load_error_hresult: None,
            production_enabled: true,
            decision,
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) fn probe() -> WindowsSandboxProbeReport {
        WindowsSandboxProbeReport {
            host_os: std::env::consts::OS,
            windows_version: None,
            dll: PROCESS_MODEL_DLL,
            export: PROCESS_SANDBOX_EXPORT,
            dll_search_scope: DLL_SEARCH_SCOPE,
            experimental_spec_version: EXPERIMENTAL_SPEC_VERSION,
            api_status: ExperimentalApiStatus::NotWindows,
            load_error_hresult: None,
            production_enabled: false,
            decision:
                "probe not run: native Windows containment can only be inspected on a Windows host",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_the_platform_production_posture_independently_of_experimental_api() {
        let report = probe();
        assert_eq!(report.production_enabled, cfg!(windows));
        assert_eq!(report.dll_search_scope, "system32-only");
        assert_eq!(report.experimental_spec_version, "0.1.0");
        assert!(report
            .render_text()
            .contains(&format!("production enabled: {}", cfg!(windows))));
    }

    #[cfg(windows)]
    #[test]
    fn windows_experimental_process_sandbox_probe_reports_capability() {
        let report = probe();
        println!(
            "{}",
            serde_json::to_string(&report).expect("probe report must serialize")
        );
        assert_eq!(report.host_os, "windows");
        assert!(report.windows_version.is_some());
        assert_ne!(report.api_status, ExperimentalApiStatus::NotWindows);
        assert!(report.production_enabled);
        if report.experimental_api_available() {
            assert_eq!(
                report.api_status,
                ExperimentalApiStatus::ExperimentalApiAvailable
            );
            assert!(report.load_error_hresult.is_none());
        }
    }

    /// M7 Windows containment, phase 3: prove the stable AppContainer token
    /// and ACL model on a real Windows host without changing the real checkout.
    ///
    /// The parent fixture creates a unique profile and four disposable roots,
    /// then copies this test executable into a read/execute-only toolchain root.
    /// The child must read that root, write only the worktree and private
    /// scratch, fail to write a sibling root, and fail to connect to a live
    /// loopback listener because no network capability is supplied.
    #[cfg(windows)]
    #[test]
    fn windows_appcontainer_hostile_fixture_denies_out_of_root_write_and_network() {
        appcontainer_fixture::run_parent().expect("AppContainer hostile fixture must pass");
    }

    /// Re-entered by [`windows_appcontainer_hostile_fixture_denies_out_of_root_write_and_network`]
    /// inside the AppContainer. An ordinary workspace test run has no manifest
    /// in its current directory, so the standalone instance is an intentional
    /// no-op; CI gates the parent test above by its collision-free exact name.
    #[cfg(windows)]
    #[test]
    fn windows_appcontainer_hostile_child() {
        appcontainer_fixture::run_child_if_requested()
            .expect("AppContainer hostile child must produce its receipt");
    }

    #[cfg(windows)]
    mod appcontainer_fixture {
        use serde::{Deserialize, Serialize};
        use std::io;
        use std::net::{SocketAddr, TcpListener, TcpStream};
        use std::os::windows::ffi::OsStrExt;
        use std::path::{Path, PathBuf};
        use std::ptr::null_mut;
        use std::time::Duration;
        use windows::core::{PCWSTR, PWSTR};
        use windows::Win32::Foundation::{
            CloseHandle, LocalFree, HANDLE, HLOCAL, WAIT_OBJECT_0, WAIT_TIMEOUT,
        };
        use windows::Win32::Security::Authorization::{
            GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
            GRANT_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        };
        use windows::Win32::Security::Isolation::{
            CreateAppContainerProfile, DeleteAppContainerProfile,
        };
        use windows::Win32::Security::{
            FreeSid, GetTokenInformation, TokenIsAppContainer, ACL, CONTAINER_INHERIT_ACE,
            DACL_SECURITY_INFORMATION, NO_INHERITANCE, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR,
            PSID, SECURITY_CAPABILITIES, TOKEN_QUERY,
        };
        use windows::Win32::Storage::FileSystem::{
            FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        };
        use windows::Win32::System::Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
            InitializeProcThreadAttributeList, OpenProcessToken, ResumeThread,
            UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED,
            EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTUPINFOEXW,
        };

        const MANIFEST_NAME: &str = "kranz-appcontainer-fixture.json";
        const CHILD_TEST: &str = "sandbox_windows::tests::windows_appcontainer_hostile_child";
        const CHILD_TIMEOUT_MS: u32 = 30_000;

        #[derive(Debug, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct FixtureManifest {
            toolchain_marker: PathBuf,
            toolchain_denied_write: PathBuf,
            worktree_write: PathBuf,
            scratch_write: PathBuf,
            outside_write: PathBuf,
            loopback_addr: SocketAddr,
            receipt: PathBuf,
        }

        #[derive(Debug, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct HostileReceipt {
            token_is_appcontainer: bool,
            toolchain_read: bool,
            toolchain_write_denied: bool,
            worktree_write: bool,
            scratch_write: bool,
            outside_write_denied: bool,
            network_denied: bool,
        }

        struct Profile {
            name: Vec<u16>,
            sid: PSID,
            deleted: bool,
        }

        impl Profile {
            fn create() -> anyhow::Result<Self> {
                let name = format!("kranz.phase3.{}", uuid::Uuid::new_v4());
                let name = wide(&name);
                let display_name = wide("Kranz phase 3 fixture");
                let description = wide("Disposable native-containment proof");
                // SAFETY: all strings are live, NUL-terminated UTF-16 buffers;
                // zero capabilities is deliberate so network remains denied.
                let sid = unsafe {
                    CreateAppContainerProfile(
                        PCWSTR(name.as_ptr()),
                        PCWSTR(display_name.as_ptr()),
                        PCWSTR(description.as_ptr()),
                        None,
                    )?
                };
                Ok(Self {
                    name,
                    sid,
                    deleted: false,
                })
            }

            fn remove(mut self) -> anyhow::Result<()> {
                // SAFETY: `name` is the same live profile moniker passed to
                // CreateAppContainerProfile and no child process remains.
                unsafe { DeleteAppContainerProfile(PCWSTR(self.name.as_ptr()))? };
                self.deleted = true;
                Ok(())
            }
        }

        impl Drop for Profile {
            fn drop(&mut self) {
                if !self.deleted {
                    // Best-effort unwind cleanup; the success path calls
                    // `remove` explicitly so CI also proves profile deletion.
                    let _ = unsafe { DeleteAppContainerProfile(PCWSTR(self.name.as_ptr())) };
                }
                // SAFETY: CreateAppContainerProfile returned this SID and the
                // contract requires exactly one FreeSid call by the owner.
                unsafe {
                    FreeSid(self.sid);
                }
            }
        }

        struct LocalAllocation(HLOCAL);

        impl Drop for LocalAllocation {
            fn drop(&mut self) {
                // SAFETY: the wrapped pointer came from GetNamedSecurityInfoW
                // or SetEntriesInAclW and is freed exactly once with LocalFree.
                unsafe {
                    LocalFree(Some(self.0));
                }
            }
        }

        struct AttributeList {
            list: LPPROC_THREAD_ATTRIBUTE_LIST,
            _storage: Vec<usize>,
        }

        impl AttributeList {
            fn security_capabilities(value: &SECURITY_CAPABILITIES) -> anyhow::Result<Self> {
                let mut bytes = 0usize;
                // The sizing call intentionally fails with insufficient buffer
                // while returning the required byte count.
                let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut bytes) };
                anyhow::ensure!(bytes > 0, "attribute-list sizing returned zero bytes");
                let words = bytes.div_ceil(std::mem::size_of::<usize>());
                let mut storage = vec![0usize; words];
                let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
                // SAFETY: the usize allocation is suitably aligned and holds
                // at least the byte count returned by the sizing call.
                unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut bytes)? };
                let result = Self {
                    list,
                    _storage: storage,
                };
                // SAFETY: `value` remains live through CreateProcessW and its
                // exact type/size match the documented attribute contract.
                unsafe {
                    UpdateProcThreadAttribute(
                        result.list,
                        0,
                        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                        Some((value as *const SECURITY_CAPABILITIES).cast()),
                        std::mem::size_of::<SECURITY_CAPABILITIES>(),
                        None,
                        None,
                    )?
                };
                Ok(result)
            }
        }

        impl Drop for AttributeList {
            fn drop(&mut self) {
                // SAFETY: InitializeProcThreadAttributeList initialized this
                // allocation and the owner deletes the list exactly once.
                unsafe { DeleteProcThreadAttributeList(self.list) };
            }
        }

        struct ProcessHandles {
            process: HANDLE,
            thread: HANDLE,
        }

        impl Drop for ProcessHandles {
            fn drop(&mut self) {
                // SAFETY: CreateProcessW returned both handles; this guard owns
                // and closes each one exactly once.
                unsafe {
                    let _ = CloseHandle(self.thread);
                    let _ = CloseHandle(self.process);
                }
            }
        }

        struct OwnedHandle(HANDLE);

        impl Drop for OwnedHandle {
            fn drop(&mut self) {
                // SAFETY: OpenProcessToken returned this owned token handle.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }

        fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
            value.as_ref().encode_wide().chain(Some(0)).collect()
        }

        fn win32(status: windows::Win32::Foundation::WIN32_ERROR) -> anyhow::Result<()> {
            status.ok().map_err(anyhow::Error::from)
        }

        fn grant_path(
            path: &Path,
            sid: PSID,
            permissions: u32,
            inherit: bool,
        ) -> anyhow::Result<()> {
            let path = wide(path.as_os_str());
            let mut old_acl: *mut ACL = null_mut();
            let mut security_descriptor = PSECURITY_DESCRIPTOR::default();
            // SAFETY: the path buffer and output pointers are valid. The
            // returned security descriptor owns `old_acl` and is LocalFree'd.
            win32(unsafe {
                GetNamedSecurityInfoW(
                    PCWSTR(path.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(&mut old_acl),
                    None,
                    &mut security_descriptor,
                )
            })?;
            let _security_descriptor = LocalAllocation(HLOCAL(security_descriptor.0));

            let entry = EXPLICIT_ACCESS_W {
                grfAccessPermissions: permissions,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: if inherit {
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
            // SAFETY: `entry` contains the live profile SID; `old_acl` stays
            // alive through the owning security descriptor above.
            win32(unsafe { SetEntriesInAclW(Some(&[entry]), Some(old_acl), &mut new_acl) })?;
            let _new_acl = LocalAllocation(HLOCAL(new_acl.cast()));
            // SAFETY: all pointers remain live for this call. This merges one
            // AppContainer ACE into the existing DACL instead of replacing the
            // user's access, and only disposable fixture paths are modified.
            win32(unsafe {
                SetNamedSecurityInfoW(
                    PCWSTR(path.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(new_acl),
                    None,
                )
            })
        }

        fn is_appcontainer_process() -> anyhow::Result<bool> {
            let mut access_handle = HANDLE::default();
            // SAFETY: GetCurrentProcess is a valid pseudohandle and the output
            // receives one owned process access-token handle.
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut access_handle)? };
            let access_handle = OwnedHandle(access_handle);
            let mut value = 0u32;
            let mut returned = 0u32;
            // SAFETY: TokenIsAppContainer returns one u32 into the exact-size
            // initialized output buffer supplied here.
            unsafe {
                GetTokenInformation(
                    access_handle.0,
                    TokenIsAppContainer,
                    Some((&mut value as *mut u32).cast()),
                    std::mem::size_of::<u32>() as u32,
                    &mut returned,
                )?
            };
            anyhow::ensure!(returned as usize == std::mem::size_of::<u32>());
            Ok(value != 0)
        }

        fn quote_argument(value: &std::ffi::OsStr) -> String {
            let value = value.to_string_lossy();
            format!("\"{}\"", value.replace('"', "\\\""))
        }

        fn launch(executable: &Path, cwd: &Path, sid: PSID) -> anyhow::Result<u32> {
            let security = SECURITY_CAPABILITIES {
                AppContainerSid: sid,
                Capabilities: null_mut(),
                CapabilityCount: 0,
                Reserved: 0,
            };
            let attributes = AttributeList::security_capabilities(&security)?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            startup.lpAttributeList = attributes.list;

            let application = wide(executable.as_os_str());
            let cwd = wide(cwd.as_os_str());
            let command_line = format!(
                "{} {} --exact --nocapture",
                quote_argument(executable.as_os_str()),
                quote_argument(std::ffi::OsStr::new(CHILD_TEST)),
            );
            let mut command_line = wide(command_line);
            let mut process_info = PROCESS_INFORMATION::default();
            // SAFETY: every buffer and structure remains live through the call;
            // the mutable command line satisfies CreateProcessW's contract.
            unsafe {
                CreateProcessW(
                    PCWSTR(application.as_ptr()),
                    Some(PWSTR(command_line.as_mut_ptr())),
                    None,
                    None,
                    false,
                    CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
                    None,
                    PCWSTR(cwd.as_ptr()),
                    &startup.StartupInfo,
                    &mut process_info,
                )?
            };
            let handles = ProcessHandles {
                process: process_info.hProcess,
                thread: process_info.hThread,
            };

            // Fail closed: assigning the still-suspended process closes the
            // spawn-before-supervision race. No hostile instruction runs until
            // the kill-on-close Job Object owns the process tree.
            let job =
                crate::backend_claude::win_job::JobHandle::create_and_assign(handles.process.0)?;
            // SAFETY: `handles.thread` is the suspended primary thread.
            anyhow::ensure!(unsafe { ResumeThread(handles.thread) } != u32::MAX);

            // SAFETY: the process handle remains live in `handles`.
            let wait = unsafe { WaitForSingleObject(handles.process, CHILD_TIMEOUT_MS) };
            if wait == WAIT_TIMEOUT {
                job.kill();
                anyhow::bail!("AppContainer fixture timed out after {CHILD_TIMEOUT_MS}ms");
            }
            anyhow::ensure!(
                wait == WAIT_OBJECT_0,
                "WaitForSingleObject returned {wait:?}"
            );
            let mut exit_code = 0u32;
            // SAFETY: the signaled process handle is valid and exit_code is an
            // exact initialized output buffer.
            unsafe { GetExitCodeProcess(handles.process, &mut exit_code)? };
            Ok(exit_code)
        }

        pub(super) fn run_parent() -> anyhow::Result<()> {
            let root = tempfile::tempdir()?;
            let toolchain = root.path().join("toolchain");
            let worktree = root.path().join("worktree");
            let scratch = root.path().join("scratch");
            let outside = root.path().join("outside");
            for path in [&toolchain, &worktree, &scratch, &outside] {
                std::fs::create_dir(path)?;
            }

            let profile = Profile::create()?;
            let read_execute = FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0;
            let read_write_execute = read_execute | FILE_GENERIC_WRITE.0;
            // The parent root only needs traversal; non-inheriting read/execute
            // exposes no child object whose own DACL lacks an AppContainer ACE.
            grant_path(root.path(), profile.sid, read_execute, false)?;
            grant_path(&toolchain, profile.sid, read_execute, true)?;
            grant_path(&worktree, profile.sid, read_write_execute, true)?;
            grant_path(&scratch, profile.sid, read_write_execute, true)?;

            let current_exe = std::env::current_exe()?;
            let executable = toolchain.join(
                current_exe
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("test executable has no file name"))?,
            );
            std::fs::copy(&current_exe, &executable)?;
            let toolchain_marker = toolchain.join("read-only-marker.txt");
            std::fs::write(&toolchain_marker, "kranz-appcontainer-phase3")?;

            let listener = TcpListener::bind("127.0.0.1:0")?;
            let manifest = FixtureManifest {
                toolchain_marker,
                toolchain_denied_write: toolchain.join("must-not-write.txt"),
                worktree_write: worktree.join("allowed-worktree.txt"),
                scratch_write: scratch.join("allowed-scratch.txt"),
                outside_write: outside.join("must-not-write.txt"),
                loopback_addr: listener.local_addr()?,
                receipt: scratch.join("receipt.json"),
            };
            let manifest_path = worktree.join(MANIFEST_NAME);
            std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;

            let exit_code = launch(&executable, &worktree, profile.sid)?;
            anyhow::ensure!(exit_code == 0, "AppContainer child exited {exit_code}");
            let receipt: HostileReceipt =
                serde_json::from_slice(&std::fs::read(&manifest.receipt)?)?;
            println!("{}", serde_json::to_string(&receipt)?);
            anyhow::ensure!(
                receipt.token_is_appcontainer,
                "child token was not AppContainer"
            );
            anyhow::ensure!(receipt.toolchain_read, "read-only toolchain was unreadable");
            anyhow::ensure!(receipt.toolchain_write_denied, "toolchain write escaped");
            anyhow::ensure!(receipt.worktree_write, "worktree write was denied");
            anyhow::ensure!(receipt.scratch_write, "private scratch write was denied");
            anyhow::ensure!(receipt.outside_write_denied, "out-of-root write escaped");
            anyhow::ensure!(receipt.network_denied, "network access escaped");
            anyhow::ensure!(!manifest.toolchain_denied_write.exists());
            anyhow::ensure!(!manifest.outside_write.exists());
            profile.remove()?;
            Ok(())
        }

        pub(super) fn run_child_if_requested() -> anyhow::Result<()> {
            let manifest_path = std::env::current_dir()?.join(MANIFEST_NAME);
            if !manifest_path.is_file() {
                return Ok(());
            }
            let manifest: FixtureManifest =
                serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
            let toolchain_read = std::fs::read_to_string(&manifest.toolchain_marker)
                .map(|value| value == "kranz-appcontainer-phase3")
                .unwrap_or(false);
            let toolchain_write_denied =
                std::fs::write(&manifest.toolchain_denied_write, "escape").is_err();
            let worktree_write = std::fs::write(&manifest.worktree_write, "allowed").is_ok();
            let scratch_write = std::fs::write(&manifest.scratch_write, "allowed").is_ok();
            let outside_write_denied = std::fs::write(&manifest.outside_write, "escape").is_err();
            let network_denied =
                TcpStream::connect_timeout(&manifest.loopback_addr, Duration::from_secs(2))
                    .is_err();
            let receipt = HostileReceipt {
                token_is_appcontainer: is_appcontainer_process()?,
                toolchain_read,
                toolchain_write_denied,
                worktree_write,
                scratch_write,
                outside_write_denied,
                network_denied,
            };
            std::fs::write(&manifest.receipt, serde_json::to_vec_pretty(&receipt)?)
                .map_err(|error| io::Error::new(error.kind(), format!("write receipt: {error}")))?;
            Ok(())
        }
    }
}
