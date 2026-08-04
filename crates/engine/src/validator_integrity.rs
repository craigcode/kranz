//! Validator immutability: the TRIPWIRE half (ticket
//! `validator-immutability-proof`, security review P1 #5; snapshot
//! follow-up in [`crate::validator_snapshot`]).
//!
//! "Read-only" validators were nominal: backends do not enforce
//! `writable: false`, and the process sandbox allows writes to the session
//! checkout — a prompt-injected or misbehaving validator can edit tests to
//! manufacture a pass, `git add && git commit` its own changes, or leave
//! unreviewed files in the deliverable. The definitive fix is the
//! copy-on-write immutable snapshot ([`crate::validator_snapshot`]): each
//! validator session runs in a THROWAWAY worktree copy of the checkout and
//! only its verdict crosses back, so writes are structurally discarded
//! rather than caught.
//!
//! What remains here is the tripwire: an IDENTITY ASSERTION around every
//! validator session, still taken on the REAL checkout. Capture HEAD +
//! porcelain status before the spawn, re-capture after — with snapshot
//! isolation in place the real checkout should be byte-identical, so any
//! drift means the isolation itself failed (a validator escaped its
//! snapshot, or moved shared git refs the snapshot cannot isolate). The
//! orchestrator then emits `validator.tamper` and blocks the milestone,
//! with no retry and no waivable finding.
//!
//! The assertion is precise: **no tracked file changed, HEAD unchanged,
//! index unchanged** — plus no new non-ignored file (a dropped test file
//! manufactures a pass just as an edit does), and **no `.git` metadata
//! change**: the worktree can look identical while `config`
//! (`core.fsmonitor`, `core.hooksPath`, aliases — all executed during the
//! ENGINE's own git invocations), `hooks/`, or refs were weaponized, so the
//! fingerprint covers them too (3rd-pass review). `git status --porcelain`
//! respects .gitignore, so legitimate gate artifact churn (`target/`, the
//! gitignored `.kranz` engine runtime) never trips it. The refs half is the
//! one mutation class the snapshot does NOT contain (worktrees share the
//! common `.git`), which is exactly why the tripwire must stay.

use crate::error::Result;
use crate::git_ops::GitRepo;

/// HEAD + `git status --porcelain` of a checkout at one instant — the
/// identity a read-only validator session must preserve byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutFingerprint {
    /// `git rev-parse HEAD`.
    pub head: String,
    /// `git status --porcelain --untracked-files=all`: index + worktree
    /// status of tracked files plus EVERY untracked non-ignored path;
    /// ignored paths (target/, .kranz runtime) never appear.
    pub status: String,
    /// Contents of `<git-common-dir>/config`: `core.fsmonitor`,
    /// `core.hooksPath`, aliases, and external-diff drivers all execute
    /// during the ENGINE's own git invocations — a validator that can write
    /// only `.git/config` owns the host without touching the worktree.
    pub git_config: String,
    /// Sorted `name HASH` lines for non-`.sample` files in
    /// `<git-common-dir>/hooks` — a planted hook executes on the engine's
    /// next git operation that isn't hooks-disabled.
    pub git_hooks: String,
    /// `git for-each-ref` output: moving a ref retargets later merges
    /// without touching HEAD or the worktree.
    pub git_refs: String,
    /// `git ls-files -v` output: `skip-worktree`/`assume-unchanged` flags
    /// hide worktree modifications from `git status` (4th-pass review) —
    /// the flags are part of the identity, so setting one is drift.
    pub index_flags: String,
    /// Contents of `<git-common-dir>/info/exclude`: the local exclude file
    /// hides untracked files from `git status` without touching the tree.
    pub info_exclude: String,
}

impl CheckoutFingerprint {
    /// Capture the identity of `repo`'s checkout right now.
    ///
    /// All git invocations run on a VERIFICATION handle (hooks + fsmonitor
    /// disabled): a poisoned `core.fsmonitor` in the checkout's config must
    /// never get its payload executed by the detection itself (4th-pass
    /// review — detection previously ran `git status` before comparing
    /// config, so the payload ran first). The `.git` metadata itself is
    /// read via the filesystem with BOUNDED, no-follow reads (5th-pass: a
    /// replaced config or info/exclude that is a FIFO or an unbounded
    /// source must not hang or exhaust the engine mid-detection — anything
    /// unusual reads as a stable refusal marker, which is itself drift).
    pub fn capture(repo: &GitRepo) -> Result<Self> {
        let verification = repo.with_hooks_disabled()?;
        let common = verification.git_common_dir()?;
        Ok(CheckoutFingerprint {
            head: verification.head_sha()?,
            status: verification.porcelain_status()?,
            git_config: bounded_metadata_read(&common.join("config")),
            git_hooks: hook_listing(&common.join("hooks")),
            git_refs: verification.for_each_ref()?,
            index_flags: verification.ls_files_v()?,
            info_exclude: bounded_metadata_read(&common.join("info").join("exclude")),
        })
    }

