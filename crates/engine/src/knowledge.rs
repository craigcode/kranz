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
use chrono::NaiveDate;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

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
    InvalidMetadata {
        field: String,
        value: Option<String>,
    },
    InvalidCitation {
        citation: String,
    },
    PathMissing {
        path: String,
    },
    PathDrifted {
        path: String,
    },
    CommandSkipped {
        command: String,
    },
    ProbeFailed {
        target: String,
        error: String,
    },
}

impl RefreshVerdict {
    /// Findings that should fail `kranz knowledge refresh` (exit 1).
    /// `already-stale` and `command-skipped` are reported but do not fail.
    pub fn is_check_needed(&self) -> bool {
        matches!(
            self,
            Self::Unverified
                | Self::InvalidMetadata { .. }
                | Self::InvalidCitation { .. }
                | Self::PathMissing { .. }
                | Self::PathDrifted { .. }
                | Self::ProbeFailed { .. }
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
/// Never rewrites a note. Never runs a `verified_against` entry prefixed with
/// `command:`; commands are reported as `command-skipped`. Every other entry
/// is a path (an optional `path:` prefix is accepted), including paths with
/// spaces. Path drift uses
/// [`GitRepo::path_changed_since`] when the repo is git and the note has
/// `last_verified`.
pub fn refresh_knowledge(repo_root: &Path) -> RefreshReport {
    let vault = repo_root.join("docs").join("knowledge");
    let git = GitRepo::open(repo_root).map_err(|err| err.to_string());
    let mut findings = Vec::new();
    if !vault.is_dir() {
        return RefreshReport { findings };
    }
    for source in collect_notes_for_refresh(repo_root, &vault) {
        match source {
            RefreshSource::Note(note) => findings.push(refresh_one(&note, repo_root, &git)),
            RefreshSource::Finding(finding) => findings.push(finding),
        }
    }
    findings.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    RefreshReport { findings }
}

fn refresh_one(
    note: &KnowledgeNote,
    repo_root: &Path,
    git: &std::result::Result<GitRepo, String>,
) -> RefreshFinding {
    let mut verdicts = Vec::new();
    if note.freshness.eq_ignore_ascii_case("stale") {
        verdicts.push(RefreshVerdict::AlreadyStale);
        return RefreshFinding {
            rel_path: note.rel_path.clone(),
            title: note.title.clone(),
            freshness: note.freshness.clone(),
            verdicts,
        };
    }
    if note.verified_against.is_empty() {
        verdicts.push(RefreshVerdict::Unverified);
        return RefreshFinding {
            rel_path: note.rel_path.clone(),
            title: note.title.clone(),
            freshness: note.freshness.clone(),
            verdicts,
        };
    }
    let Some(since) = validated_last_verified(note, &mut verdicts) else {
        return RefreshFinding {
            rel_path: note.rel_path.clone(),
            title: note.title.clone(),
            freshness: note.freshness.clone(),
            verdicts,
        };
    };
    for citation in &note.verified_against {
        verdicts.push(refresh_citation(citation, &since, repo_root, git));
    }
    RefreshFinding {
        rel_path: note.rel_path.clone(),
        title: note.title.clone(),
        freshness: note.freshness.clone(),
        verdicts,
    }
}

fn validated_last_verified(
    note: &KnowledgeNote,
    verdicts: &mut Vec<RefreshVerdict>,
) -> Option<String> {
    let Some(value) = note.last_verified.as_deref() else {
        verdicts.push(RefreshVerdict::InvalidMetadata {
            field: "last_verified".into(),
            value: None,
        });
        return None;
    };
    let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") else {
        verdicts.push(RefreshVerdict::InvalidMetadata {
            field: "last_verified".into(),
            value: Some(value.to_string()),
        });
        return None;
    };
    Some(date.format("%Y-%m-%d").to_string())
}

fn refresh_citation(
    citation: &str,
    since: &str,
    repo_root: &Path,
    git: &std::result::Result<GitRepo, String>,
) -> RefreshVerdict {
    let citation = citation.trim();
    if let Some(command) = citation.strip_prefix("command:") {
        let command = command.trim();
        return if command.is_empty() {
            RefreshVerdict::InvalidCitation {
                citation: citation.to_string(),
            }
        } else {
            RefreshVerdict::CommandSkipped {
                command: command.to_string(),
            }
        };
    }
    let path = citation
        .strip_prefix("path:")
        .map(str::trim)
        .unwrap_or(citation);
    if !citation_is_repo_relative(path) {
        return RefreshVerdict::InvalidCitation {
            citation: citation.to_string(),
        };
    }

    match citation_exists_inside_repo(repo_root, path) {
        Ok(false) => {
            return RefreshVerdict::PathMissing {
                path: path.to_string(),
            };
        }
        Ok(true) => {}
        Err(verdict) => return verdict,
    }

    let git = match git {
        Ok(git) => git,
        Err(error) => {
            return RefreshVerdict::ProbeFailed {
                target: path.to_string(),
                error: error.clone(),
            };
        }
    };
    match git.path_changed_since(path, since) {
        Ok(true) => RefreshVerdict::PathDrifted {
            path: path.to_string(),
        },
        Ok(false) => RefreshVerdict::Ok,
        Err(error) => RefreshVerdict::ProbeFailed {
            target: path.to_string(),
            error: error.to_string(),
        },
    }
}

fn citation_is_repo_relative(citation: &str) -> bool {
    !citation.is_empty()
        && Path::new(citation)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn citation_exists_inside_repo(
    repo_root: &Path,
    citation: &str,
) -> std::result::Result<bool, RefreshVerdict> {
    let path = repo_root.join(citation);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(RefreshVerdict::ProbeFailed {
                target: citation.to_string(),
                error: err.to_string(),
            });
        }
    }

    let canonical_root =
        std::fs::canonicalize(repo_root).map_err(|err| RefreshVerdict::ProbeFailed {
            target: citation.to_string(),
            error: err.to_string(),
        })?;
    let canonical_path =
        std::fs::canonicalize(&path).map_err(|err| RefreshVerdict::ProbeFailed {
            target: citation.to_string(),
            error: err.to_string(),
        })?;
    if !canonical_path.starts_with(canonical_root) {
        return Err(RefreshVerdict::InvalidCitation {
            citation: citation.to_string(),
        });
    }
    Ok(true)
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

enum RefreshSource {
    Note(KnowledgeNote),
    Finding(RefreshFinding),
}

/// All notes, including stale, unverified, and malformed sources (refresh must
/// report anything it cannot inspect). Still skips `CONVENTIONS.md` (format
/// doc, not a fact note).
fn collect_notes_for_refresh(repo_root: &Path, vault: &Path) -> Vec<RefreshSource> {
    let mut out = Vec::new();
    let walker = match walkdir_md(vault) {
        Ok(walker) => walker,
        Err(err) => {
            return vec![RefreshSource::Finding(refresh_problem(
                "docs/knowledge".into(),
                "Knowledge vault".into(),
                RefreshVerdict::ProbeFailed {
                    target: "docs/knowledge".into(),
                    error: err.to_string(),
                },
            ))];
        }
    };
    for path in walker {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.eq_ignore_ascii_case("CONVENTIONS.md") {
            continue;
        }
        let rel_path = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("knowledge note")
            .to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                out.push(RefreshSource::Finding(refresh_problem(
                    rel_path.clone(),
                    title,
                    RefreshVerdict::ProbeFailed {
                        target: rel_path,
                        error: err.to_string(),
                    },
                )));
                continue;
            }
        };
        let Some(note) = parse_note(&path, repo_root, &text) else {
            out.push(RefreshSource::Finding(refresh_problem(
                rel_path,
                title,
                RefreshVerdict::InvalidMetadata {
                    field: "frontmatter".into(),
                    value: None,
                },
            )));
            continue;
        };
        out.push(RefreshSource::Note(note));
    }
    out
}

