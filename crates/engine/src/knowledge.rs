//! Ranked, byte-capped `docs/knowledge/` injection for planning seeds (slice 2).
//!
//! Separate from the lessons budget ([`crate::lessons::LESSONS_INJECT_MAX_BYTES`]).
//! Missing vault → inject nothing. Stale notes and notes without
//! `verified_against` are excluded from automatic injection.
//!
//! Boundary note (positioning ADR, frozen surface): context-MANAGEMENT
//! features — retrieval pipelines, memory systems, dynamic context
//! optimization — are frozen; this is a capped, ranked, auditable
//! injection, deliberately not a context engine
//! (docs/knowledge/decisions/positioning-governance-evidence-layer.md).

use crate::git_ops::GitRepo;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Hard cap (bytes) on the rendered knowledge block — D-C / ticket.
pub const KNOWLEDGE_INJECT_MAX_BYTES: usize = 4096;

const HEADER: &str = "## Knowledge from this repo\n\n";

/// Inputs that drive note ranking for a planning or revised-planning seed.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeQuery<'a> {
    pub goal: &'a str,
    pub ticket_body: Option<&'a str>,
    pub touch_hints: &'a [String],
    pub changed_files: &'a [String],
}

/// Render a ≤4 KiB knowledge block, or `None` when the vault is missing/empty
/// of injectable content.
pub fn render_knowledge_for_planning(
    repo_root: &Path,
    query: &KnowledgeQuery<'_>,
) -> Option<String> {
    let vault = repo_root.join("docs").join("knowledge");
    if !vault.is_dir() {
        return None;
    }

    let mut out = String::with_capacity(KNOWLEDGE_INJECT_MAX_BYTES);
    out.push_str(HEADER);

    // 1) Truncated map from index.md (headings / top-level map bullets).
    // The map itself may be truncated to fit; notes below are whole-or-skip.
    if let Some(map) = render_index_map(&vault) {
        push_truncated(&mut out, &map);
    }
    if out.len() >= KNOWLEDGE_INJECT_MAX_BYTES {
        return Some(truncate_to_bytes(&out, KNOWLEDGE_INJECT_MAX_BYTES));
    }

    let mut notes = collect_notes(repo_root, &vault);
    if notes.is_empty() && out.trim() == HEADER.trim() {
        return None;
    }

    let haystack = {
        let mut s = query.goal.to_ascii_lowercase();
        if let Some(body) = query.ticket_body {
            s.push('\n');
            s.push_str(&body.to_ascii_lowercase());
        }
        s
    };
    let path_hints: Vec<String> = query
        .touch_hints
        .iter()
        .chain(query.changed_files.iter())
        .map(|p| p.replace('\\', "/"))
        .collect();

    // Tier 2: explicitly referenced by goal/ticket (path or title).
    // Tier 3: verified_against overlaps touch/changed hints.
    let mut tier2 = Vec::new();
    let mut tier3 = Vec::new();
    for note in notes.drain(..) {
        if note_referenced(&note, &haystack) {
            tier2.push(note);
        } else if note_overlaps_paths(&note, &path_hints) {
            tier3.push(note);
        }
    }
    // Stable order within a tier: path lexicographic.
    tier2.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    tier3.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    for note in tier2.into_iter().chain(tier3) {
        if out.len() >= KNOWLEDGE_INJECT_MAX_BYTES {
            break;
        }
        let block = format_note_excerpt(&note);
        if !push_budgeted(&mut out, &block) {
            break;
        }
    }

    let trimmed = out.trim_end();
    if trimmed == HEADER.trim() || trimmed.is_empty() {
        None
    } else {
        Some(truncate_to_bytes(trimmed, KNOWLEDGE_INJECT_MAX_BYTES))
    }
}

/// One citation verdict from [`refresh_knowledge`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "verdict")]
pub enum RefreshVerdict {
    Ok,
    AlreadyStale,
    Unverified,
    PathMissing { path: String },
    PathDrifted { path: String },
    CommandSkipped { command: String },
}