    /// The drift between `self` (before the session) and `after` — `None`
    /// when the checkout is byte-identical.
    pub fn drift(&self, after: &Self) -> Option<CheckoutDrift> {
        if self == after {
            return None;
        }
        let before: std::collections::BTreeSet<&str> = self.status.lines().collect();
        let later: std::collections::BTreeSet<&str> = after.status.lines().collect();
        let mut metadata_fields: Vec<String> = Vec::new();
        if self.git_config != after.git_config {
            metadata_fields.push("config".to_string());
        }
        if self.git_hooks != after.git_hooks {
            metadata_fields.push("hooks".to_string());
        }
        if self.git_refs != after.git_refs {
            metadata_fields.push("refs".to_string());
        }
        if self.index_flags != after.index_flags {
            metadata_fields.push("index-flags".to_string());
        }
        if self.info_exclude != after.info_exclude {
            metadata_fields.push("info-exclude".to_string());
        }
        let git_metadata_changed = !metadata_fields.is_empty();
        Some(CheckoutDrift {
            head_before: self.head.clone(),
            head_after: after.head.clone(),
            appeared: later.difference(&before).map(|s| s.to_string()).collect(),
            resolved: before.difference(&later).map(|s| s.to_string()).collect(),
            git_metadata_changed,
            git_metadata_fields: metadata_fields,
        })
    }
}

/// Bounded, no-follow read of a `.git` metadata file (config,
/// info/exclude): regular files only, first 64 KiB (`.git` config files
/// are kilobyte-scale; anything larger is itself suspicious). A symlink,
/// FIFO, oversized, or unreadable path returns a STABLE refusal marker
/// instead of being opened (5th-pass review: a validator that can replace
/// the shared config with a FIFO or an unbounded source must not hang or
/// exhaust the engine mid-capture — the marker is constant per shape, so
/// it only registers as drift when it changes).
fn bounded_metadata_read(path: &std::path::Path) -> String {
    use std::io::Read as _;
    const CAP: u64 = 64 * 1024;
    let mut file = match open_regular_nofollow_nonblocking(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return String::new(),
        Err(_) => return suspect_marker(path),
    };
    let Ok(metadata) = file.metadata() else {
        return "SUSPECT:unreadable".to_string();
    };
    let file_type = metadata.file_type();
    if !file_type.is_file() {
        let kind = if file_type.is_dir() { "dir" } else { "special" };
        return format!("SUSPECT:{kind}");
    }
    if metadata.len() > CAP {
        return format!("SUSPECT:oversized:{}", metadata.len());
    }
    let mut buf = Vec::new();
    match (&mut file).take(CAP + 1).read_to_end(&mut buf) {
        Ok(_) if buf.len() as u64 <= CAP => String::from_utf8_lossy(&buf).into_owned(),
        Ok(_) => format!("SUSPECT:oversized:{}+", CAP),
        Err(_) => "SUSPECT:unreadable".to_string(),
    }
}

/// Open a validator-controlled metadata entry without following its leaf and
/// without blocking on a FIFO. The fd is verified after open, so swapping a
/// regular entry for a symlink/device between `readdir` and `open` cannot
/// escape the checks.
fn open_regular_nofollow_nonblocking(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

fn suspect_marker(path: &std::path::Path) -> String {
    let kind = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => "symlink",
        Ok(metadata) if metadata.file_type().is_dir() => "dir",
        Ok(metadata) if !metadata.file_type().is_file() => "special",
        _ => "unreadable",
    };
    format!("SUSPECT:{kind}")
}