fn refresh_problem(rel_path: String, title: String, verdict: RefreshVerdict) -> RefreshFinding {
    RefreshFinding {
        rel_path,
        title,
        freshness: "unknown".into(),
        verdicts: vec![verdict],
    }
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
        if v.trim_start().starts_with("command:") {
            continue;
        }
        let v_norm = v
            .trim()
            .strip_prefix("path:")
            .map(str::trim)
            .unwrap_or(v.trim())
            .replace('\\', "/");
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
            "---\ntitle: {rel}\nowner: agent\nfreshness: {freshness}\nlast_verified: 2099-01-01\n{va}---\n\n{body}\n"
        );
        fs::write(path, text).unwrap();
    }

    fn vault(root: &Path) {
        let v = root.join("docs/knowledge");
        fs::create_dir_all(v.join("architecture")).unwrap();
        fs::write(
            v.join("index.md"),
            "---\ntitle: index\nfreshness: live\nlast_verified: 2099-01-01\nverified_against:\n  - AGENTS.md\n---\n\n# Vault\n\n## Map\n\n### architecture/\n- [Pipe](architecture/pipe.md)\n",
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

    fn git_commit_all_at(root: &Path, msg: &str, timestamp: &str) {
        assert!(git(root, &["add", "-A"]).status.success());
        assert!(std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false", "commit", "-m", msg])
            .current_dir(root)
            .env("GIT_AUTHOR_DATE", timestamp)
            .env("GIT_COMMITTER_DATE", timestamp)
            .output()
            .expect("git commit")
            .status
            .success());
    }

    fn replace_note_date(root: &Path, rel: &str, replacement: Option<&str>) {
        let path = root.join("docs/knowledge").join(rel);
        let text = fs::read_to_string(&path).unwrap();
        let text = match replacement {
            Some(date) => text.replace(
                "last_verified: 2099-01-01",
                &format!("last_verified: {date}"),
            ),
            None => text
                .lines()
                .filter(|line| !line.starts_with("last_verified:"))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        fs::write(path, format!("{text}\n")).unwrap();
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
        git_init(tmp.path());
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/cmd.md",
            "check-on-touch",
            &["AGENTS.md", "command: cargo test -p kranz-engine lessons"],
            "Path plus a command.",
        );
        git_commit_all(tmp.path(), "seed");
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
        git_init(tmp.path());
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/old.md",
            "stale",
            &[],
            "Old news.",
        );
        git_commit_all(tmp.path(), "seed");
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
            text.replace("last_verified: 2099-01-01", "last_verified: 2020-01-01"),
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

    #[test]
    fn knowledge_refresh_reports_unverified_note() {
        let tmp = tempfile::tempdir().unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/unverified.md",
            "check-on-touch",
            &[],
            "No citations.",
        );

        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("unverified.md"))
            .expect("unverified finding");
        assert_eq!(finding.verdicts, [RefreshVerdict::Unverified]);
        assert!(report.check_needed());
    }

    /// `kranz knowledge refresh --json` prints this report verbatim, so a field
    /// rename here is a CLI contract break. Pin the wire shape.
    #[test]
    fn knowledge_refresh_report_serializes_verdicts_for_json_output() {
        let tmp = tempfile::tempdir().unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/unverified.md",
            "check-on-touch",
            &[],
            "No citations.",
        );

        let json = serde_json::to_value(refresh_knowledge(tmp.path())).unwrap();
        let finding = json["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .find(|finding| {
                finding["relPath"]
                    .as_str()
                    .is_some_and(|rel| rel.ends_with("unverified.md"))
            })
            .expect("unverified finding");
        assert_eq!(finding["freshness"], "check-on-touch");
        assert_eq!(
            finding["verdicts"],
            serde_json::json!([{"verdict": "unverified"}]),
            "{json:#}"
        );
    }

    #[test]
    fn knowledge_refresh_reports_missing_and_invalid_last_verified() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        for (name, replacement, verified) in [
            (
                "missing-date.md",
                None,
                "cargo test -p kranz-engine knowledge_refresh",
            ),
            ("bad-date.md", Some("2026-99-99"), "AGENTS.md"),
        ] {
            write_note(
                &tmp.path().join("docs/knowledge"),
                &format!("architecture/{name}"),
                "check-on-touch",
                &[verified],
                "Date metadata matters.",
            );
            replace_note_date(tmp.path(), &format!("architecture/{name}"), replacement);
        }

        let report = refresh_knowledge(tmp.path());
        for name in ["missing-date.md", "bad-date.md"] {
            let finding = report
                .findings
                .iter()
                .find(|finding| finding.rel_path.ends_with(name))
                .expect("date finding");
            assert!(finding.verdicts.iter().any(|verdict| matches!(
                verdict,
                RefreshVerdict::InvalidMetadata { field, .. } if field == "last_verified"
            )));
        }
        assert!(report.check_needed());
    }

    #[test]
    fn knowledge_refresh_reports_git_open_failure() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/no-git.md",
            "check-on-touch",
            &["AGENTS.md"],
            "Requires Git history.",
        );

        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("no-git.md"))
            .expect("git-open finding");
        assert!(finding.verdicts.iter().any(|verdict| matches!(
            verdict,
            RefreshVerdict::ProbeFailed { target, .. } if target == "AGENTS.md"
        )));
        assert!(report.check_needed());
    }

    #[test]
    fn knowledge_refresh_reports_git_log_failure() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/unborn.md",
            "check-on-touch",
            &["AGENTS.md"],
            "An unborn repository has no log to inspect.",
        );

        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("unborn.md"))
            .expect("git-log finding");
        assert!(finding.verdicts.iter().any(|verdict| matches!(
            verdict,
            RefreshVerdict::ProbeFailed { target, .. } if target == "AGENTS.md"
        )));
        assert!(report.check_needed());
    }

    #[test]
    fn knowledge_refresh_reports_malformed_note() {
        let tmp = tempfile::tempdir().unwrap();
        vault(tmp.path());
        fs::write(
            tmp.path().join("docs/knowledge/architecture/broken.md"),
            "# Missing frontmatter\n",
        )
        .unwrap();

        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("broken.md"))
            .expect("malformed finding");
        assert!(finding.verdicts.iter().any(|verdict| matches!(
            verdict,
            RefreshVerdict::InvalidMetadata { field, .. } if field == "frontmatter"
        )));
        assert!(report.check_needed());
    }

    #[test]
    fn knowledge_refresh_all_ok_is_clean() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/clean.md",
            "check-on-touch",
            &["AGENTS.md"],
            "No changes after verification.",
        );
        git_commit_all(tmp.path(), "seed");

        let report = refresh_knowledge(tmp.path());
        assert!(!report.check_needed(), "{report:?}");
        assert!(report
            .findings
            .iter()
            .flat_map(|finding| &finding.verdicts)
            .all(|verdict| matches!(verdict, RefreshVerdict::Ok)));
    }

    #[test]
    fn knowledge_refresh_checks_existing_path_with_spaces() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        fs::write(tmp.path().join("path with spaces.md"), "evidence\n").unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/spaces.md",
            "check-on-touch",
            &["path with spaces.md"],
            "Spaces do not imply a command.",
        );
        git_commit_all(tmp.path(), "seed");

        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("spaces.md"))
            .expect("spaces finding");
        assert_eq!(finding.verdicts, [RefreshVerdict::Ok]);
    }

    #[test]
    fn knowledge_refresh_missing_path_with_spaces_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/missing-spaces.md",
            "check-on-touch",
            &["deleted path with spaces.md"],
            "A deleted path stays a path.",
        );
        git_commit_all(tmp.path(), "seed");

        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("missing-spaces.md"))
            .expect("spaces finding");
        assert!(report.check_needed(), "{report:?}");
        assert_eq!(
            finding.verdicts,
            [RefreshVerdict::PathMissing {
                path: "deleted path with spaces.md".to_string()
            }]
        );
    }

    #[test]
    fn knowledge_refresh_rejects_outside_repo_citation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir(&root).unwrap();
        fs::write(tmp.path().join("outside file.md"), "private\n").unwrap();
        vault(&root);
        write_note(
            &root.join("docs/knowledge"),
            "architecture/outside.md",
            "check-on-touch",
            &["../outside file.md"],
            "Must stay inside the repository.",
        );

        let report = refresh_knowledge(&root);
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("outside.md"))
            .expect("outside finding");
        assert!(finding.verdicts.iter().any(|verdict| matches!(
            verdict,
            RefreshVerdict::InvalidCitation { citation } if citation == "../outside file.md"
        )));
        assert!(report.check_needed());
    }

    #[test]
    fn knowledge_refresh_already_stale_skips_broken_citations() {
        let tmp = tempfile::tempdir().unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/stale-broken.md",
            "stale",
            &["missing.rs"],
            "Already excluded from injection.",
        );

        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("stale-broken.md"))
            .expect("stale finding");
        assert_eq!(finding.verdicts, [RefreshVerdict::AlreadyStale]);
    }

    #[test]
    fn knowledge_refresh_same_day_commit_is_not_drift() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        fs::write(tmp.path().join("AGENTS.md"), "rules\n").unwrap();
        vault(tmp.path());
        write_note(
            &tmp.path().join("docs/knowledge"),
            "architecture/same-day.md",
            "check-on-touch",
            &["AGENTS.md"],
            "Verified after the same-day change.",
        );
        replace_note_date(tmp.path(), "architecture/same-day.md", Some("2026-07-08"));
        git_commit_all_at(tmp.path(), "seed", "2026-07-08T12:00:00Z");

        let report = refresh_knowledge(tmp.path());
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.rel_path.ends_with("same-day.md"))
            .expect("same-day finding");
        assert_eq!(finding.verdicts, [RefreshVerdict::Ok]);
    }
}
