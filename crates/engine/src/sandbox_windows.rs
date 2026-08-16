//! Windows native-containment capability probe.
//!
//! This module deliberately does not launch a sandboxed process. Microsoft's
//! `Experimental_CreateProcessInSandbox` contract is experimental and its
//! required `SandboxSpec.fbs` schema is not publicly available. Guessing that
//! security-critical wire format would turn API presence into a false safety
//! claim. The probe therefore records only host/API capability while Windows
//! enforcement remains fail-closed in [`crate::sandbox`].
//!
//! DLL discovery follows Microsoft's documented pattern exactly: load
//! `processmodel.dll` from System32 only, then resolve the experimental export
//! dynamically. The restricted search scope prevents a worker-controlled DLL
//! on the current directory or `PATH` from spoofing capability evidence.

use serde::Serialize;

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
    /// Always false until a real hostile-host receipt proves the launcher.
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
                    production_enabled: false,
                    decision: "native enforcement remains fail-closed: the experimental System32 DLL is unavailable",
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
                "experimental API detected; production launch remains disabled until the schema and hostile-host receipt are available",
            )
        } else {
            (
                ExperimentalApiStatus::ExportUnavailable,
                "native enforcement remains fail-closed: processmodel.dll does not export the experimental sandbox API",
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
            production_enabled: false,
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
    fn probe_never_enables_an_unproved_experimental_api() {
        let report = probe();
        assert!(!report.production_enabled);
        assert_eq!(report.dll_search_scope, "system32-only");
        assert_eq!(report.experimental_spec_version, "0.1.0");
        assert!(report.render_text().contains("production enabled: false"));
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
        assert!(!report.production_enabled);
        if report.experimental_api_available() {
            assert_eq!(
                report.api_status,
                ExperimentalApiStatus::ExperimentalApiAvailable
            );
            assert!(report.load_error_hresult.is_none());
        }
    }
}