impl RefreshVerdict {
    /// Findings that should fail `kranz knowledge refresh` (exit 1).
    /// `already-stale` and `command-skipped` are reported but do not fail.
    pub fn is_check_needed(&self) -> bool {
        matches!(
            self,
            Self::Unverified | Self::PathMissing { .. } | Self::PathDrifted { .. }
        )
    }
}

/// One vault note's refresh result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshFinding {
    pub rel_path: String,
    pub title: String,
    pub freshness: String,
    pub verdicts: Vec<RefreshVerdict>,
}

/// Report-only drift check over `docs/knowledge/` (slice 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshReport {
    pub findings: Vec<RefreshFinding>,
}

impl RefreshReport {
    pub fn check_needed(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.verdicts.iter().any(RefreshVerdict::is_check_needed))
    }
}

/// Walk the vault and report drifted / missing / unverified citations.
///
/// Never rewrites a note. Never runs a `verified_against` command (whitespace
/// citations are `command-skipped`). Path drift uses
/// [`GitRepo::path_changed_since`] when the repo is git and the note has
/// `last_verified`.
pub fn refresh_knowledge(repo_root: &Path) -> RefreshReport {
    let vault = repo_root.join("docs").join("knowledge");
    let git = GitRepo::open(repo_root).ok();
    let mut findings = Vec::new();
    if !vault.is_dir() {
        return RefreshReport { findings };
    }
    for note in collect_notes_for_refresh(repo_root, &vault) {
        findings.push(refresh_one(&note, repo_root, git.as_ref()));
    }
    findings.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    RefreshReport { findings }
}

fn refresh_one(note: &KnowledgeNote, repo_root: &Path, git: Option<&GitRepo>) -> RefreshFinding {
    let mut verdicts = Vec::new();
    if note.freshness.eq_ignore_ascii_case("stale") {
        verdicts.push(RefreshVerdict::AlreadyStale);
    }
    if note.verified_against.is_empty() {
        if !note.freshness.eq_ignore_ascii_case("stale") {
            verdicts.push(RefreshVerdict::Unverified);
        }
        return RefreshFinding {
            rel_path: note.rel_path.clone(),
            title: note.title.clone(),
            freshness: note.freshness.clone(),
            verdicts,
        };
    }
    for citation in &note.verified_against {
        verdicts.push(refresh_citation(citation, note, repo_root, git));
    }
    RefreshFinding {
        rel_path: note.rel_path.clone(),
        title: note.title.clone(),
        freshness: note.freshness.clone(),
        verdicts,
    }
}

fn refresh_citation(
    citation: &str,
    note: &KnowledgeNote,
    repo_root: &Path,
    git: Option<&GitRepo>,
) -> RefreshVerdict {
    if citation.chars().any(char::is_whitespace) {
        return RefreshVerdict::CommandSkipped {
            command: citation.to_string(),
        };
    }
    let path = repo_root.join(citation);
    let exists = path.is_file() || path.is_dir();
    if !exists {
        return RefreshVerdict::PathMissing {
            path: citation.to_string(),
        };
    }
    if let (Some(git), Some(since)) = (git, note.last_verified.as_deref()) {
        if git
            .path_changed_since(citation, since)
            .ok()
            .unwrap_or(false)
        {
            return RefreshVerdict::PathDrifted {
                path: citation.to_string(),
            };
        }
    }
    RefreshVerdict::Ok
}

#[derive(Debug, Clone)]
struct KnowledgeNote {
    /// Path relative to repo root, forward slashes (e.g. `docs/knowledge/foo.md`).
    rel_path: String,
    title: String,
    freshness: String,
    last_verified: Option<String>,
    verified_against: Vec<String>,
    body: String,
}

