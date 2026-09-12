use kranz_engine::paths::{open_read_nofollow, read_regular_file_bounded};

#[test]
fn bounded_read_accepts_exact_limit_and_refuses_oversized_or_non_utf8_files() {
    let repo = tempfile::tempdir().unwrap();
    let path = repo.path().join("artifact");
    std::fs::write(&path, "12345678").unwrap();
    assert_eq!(
        read_regular_file_bounded(open_read_nofollow(&path).unwrap(), 8).unwrap(),
        "12345678"
    );
    let error = read_regular_file_bounded(open_read_nofollow(&path).unwrap(), 7).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);
    std::fs::write(&path, [0xff]).unwrap();
    let error = read_regular_file_bounded(open_read_nofollow(&path).unwrap(), 8).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(open_read_nofollow(repo.path()).is_err());
}

#[cfg(unix)]
#[test]
fn nofollow_read_rejects_fifo_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    const CHILD_PATH: &str = "KRANZ_TEST_FIFO_READ_CHILD_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        assert!(open_read_nofollow(std::path::Path::new(&path)).is_err());
        return;
    }
    let repo = tempfile::tempdir().unwrap();
    let path = repo.path().join("fifo");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated path remains alive throughout mkfifo.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "nofollow_read_rejects_fifo_without_waiting_for_a_writer",
        ])
        .env(CHILD_PATH, &path)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("opening a FIFO must not wait for a writer");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
