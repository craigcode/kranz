//! Portable shell snippets for test fixtures.
//!
//! Contract, readiness, and gate commands bottom out in `cmd /C` on Windows
//! and `sh -c` everywhere else (see [`crate::command_exec`]'s `shell_argv`).
//! cmd.exe has no `true`, `false`, `test`, or `sleep` builtin and Windows
//! ships no such executables, so a fixture hard-coding them only resolves on a
//! machine with Git's `usr/bin` on PATH. Hosted CI happens to provide that; a
//! stock Windows developer box does not, which is why fixtures that looked
//! portable passed on `windows-latest` and failed on a real workstation.
//!
//! Prefer these helpers over literal POSIX binaries in fixtures. `exit 0` /
//! `exit 1` are the two constructs that genuinely parse the same in both
//! shells; everything else needs a branch.

/// A command that always succeeds, in both `sh` and `cmd`.
pub(crate) const SUCCEED: &str = "exit 0";

/// A command that always fails, in both `sh` and `cmd`.
pub(crate) const FAIL: &str = "exit 1";

/// Succeed iff `path` exists as a file, relative to the command's cwd.
pub(crate) fn file_exists(path: &str) -> String {
    if cfg!(windows) {
        // `if exist` is a cmd builtin; the trailing `else` makes the failure
        // an explicit non-zero exit rather than cmd's implicit success.
        format!("if exist {path} (exit 0) else (exit 1)")
    } else {
        format!("test -f {path}")
    }
}

/// Succeed iff `path` exists, and then run `then` (itself a portable snippet).
pub(crate) fn if_file_exists(path: &str, then: &str) -> String {
    if cfg!(windows) {
        format!("if exist {path} ({then}) else (exit 1)")
    } else {
        format!("test -f {path} && {then}")
    }
}

/// Write `text` to `path` as a single line.
pub(crate) fn write_line(text: &str, path: &str) -> String {
    // `echo x > file` is spelled the same in both shells, but cmd would carry
    // the space before `>` into the file, so close it up there. Closing it up
    // makes a trailing digit dangerous: cmd reads `echo x1>f` as a redirect of
    // file descriptor 1, silently writing "x" instead of "x1".
    debug_assert!(
        !text.ends_with(|c: char| c.is_ascii_digit()),
        "write_line text must not end in a digit: cmd.exe would parse `{text}>` \
         as a file-descriptor redirect"
    );
    if cfg!(windows) {
        format!("echo {text}>{path}")
    } else {
        format!("echo {text} > {path}")
    }
}

/// Block for at least `millis`, without relying on a `sleep` binary.
pub(crate) fn sleep_millis(cwd: &std::path::Path, millis: u64) -> String {
    if cfg!(windows) {
        // Use this test binary as the timer: ping can return early and
        // PowerShell exceeded the fixture's timeout in Windows CI.
        // A relative executable name also avoids cmd's nested quote handling
        // when the temporary directory or original binary path has spaces.
        std::fs::copy(
            std::env::current_exe().unwrap(),
            cwd.join("kranz-sleep-fixture.exe"),
        )
        .unwrap();
        format!(
            "set KRANZ_TEST_SLEEP_MILLIS={millis}&& .\\kranz-sleep-fixture.exe --ignored --exact test_shell::sleep_fixture --nocapture"
        )
    } else {
        let seconds = millis as f64 / 1000.0;
        format!("sleep {seconds}")
    }
}

#[cfg(windows)]
#[test]
#[ignore = "subprocess timer, invoked by contract-lint tests"]
fn sleep_fixture() {
    let millis: u64 = std::env::var("KRANZ_TEST_SLEEP_MILLIS")
        .expect("timer duration")
        .parse()
        .expect("numeric timer duration");
    std::thread::sleep(std::time::Duration::from_millis(millis));
    println!("KRANZ_TEST_SLEEP_COMPLETED={millis}");
}