fn collect_notes(repo_root: &Path, vault: &Path) -> Vec<KnowledgeNote> {
    let mut out = Vec::new();
    let Ok(walker) = walkdir_md(vault) else {
        return out;
    };
    for path in walker {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.eq_ignore_ascii_case("index.md") || name.eq_ignore_ascii_case("CONVENTIONS.md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(note) = parse_note(&path, repo_root, &text) else {
            continue;
        };
        // Exclude stale and notes without verified_against.
        if note.freshness.eq_ignore_ascii_case("stale") || note.verified_against.is_empty() {
            continue;
        }
        out.push(note);
    }
    out
}

/// All parseable notes, including stale and unverified (refresh must see them).
/// Still skips `CONVENTIONS.md` (format doc, not a fact note).
fn collect_notes_for_refresh(repo_root: &Path, vault: &Path) -> Vec<KnowledgeNote> {
    let mut out = Vec::new();
    let Ok(walker) = walkdir_md(vault) else {
        return out;
    };
    for path in walker {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.eq_ignore_ascii_case("CONVENTIONS.md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(note) = parse_note(&path, repo_root, &text) else {
            continue;
        };
        out.push(note);
    }
    out
}

fn walkdir_md(vault: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(&path, files)?;
            } else if ft.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            {
                files.push(path);
            }
        }
        Ok(())
    }
    walk(vault, &mut files)?;
    files.sort();
    Ok(files)
}

fn parse_note(path: &Path, repo_root: &Path, text: &str) -> Option<KnowledgeNote> {
    let (fm, body) = parse_knowledge_frontmatter(text)?;
    let title = fm.get("title").cloned().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("note")
            .to_string()
    });
    let freshness = fm
        .get("freshness")
        .cloned()
        .unwrap_or_else(|| "check-on-touch".into());
    let last_verified = fm.get("last_verified").cloned().filter(|s| !s.is_empty());
    let verified_against = fm
        .get("verified_against")
        .map(|s| {
            s.split('\n')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let rel_path = path
        .strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    Some(KnowledgeNote {
        rel_path,
        title,
        freshness,
        last_verified,
        verified_against,
        body,
    })
}

/// Minimal frontmatter parser supporting scalars and YAML `- item` lists.
/// Returns `None` when frontmatter is missing/malformed (note skipped).
fn parse_knowledge_frontmatter(
    text: &str,
) -> Option<(std::collections::BTreeMap<String, String>, String)> {
    let source = text.trim_start_matches('\u{feff}');
    let first_end = source.find('\n').map(|i| i + 1).unwrap_or(source.len());
    if source[..first_end].trim_end() != "---" {
        return None;
    }
    let mut map = std::collections::BTreeMap::new();
    let mut offset = first_end;
    let mut list_key: Option<String> = None;
    let mut list_items: Vec<String> = Vec::new();

    let flush_list = |map: &mut std::collections::BTreeMap<String, String>,
                      list_key: &mut Option<String>,
                      list_items: &mut Vec<String>| {
        if let Some(k) = list_key.take() {
            map.insert(k, list_items.join("\n"));
            list_items.clear();
        }
    };

    while offset < source.len() {
        let rest = &source[offset..];
        let line_len = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        let raw = &rest[..line_len];
        if raw.trim_end() == "---" {
            flush_list(&mut map, &mut list_key, &mut list_items);
            let body = source[offset + line_len..].to_string();
            return Some((map, body));
        }
        let line = raw.trim_end();
        if let Some(item) = line.strip_prefix("  - ").or_else(|| {
            if line.starts_with("- ") && list_key.is_some() {
                Some(&line[2..])
            } else {
                None
            }
        }) {
            // Continuation of a YAML list under the current key.
            if list_key.is_some() {
                let item = item.trim().trim_matches('"').trim_matches('\'').to_string();
                if !item.is_empty() {
                    list_items.push(item);
                }
            }
        } else if let Some((key, value)) = line.split_once(':') {
            flush_list(&mut map, &mut list_key, &mut list_items);
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if value.is_empty() {
                list_key = Some(key);
            } else if let Some(inner) = value.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                let joined = inner
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                map.insert(key, joined);
            } else {
                map.insert(key, value.to_string());
            }
        } else if !line.trim().is_empty() {
            // Non-key prose inside frontmatter ends list accumulation.
            flush_list(&mut map, &mut list_key, &mut list_items);
        }
        offset += line_len;
    }
    None
}