/// Sorted `name HASH` lines for the non-`.sample` hooks in `hooks_dir`
/// (content-hashed: a same-length rewrite must not slip past). Reads are
/// BOUNDED and never followed (4th-pass review): only regular files —
/// hashed whole when ≤ 1 MiB (hooks are kilobyte-scale; the whole file
/// closes the middle blind spot), and as first-32 KiB + last-32 KiB +
/// length above 1 MiB (pathological size; the residual middle gap there is
/// documented, not hidden). A symlink, FIFO, device, or other special
/// entry is NEVER opened — a FIFO would block forever, a `/dev/zero`
/// symlink would allocate without bound — and records a stable
/// `name SUSPECT:<kind>` marker instead: the same marker on the next
/// capture, so an unchanged oddity is not itself drift, but any change to
/// it is. Absent or unreadable dirs list as empty.
fn hook_listing(hooks_dir: &std::path::Path) -> String {
    use std::hash::{Hash, Hasher};
    const HOOK_FULL_READ_MAX: u64 = 1024 * 1024;
    const HOOK_WINDOW: u64 = 32 * 1024;
    let mut lines: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(hooks_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".sample") {
                continue;
            }
            let mut file = match open_regular_nofollow_nonblocking(&entry.path()) {
                Ok(file) => file,
                Err(_) => {
                    lines.push(format!("{name} {}", suspect_marker(&entry.path())));
                    continue;
                }
            };
            let Ok(metadata) = file.metadata() else {
                lines.push(format!("{name} SUSPECT:unreadable"));
                continue;
            };
            let file_type = metadata.file_type();
            if !file_type.is_file() {
                let kind = if file_type.is_dir() { "dir" } else { "special" };
                lines.push(format!("{name} SUSPECT:{kind}"));
                continue;
            }
            use std::io::{Read as _, Seek as _, SeekFrom};
            let len = metadata.len();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            if len <= HOOK_FULL_READ_MAX {
                let mut contents = Vec::new();
                if (&mut file)
                    .take(HOOK_FULL_READ_MAX + 1)
                    .read_to_end(&mut contents)
                    .is_err()
                    || contents.len() as u64 > HOOK_FULL_READ_MAX
                {
                    lines.push(format!("{name} SUSPECT:oversized"));
                    continue;
                }
                contents.hash(&mut hasher);
            } else {
                let mut head = vec![0u8; HOOK_WINDOW as usize];
                let head_read = file.read(&mut head).unwrap_or(0);
                head[..head_read].hash(&mut hasher);
                let tail_start = len.saturating_sub(HOOK_WINDOW);
                if file.seek(SeekFrom::Start(tail_start)).is_ok() {
                    let mut tail = vec![0u8; HOOK_WINDOW as usize];
                    let tail_read = file.read(&mut tail).unwrap_or(0);
                    tail[..tail_read].hash(&mut hasher);
                }
                len.hash(&mut hasher);
            }
            lines.push(format!("{name} {:016x}", hasher.finish()));
        }
    }
    lines.sort();
    lines.join("\n")
}

/// What a validator session changed: HEAD movement plus the porcelain
/// entries gained/lost across the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutDrift {
    pub head_before: String,
    pub head_after: String,
    /// Porcelain entries present after but not before (the session's
    /// writes).
    pub appeared: Vec<String>,
    /// Porcelain entries present before but not after (the session reverted
    /// or hid a pre-existing dirty state — equally a mutation).
    pub resolved: Vec<String>,
    /// `.git` config/hooks/refs changed — the checkout can look identical
    /// while the plumbing was weaponized.
    pub git_metadata_changed: bool,
    /// WHICH metadata surfaces changed (`config`/`hooks`/`refs`/
    /// `index-flags`/`info-exclude`) — recorded so a tripwire fire is
    /// diagnosable without reconstructing the window (mission m-83d1ed
    /// fired on metadata alone with no field named).
    pub git_metadata_fields: Vec<String>,
}

