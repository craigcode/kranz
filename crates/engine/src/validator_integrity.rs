//! Validator immutability proof (ticket `validator-immutability-proof`,
//! security review P1 #5).
//!
//! "Read-only" validators are nominal: backends do not enforce
//! `writable: false`, and the process sandbox allows writes to the session
//! checkout — a prompt-injected or misbehaving validator can edit tests to
//! manufacture a pass, `git add && git commit` its own changes, or leave
//! unreviewed files in the deliverable. Until a true immutable snapshot
//! lands (a copy-on-write worktree discarded after the round), the honest
//! interim is an IDENTITY ASSERTION around every validator session: capture
//! HEAD + porcelain status before the spawn, re-capture after, and any drift
//! fails the round honestly — the orchestrator emits `validator.tamper` and
//! blocks the milestone, with no retry and no waivable finding.
//!
//! The assertion is precise: **no tracked file changed, HEAD unchanged,
//! index unchanged** — plus no new non-ignored file (a dropped test file
//! manufactures a pass just as an edit does), and **no `.git` metadata
//! change**: the worktree can look identical while `config`
//! (`core.fsmonitor`, `core.hooksPath`, aliases — all executed during the
//! ENGINE's own git invocations), `hooks/`, or refs were weaponized, so the
//! fingerprint covers them too (3rd-pass review). `git status --porcelain`
//! respects .gitignore, so legitimate gate artifact churn (`target/`, the
//! gitignored `.kranz` engine runtime) never trips it. Validators CAN still
//! write; the guarantee is that any write is caught and fails the round.

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
}

impl CheckoutFingerprint {
    /// Capture the identity of `repo`'s checkout right now.
    pub fn capture(repo: &GitRepo) -> Result<Self> {
        let common = repo.git_common_dir()?;
        Ok(CheckoutFingerprint {
            head: repo.head_sha()?,
            status: repo.porcelain_status()?,
            git_config: std::fs::read_to_string(common.join("config")).unwrap_or_default(),
            git_hooks: hook_listing(&common.join("hooks")),
            git_refs: repo.for_each_ref()?,
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
        Some(CheckoutDrift {
            head_before: self.head.clone(),
            head_after: after.head.clone(),
            appeared: later.difference(&before).map(|s| s.to_string()).collect(),
            resolved: before.difference(&later).map(|s| s.to_string()).collect(),
            git_metadata_changed: self.git_config != after.git_config
                || self.git_hooks != after.git_hooks
                || self.git_refs != after.git_refs,
        })
    }
}

/// Sorted `name HASH` lines for the non-`.sample` hooks in `hooks_dir`
/// (content-hashed: a same-length rewrite must not slip past). Absent or
/// unreadable dirs list as empty — an absent hooks dir and an empty one
/// are equivalent for tamper purposes.
fn hook_listing(hooks_dir: &std::path::Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut lines: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(hooks_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".sample") {
                continue;
            }
            if let Ok(contents) = std::fs::read(entry.path()) {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                contents.hash(&mut hasher);
                lines.push(format!("{name} {:016x}", hasher.finish()));
            }
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
            parts.push(".git metadata changed (config/hooks/refs)".to_string());
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