fn render_index_map(vault: &Path) -> Option<String> {
    let text = std::fs::read_to_string(vault.join("index.md")).ok()?;
    let (_, body) = parse_knowledge_frontmatter(&text).unwrap_or((Default::default(), text));
    let mut map = String::from("### Vault map\n");
    let mut in_map = false;
    for line in body.lines() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("## Map") || t.eq_ignore_ascii_case("## map") {
            in_map = true;
            continue;
        }
        if in_map && t.starts_with("## ") {
            break;
        }
        if in_map && (t.starts_with("### ") || t.starts_with("- ") || t.starts_with("* ")) {
            map.push_str(line.trim_end());
            map.push('\n');
        }
    }
    if map.trim() == "### Vault map" {
        // Fall back: first ~30 non-empty body lines as a sketch.
        map.clear();
        map.push_str("### Vault map\n");
        for line in body.lines().filter(|l| !l.trim().is_empty()).take(30) {
            map.push_str(line.trim_end());
            map.push('\n');
        }
    }
    Some(map)
}

fn note_referenced(note: &KnowledgeNote, haystack_lower: &str) -> bool {
    let path_l = note.rel_path.to_ascii_lowercase();
    // Prefer explicit path references (full or vault-relative).
    if !path_l.is_empty() && haystack_lower.contains(&path_l) {
        return true;
    }
    if let Some(under) = path_l.strip_prefix("docs/knowledge/") {
        if under.len() >= 6 && haystack_lower.contains(under) {
            return true;
        }
    }
    // Basename only as a whole token (avoid "plan" matching every goal).
    if let Some(base) = Path::new(&note.rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
    {
        let base_l = base.to_ascii_lowercase();
        if base_l.len() >= 6 && contains_word(haystack_lower, &base_l) {
            return true;
        }
    }
    let title_l = note.title.to_ascii_lowercase();
    if title_l.len() >= 8 && contains_word(haystack_lower, &title_l) {
        return true;
    }
    false
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    for (i, _) in haystack.match_indices(needle) {
        let before_ok = i == 0
            || !haystack
                .as_bytes()
                .get(i - 1)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-');
        let after = i + needle.len();
        let after_ok = after >= haystack.len()
            || !haystack
                .as_bytes()
                .get(after)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-');
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

fn note_overlaps_paths(note: &KnowledgeNote, hints: &[String]) -> bool {
    if hints.is_empty() {
        return false;
    }
    for v in &note.verified_against {
        let v_norm = v.replace('\\', "/");
        for h in hints {
            if v_norm == *h || v_norm.ends_with(h) || h.ends_with(&v_norm) || h.contains(&v_norm) {
                return true;
            }
        }
    }
    false
}

fn format_note_excerpt(note: &KnowledgeNote) -> String {
    let mut body_lines = note
        .body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(12);
    let mut excerpt = String::new();
    for line in body_lines.by_ref() {
        if excerpt.len() > 600 {
            break;
        }
        excerpt.push_str(line);
        excerpt.push('\n');
    }
    let verified = note.verified_against.join(", ");
    format!(
        "### {} (`{}`)\nfreshness: {} · verified_against: {}\n{}\n",
        note.title, note.rel_path, note.freshness, verified, excerpt
    )
}

/// Push `chunk` only if it fits entirely under the remaining budget.
/// Prefer skipping a lower-ranked note over truncating it mid-body (D-C).
fn push_budgeted(out: &mut String, chunk: &str) -> bool {
    let remaining = KNOWLEDGE_INJECT_MAX_BYTES.saturating_sub(out.len());
    if remaining == 0 || chunk.len() > remaining {
        return false;
    }
    out.push_str(chunk);
    true
}

/// Push as much of `chunk` as fits (used for the vault map only).
fn push_truncated(out: &mut String, chunk: &str) {
    let remaining = KNOWLEDGE_INJECT_MAX_BYTES.saturating_sub(out.len());
    if remaining == 0 {
        return;
    }
    if chunk.len() <= remaining {
        out.push_str(chunk);
    } else {
        out.push_str(&truncate_to_bytes(chunk, remaining));
    }
}

fn truncate_to_bytes(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_note(dir: &Path, rel: &str, freshness: &str, verified: &[&str], body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut va = String::from("verified_against:\n");
        for v in verified {
            va.push_str(&format!("  - {v}\n"));
        }
        let text = format!(
            "---\ntitle: {rel}\nowner: agent\nfreshness: {freshness}\nlast_verified: 2026-07-08\n{va}---\n\n{body}\n"
        );
        fs::write(path, text).unwrap();
    }

    fn vault(root: &Path) {
        let v = root.join("docs/knowledge");
        fs::create_dir_all(v.join("architecture")).unwrap();
        fs::write(
            v.join("index.md"),
            "---\ntitle: index\nfreshness: live\nverified_against:\n  - AGENTS.md\n---\n\n# Vault\n\n## Map\n\n### architecture/\n- [Pipe](architecture/pipe.md)\n",
        )
        .unwrap();
        fs::write(v.join("CONVENTIONS.md"), "# conventions\n").unwrap();
    }

    #[test]
    fn missing_vault_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(render_knowledge_for_planning(tmp.path(), &KnowledgeQuery::default()).is_none());
    }

    #[test]
    fn stale_and_unverified_notes_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/stale.md",
            "stale",
            &["crates/engine/src/foo.rs"],
            "Should not inject.",
        );
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/bare.md",
            "live",
            &[],
            "No verified_against.",
        );
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/pipe.md",
            "live",
            &["crates/engine/src/orchestrator.rs"],
            "Good note about the pipeline.",
        );
        let q = KnowledgeQuery {
            goal: "read architecture/pipe.md please",
            ..Default::default()
        };
        let block = render_knowledge_for_planning(tmp.path(), &q).expect("block");
        assert!(block.contains("architecture/pipe.md"), "{block}");
        assert!(!block.contains("Should not inject"), "{block}");
        assert!(!block.contains("No verified_against"), "{block}");
        assert!(
            block.contains("Vault map") || block.contains("architecture/"),
            "{block}"
        );
    }

    #[test]
    fn respects_byte_budget() {
        let tmp = tempfile::tempdir().unwrap();
        vault(tmp.path());
        let big = "x".repeat(3000);
        for i in 0..5 {
            write_note(
                &tmp.path().join("docs/knowledge"),
                &format!("architecture/n{i}.md"),
                "live",
                &["crates/engine/src/orchestrator.rs"],
                &format!("note {i} {big}"),
            );
        }
        let q = KnowledgeQuery {
            goal: "architecture/n0.md architecture/n1.md architecture/n2.md architecture/n3.md architecture/n4.md",
            ..Default::default()
        };
        let block = render_knowledge_for_planning(tmp.path(), &q).expect("block");
        assert!(
            block.len() <= KNOWLEDGE_INJECT_MAX_BYTES,
            "len {} > {}",
            block.len(),
            KNOWLEDGE_INJECT_MAX_BYTES
        );
    }

    #[test]
    fn path_overlap_selects_tier3_when_not_named() {
        let tmp = tempfile::tempdir().unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "validation/gates.md",
            "check-on-touch",
            &["crates/engine/src/merge_gate.rs"],
            "Gate recipes.",
        );
        let hints = vec!["crates/engine/src/merge_gate.rs".into()];
        let q = KnowledgeQuery {
            goal: "improve merge flow",
            touch_hints: &hints,
            ..Default::default()
        };
        let block = render_knowledge_for_planning(tmp.path(), &q).expect("block");
        assert!(block.contains("validation/gates.md"), "{block}");
    }

    #[test]
    fn knowledge_budget_constant_is_separate_from_lessons() {
        assert_eq!(KNOWLEDGE_INJECT_MAX_BYTES, 4096);
        assert_ne!(
            KNOWLEDGE_INJECT_MAX_BYTES,
            crate::lessons::LESSONS_INJECT_MAX_BYTES
        );
    }

    fn git(root: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git")
    }

    fn git_init(root: &Path) {
        if !git(root, &["init", "-b", "main"]).status.success() {
            assert!(git(root, &["init"]).status.success());
        }
        assert!(git(root, &["config", "user.name", "kranz-test"])
            .status
            .success());
        assert!(git(root, &["config", "user.email", "test@kranz.local"])
            .status
            .success());
    }

    fn git_commit_all(root: &Path, msg: &str) {
        assert!(git(root, &["add", "-A"]).status.success());
        assert!(
            git(root, &["-c", "commit.gpgsign=false", "commit", "-m", msg])
                .status
                .success()
        );
    }

    #[test]
    fn knowledge_refresh_reports_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/ghost.md",
            "check-on-touch",
            &["does-not-exist.rs"],
            "A missing citation.",
        );
        let report = refresh_knowledge(tmp.path());
        assert!(
            report.check_needed(),
            "missing path must fail the refresh: {report:?}"
        );
        assert!(
            report.findings.iter().any(|f| {
                f.rel_path.contains("ghost.md")
                    && f.verdicts.iter().any(|v| {
                        matches!(
                            v,
                            RefreshVerdict::PathMissing { path } if path == "does-not-exist.rs"
                        )
                    })
            }),
            "{report:?}"
        );
    }

    #[test]
    fn knowledge_refresh_skips_commands_and_does_not_fail() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/cmd.md",
            "check-on-touch",
            &["AGENTS.md", "cargo test -p kranz-engine lessons"],
            "Path plus a command.",
        );
        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|f| f.rel_path.contains("cmd.md"))
            .expect("cmd note");
        assert!(
            finding.verdicts.iter().any(|v| {
                matches!(
                    v,
                    RefreshVerdict::CommandSkipped { command }
                        if command.contains("cargo test")
                )
            }),
            "{finding:?}"
        );
        assert!(
            !finding.verdicts.iter().any(RefreshVerdict::is_check_needed),
            "command-skipped must not fail: {finding:?}"
        );
    }

    #[test]
    fn knowledge_refresh_already_stale_does_not_fail() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/old.md",
            "stale",
            &[],
            "Old news.",
        );
        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|f| f.rel_path.contains("old.md"))
            .expect("stale note");
        assert!(
            finding
                .verdicts
                .iter()
                .any(|v| matches!(v, RefreshVerdict::AlreadyStale)),
            "{finding:?}"
        );
        assert!(
            !report.check_needed(),
            "already-stale alone must not fail: {report:?}"
        );
    }

    #[test]
    fn knowledge_refresh_reports_drifted_path() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        fs::write(tmp.path().join("AGENTS.md"), "v1\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/drift.md",
            "check-on-touch",
            &["AGENTS.md"],
            "Will drift.",
        );
        // Backdate last_verified so today's commit is after the check day.
        let note_path = tmp.path().join("docs/knowledge/architecture/drift.md");
        let text = fs::read_to_string(&note_path).unwrap();
        fs::write(
            &note_path,
            text.replace("last_verified: 2026-07-08", "last_verified: 2020-01-01"),
        )
        .unwrap();
        git_commit_all(tmp.path(), "seed");
        fs::write(tmp.path().join("AGENTS.md"), "v2\n").unwrap();
        git_commit_all(tmp.path(), "touch agents");
        let report = refresh_knowledge(tmp.path());
        assert!(
            report.check_needed(),
            "edited verified path must drift: {report:?}"
        );
        assert!(
            report.findings.iter().any(|f| {
                f.rel_path.contains("drift.md")
                    && f.verdicts.iter().any(|v| {
                        matches!(v, RefreshVerdict::PathDrifted { path } if path == "AGENTS.md")
                    })
            }),
            "{report:?}"
        );
    }
}