impl CheckoutDrift {
    /// One-line human summary for the `milestone.blocked` reason, capped so
    /// a pathological drift (thousands of entries) stays readable.
    pub fn summary(&self) -> String {
        const MAX_ENTRIES: usize = 5;
        let mut parts: Vec<String> = Vec::new();
        if self.head_before != self.head_after {
            parts.push(format!(
                "HEAD moved {} -> {}",
                short_sha(&self.head_before),
                short_sha(&self.head_after)
            ));
        }
        let entries = self.appeared.len() + self.resolved.len();
        if entries > 0 {
            let mut shown: Vec<&str> = self
                .appeared
                .iter()
                .map(String::as_str)
                .chain(self.resolved.iter().map(String::as_str))
                .take(MAX_ENTRIES)
                .collect();
            if entries > MAX_ENTRIES {
                shown.push("…");
            }
            parts.push(format!(
                "{entries} status entr{} changed: {}",
                if entries == 1 { "y" } else { "ies" },
                shown.join(", ")
            ));
        }
        if self.git_metadata_changed {
            parts.push(format!(
                ".git metadata changed ({})",
                self.git_metadata_fields.join("/")
            ));
        }
        parts.join("; ")
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(head: &str, status: &str) -> CheckoutFingerprint {
        CheckoutFingerprint {
            head: head.to_string(),
            status: status.to_string(),
            git_config: String::new(),
            git_hooks: String::new(),
            git_refs: String::new(),
            index_flags: String::new(),
            info_exclude: String::new(),
        }
    }

    #[test]
    fn identical_fingerprints_have_no_drift() {
        let before = fp("abc123", " M src/a.rs\n?? notes.txt\n");
        assert_eq!(before.drift(&before.clone()), None);
    }

    #[test]
    fn head_move_is_drift_even_with_identical_status() {
        let before = fp("abc1234", "");
        let after = fp("def5678", "");
        let drift = before.drift(&after).expect("head move must be drift");
        assert_eq!(drift.head_before, "abc1234");
        assert_eq!(drift.head_after, "def5678");
        assert!(drift.appeared.is_empty());
        assert!(drift.resolved.is_empty());
        assert!(drift.summary().contains("HEAD moved abc1234 -> def5678"));
    }

    #[test]
    fn status_changes_split_into_appeared_and_resolved() {
        let before = fp("abc1234", " M src/a.rs\n");
        let after = fp("abc1234", " M src/b.rs\n?? dropped.rs\n");
        let drift = before.drift(&after).expect("status change must be drift");
        assert_eq!(drift.appeared, vec![" M src/b.rs", "?? dropped.rs"]);
        assert_eq!(drift.resolved, vec![" M src/a.rs"]);
        let summary = drift.summary();
        assert!(summary.contains("3 status entries changed"), "{summary}");
        assert!(summary.contains("?? dropped.rs"), "{summary}");
    }

    #[test]
    fn summary_caps_long_entry_lists() {
        let after_status: String = (0..20).map(|i| format!("?? f{i}.rs\n")).collect();
        let drift = fp("h", "").drift(&fp("h", &after_status)).unwrap();
        let summary = drift.summary();
        assert!(summary.contains("20 status entries changed"), "{summary}");
        assert!(summary.contains('…'), "{summary}");
    }

    /// 4th-pass review: skip-worktree/assume-unchanged flags and the local
    /// exclude file hide modifications from porcelain — changing either is
    /// metadata drift even with HEAD and status byte-identical.
    #[test]
    fn index_flags_and_info_exclude_changes_are_drift() {
        let before = fp("abc1234", "");

        let mut flagged = before.clone();
        flagged.index_flags = "S src/hidden_test.rs\n".to_string();
        let drift = before
            .drift(&flagged)
            .expect("a skip-worktree flag must be drift");
        assert!(drift.git_metadata_changed);

        let mut excluded = before.clone();
        excluded.info_exclude = "secret-test.sh\n".to_string();
        let drift = before
            .drift(&excluded)
            .expect("an info/exclude change must be drift");
        assert!(drift.git_metadata_changed);
    }

    /// 4th-pass review: special entries (FIFO/symlink/dir) are never opened
    /// — they record a stable SUSPECT marker instead, so the listing cannot
    /// block or exhaust memory, and an unchanged oddity is not drift.
    #[cfg(unix)]
    #[test]
    fn hook_listing_marks_special_entries_without_opening_them() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        // A FIFO: opening it for read would block forever.
        let fifo_path = std::ffi::CString::new(hooks.join("evil-fifo").to_str().unwrap()).unwrap();
        let rc = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o700) };
        assert_eq!(rc, 0, "mkfifo failed");
        // A symlink to an unbounded device: reading it would never fill.
        symlink("/dev/zero", hooks.join("evil-link")).unwrap();
        std::fs::create_dir(hooks.join("nested")).unwrap();
        std::fs::write(hooks.join("good-hook"), b"echo ok").unwrap();

        let listing = hook_listing(&hooks);
        assert!(listing.contains("evil-fifo SUSPECT:special"), "{listing}");
        assert!(listing.contains("evil-link SUSPECT:symlink"), "{listing}");
        assert!(listing.contains("nested SUSPECT:dir"), "{listing}");
        assert!(listing.contains("good-hook "), "{listing}");
        // Stable: the same oddities list identically (no false drift).
        assert_eq!(listing, hook_listing(&hooks));
    }

    #[cfg(unix)]
    #[test]
    fn metadata_reader_refuses_fifo_and_unbounded_symlink_without_opening_them() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("config-fifo");
        let fifo_c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let link = dir.path().join("config-link");
        symlink("/dev/zero", &link).unwrap();

        assert_eq!(bounded_metadata_read(&fifo), "SUSPECT:special");
        assert_eq!(bounded_metadata_read(&link), "SUSPECT:symlink");
    }

    #[cfg(unix)]
    #[test]
    fn capture_disables_validator_controlled_fsmonitor_before_running_git() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("run git")
        };
        assert!(git(&["init", "-q"]).status.success());
        std::fs::write(dir.path().join("tracked"), "one").unwrap();
        assert!(git(&["add", "tracked"]).status.success());
        assert!(git(&[
            "-c",
            "user.name=kranz-test",
            "-c",
            "user.email=kranz@test.invalid",
            "commit",
            "-qm",
            "initial",
        ])
        .status
        .success());

        let marker = dir.path().join("fsmonitor-ran");
        let monitor = dir.path().join("evil-fsmonitor");
        std::fs::write(
            &monitor,
            format!(
                "#!/bin/sh\nprintf invoked > '{}'\nexit 1\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&monitor, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            git(&["config", "core.fsmonitor", monitor.to_str().unwrap()])
                .status
                .success()
        );

        let _ = git(&["status", "--porcelain"]);
        assert!(
            marker.exists(),
            "fixture: ordinary git status runs fsmonitor"
        );
        std::fs::remove_file(&marker).unwrap();

        let repo = GitRepo::open(dir.path()).unwrap();
        CheckoutFingerprint::capture(&repo).unwrap();
        assert!(
            !marker.exists(),
            "fingerprint capture must disable fsmonitor before its first git invocation"
        );
    }

    /// An over-cap hook still hashes deterministically, and a tail change
    /// beyond the read cap still changes the line via the recorded length.
    #[test]
    fn hook_listing_bounds_large_hooks_but_still_notices_tail_changes() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        std::fs::write(hooks.join("big"), vec![b'a'; 128 * 1024]).unwrap();

        let first = hook_listing(&hooks);
        assert!(first.starts_with("big "), "{first}");
        assert_eq!(first, hook_listing(&hooks), "listing is deterministic");

        // Change ONLY bytes beyond the 64 KiB read cap: the length half of
        // the hash still notices.
        let mut contents = vec![b'a'; 128 * 1024];
        contents[127 * 1024] = b'b';
        std::fs::write(hooks.join("big"), &contents).unwrap();
        assert_ne!(first, hook_listing(&hooks));
    }

    /// 3rd-pass review: the worktree can look identical while `.git` was
    /// weaponized — config (`core.fsmonitor`), a planted hook, or a moved
    /// ref must each be drift even with HEAD and status untouched.
    #[test]
    fn git_metadata_change_is_drift_with_identical_checkout() {
        let before = fp("abc1234", "");

        let mut config_tampered = before.clone();
        config_tampered.git_config = "[core]\n\tfsmonitor = evil\n".to_string();
        let drift = before
            .drift(&config_tampered)
            .expect("config tamper must be drift");
        assert!(drift.git_metadata_changed);
        assert!(
            drift.summary().contains(".git metadata"),
            "{}",
            drift.summary()
        );

        let mut hook_planted = before.clone();
        hook_planted.git_hooks = "post-checkout deadbeefdeadbeef\n".to_string();
        let drift = before
            .drift(&hook_planted)
            .expect("planted hook must be drift");
        assert!(drift.git_metadata_changed);

        let mut ref_moved = before.clone();
        ref_moved.git_refs = "refs/heads/main deadbeef\n".to_string();
        let drift = before.drift(&ref_moved).expect("moved ref must be drift");
        assert!(drift.git_metadata_changed);

        // No false positive: identical metadata is not drift.
        assert_eq!(before.drift(&before.clone()), None);
    }

    /// The hook listing ignores `.sample` files and hashes contents, so a
    /// same-length rewrite still changes the line.
    #[test]
    fn hook_listing_skips_samples_and_hashes_contents() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        std::fs::write(hooks.join("pre-commit.sample"), "sample-a").unwrap();
        std::fs::write(hooks.join("post-checkout"), b"echo one").unwrap();

        let listing = hook_listing(&hooks);
        assert!(!listing.contains("sample"), "{listing}");
        assert!(listing.starts_with("post-checkout "), "{listing}");

        // Same name, same length, different content: the hash must change.
        std::fs::write(hooks.join("post-checkout"), b"echo two").unwrap();
        let rewritten = hook_listing(&hooks);
        assert_ne!(listing, rewritten);

        // An absent dir lists empty (== an empty hooks dir).
        assert_eq!(hook_listing(&dir.path().join("missing")), "");
    }
}
