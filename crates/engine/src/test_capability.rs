//! Runtime-gated tests: skip loudly, and fail where the capability is required.
//!
//! A test that returns early because a tool is missing prints `ok`. libtest
//! reports it identically to a test that did the work, and the `eprintln!`
//! explaining the skip is captured and never shown unless the test fails. So a
//! capability can quietly stop being exercised while CI keeps reporting
//! success — indefinitely, and invisibly.
//!
//! That is not hypothetical. `command_available` did not consult `PATHEXT`, so
//! `sandbox_container::detect()` never found `docker.exe` and the Windows
//! container tests skipped for the life of that CI lane. When the lookup was
//! fixed they ran for the first time and immediately failed on two real bugs.
//! The lane had been green throughout.
//!
//! [`AGENTS.md`] rule 5 already guards the neighbouring shape — a test FILTER
//! matching zero tests — with `grep -qE 'test result: ok\. [1-9]'`. This module
//! guards the other one.
//!
//! # Contract
//!
//! Call [`skip`] instead of a bare `eprintln!` + `return`. It emits a stable,
//! greppable marker, and PANICS when the capability appears in
//! `KRANZ_REQUIRED_CAPABILITIES` — so a platform that is supposed to have a
//! tool fails loudly the moment it stops having one, instead of silently
//! reverting to skips.
//!
//! CI declares per-platform expectations rather than asserting a skip list
//! after the fact: ubuntu requires `git,bwrap,container`, macOS requires
//! `git,sandbox-exec,container`, Windows requires `git`. Windows deliberately
//! omits `container` — the provider is macOS/Linux only and session and gate
//! resolution already fail closed there.

/// Environment variable naming the capabilities that MUST be present.
/// Comma-separated; matching is exact and case-insensitive.
pub const REQUIRED_CAPABILITIES_ENV: &str = "KRANZ_REQUIRED_CAPABILITIES";

/// Prefix on every skip line, so a run can be searched for what it did not do.
pub const SKIP_MARKER: &str = "KRANZ_TEST_SKIP";

/// Optional file the skip ledger is appended to.
///
/// Printing alone does NOT make a skip visible: libtest captures stdout and
/// stderr for a PASSING test, and a skipping test passes, so the marker is
/// swallowed in exactly the case that matters. `--nocapture` would surface it
/// but floods the log and interleaves badly under parallelism. A file survives
/// capture, so CI can print the ledger after the suite and show what the run
/// did not exercise.
pub const SKIP_LOG_ENV: &str = "KRANZ_SKIP_LOG";

/// Capability names. Constants rather than loose strings so a typo in a test
/// cannot silently opt out of the requirement it meant to declare.
pub mod capability {
    /// `git` on PATH. Every mission test needs it; a skip here is close to a
    /// total loss of coverage.
    pub const GIT: &str = "git";
    /// A container runtime (`docker`/`podman`/`nerdctl`/`container`) whose
    /// daemon can run the shipped Linux images.
    pub const CONTAINER: &str = "container";
    /// Linux `bwrap` — the tier-2 sandbox backend.
    pub const BWRAP: &str = "bwrap";
    /// macOS `sandbox-exec` — the Seatbelt backend.
    pub const SANDBOX_EXEC: &str = "sandbox-exec";
    /// POSIX `grep`, for the assertions that are ABOUT its exit-status
    /// semantics and cannot be rewritten portably.
    pub const GREP: &str = "grep";
}

/// True when `capability` is listed in [`REQUIRED_CAPABILITIES_ENV`].
pub fn is_required(capability: &str) -> bool {
    std::env::var(REQUIRED_CAPABILITIES_ENV)
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .any(|entry| entry.eq_ignore_ascii_case(capability))
        })
        .unwrap_or(false)
}

/// Record that a runtime-gated test is skipping for want of `capability`.
///
/// Panics when the capability is required on this platform. The caller still
/// writes its own `return`, so the skip stays visible at the call site:
///
/// ```ignore
/// let Some(runtime) = detect() else {
///     test_capability::skip(capability::CONTAINER, "no runtime on PATH");
///     return;
/// };
/// ```
///
/// The message is deliberately explicit about why a skip is being escalated:
/// whoever hits it is usually not the person who set the CI variable.
pub fn skip(capability: &str, detail: &str) {
    if is_required(capability) {
        panic!(
            "required capability {capability:?} is missing on this host: {detail}\n\
             \n\
             {REQUIRED_CAPABILITIES_ENV} lists {capability:?}, so this platform is \
             expected to exercise it. Skipping here would report `ok` for a test \
             that never ran, which is how the Windows container tests hid two real \
             bugs for the life of that CI lane.\n\
             \n\
             Either install the capability on this host, or remove it from \
             {REQUIRED_CAPABILITIES_ENV} for this platform and say why."
        );
    }
    let line = format!("{SKIP_MARKER}: {capability}: {detail}");
    // Visible under `--nocapture`, and to a human reading a failing target.
    println!("{line}");
    // Survives libtest's capture. Append rather than truncate: every test
    // binary in the workspace writes to the same ledger, and they run as
    // separate processes. Best-effort by design — a test must never fail
    // because the ledger could not be written.
    if let Ok(path) = std::env::var(SKIP_LOG_ENV) {
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_matching_is_exact_and_case_insensitive() {
        // Parsing is checked directly rather than through the env, which is
        // process-global and would race the rest of the suite.
        let parse = |raw: &str, want: &str| {
            raw.split(',')
                .map(str::trim)
                .any(|entry| entry.eq_ignore_ascii_case(want))
        };
        assert!(parse("git,container", "git"));
        assert!(parse("git, container", "container"));
        assert!(parse("GIT", "git"), "matching is case-insensitive");
        assert!(!parse("git-lfs", "git"), "matching must not be a substring");
        assert!(!parse("", "git"));
    }

    #[test]
    fn an_unrequired_capability_skips_without_panicking() {
        // Nothing sets a requirement for this name, so this must not panic.
        skip("a-capability-no-platform-requires", "unit test");
    }
}
