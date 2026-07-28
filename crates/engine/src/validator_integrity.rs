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
//! manufactures a pass just as an edit does). `git status --porcelain`
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
    /// `git status --porcelain` (v1): index + worktree status of tracked
    /// files plus untracked non-ignored paths; ignored paths (target/,
    /// .kranz runtime) never appear.
    pub status: String,
}

impl CheckoutFingerprint {
    /// Capture the identity of `repo`'s checkout right now.
    pub fn capture(repo: &GitRepo) -> Result<Self> {
        Ok(CheckoutFingerprint {
            head: repo.head_sha()?,
            status: repo.porcelain_status()?,
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
        })
    }
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
}
