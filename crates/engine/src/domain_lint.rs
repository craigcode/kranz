//! Clean-room domain-vocabulary lint (KRZ-314; the positioning ADR's IP
//! boundary — `docs/knowledge/decisions/positioning-governance-evidence-layer.md`).
//!
//! Kranz core is domain-free: the positioning ADR's protected vocabulary
//! classes (consumer names, legacy-platform terms, consumer schema
//! identifiers) must never appear in code, comments, docs, tickets,
//! fixtures, or example config. Domain knowledge ships in private packs
//! behind the pack contract (KRZ-313). This module is the mechanical guard:
//! it scans the scoped tree and fails when a banned term appears, naming
//! file and line.
//!
//! # Why salted hashes, and what the salt does NOT do
//!
//! The denylist must not itself leak the vocabulary it bans, so the committed
//! config (`.kranz/domain-denylist.json`) stores ONLY salted SHA-256 hashes of
//! normalized terms; the plaintext list lives outside the repo (the gitignored
//! `.kranz/domain-terms.local`, regenerated into the config by
//! `kranz domain-lint --seed-config`). The salt is committed and therefore NOT
//! a secret: it does not make the list unguessable — a motivated reader who
//! suspects a term can recompute `sha256(salt || NUL || term)` and confirm it.
//! What the salt buys is that the hashes are not bare `sha256(term)` values a
//! rainbow table or a search engine resolves with zero per-repo effort. The
//! real boundary is that plaintext never enters the repo; the hash config is
//! the auditable shadow of it. The salt is generated once and preserved across
//! reseeds so waiver fingerprints stay stable.
//!
//! # Normalization (the matching contract)
//!
//! A term (or a span of scanned text) normalizes to a sequence of TOKENS:
//! maximal runs of ASCII alphanumeric characters, ASCII-lowercased, joined
//! with one space. Everything else — whitespace, punctuation, non-ASCII — is
//! a separator. Consequences, all deliberate:
//!
//! - case, spacing, and punctuation variants of a term match (`X y`, `x-y`,
//!   `x.y`, `x  y` are the same term);
//! - a phrase may span a line break (prose wraps; scanning is over the whole
//!   file's token stream, with the hit reported at the first token's line);
//! - camelCase compounds are ONE token (`zzAcme` → `zzacme`) and do NOT match
//!   the two-token term `zz acme` — list authors seed both forms when both
//!   matter;
//! - terms longer than [`MAX_TERM_TOKENS`] tokens are refused at seed time
//!   (the scanner never builds longer n-grams, so a longer term could never
//!   match — failing loudly beats a silently vacuous entry).
//!
//! # Scope
//!
//! The candidate set is `git ls-files --cached --others --exclude-standard`:
//! tracked files plus untracked-but-not-ignored files. That is the only
//! honest way to "respect .gitignore" — git's own ignore engine decides — and
//! it makes the local command and the CI job agree on the same tree. On top
//! of that the lint excludes, by path:
//!
//! - `.kranz/missions/` — mission runtime artifacts are OPERATOR content
//!   (ticket text, plans, reports quote whatever the operator's domain
//!   actually is; the boundary governs kranz core, not what missions were
//!   about);
//! - the lint's own files (the denylist config, the allowlist, the plaintext
//!   terms file) — they contain only hashes and fingerprints, but the guard
//!   never reads its own policy;
//! - binary files (a NUL byte in the content) and files over
//!   [`MAX_FILE_BYTES`], skipped whole — a bounded read keeps the lint's cost
//!   predictable and a partial scan would be a false sense of coverage (same
//!   posture as `scrub`'s scan bound).
//!
//! # Waivers
//!
//! A finding carries a FINGERPRINT:
//! `sha256(salt || NUL || normalized-term || NUL || repo-relative-path)`,
//! truncated to 24 hex chars. `.kranz/domain-allowlist` holds one fingerprint
//! per line — the reviewed-waiver idiom of `.kranz/secret-allowlist`, with the
//! same rule: comments describe the waived hit, they never quote it. Unlike
//! the secret allowlist the fingerprint is PATH-SCOPED: a banned term waived
//! in one file still trips everywhere else, because a global pass on a
//! vocabulary boundary is a much wider hole than a global pass on one
//! detector's false positive. A waiver covers every occurrence of that term
//! in that path, present and future — re-review on any edit that leans on it.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Committed denylist config: salt + hashed banned terms, never plaintext.
pub const DENYLIST_PATH: &str = ".kranz/domain-denylist.json";

/// Committed reviewed waivers, one finding fingerprint per line (comments
/// describe, never quote — the `.kranz/secret-allowlist` idiom).
pub const ALLOWLIST_PATH: &str = ".kranz/domain-allowlist";

/// Gitignored operator-local plaintext source the denylist is seeded from
/// (`kranz domain-lint --seed-config`). Never committed: it IS the vocabulary
/// the boundary protects.
pub const TERMS_LOCAL_PATH: &str = ".kranz/domain-terms.local";

/// Longest banned phrase the scanner can match, in normalized tokens. Seed
/// refuses longer terms rather than entering them vacuously.
pub const MAX_TERM_TOKENS: usize = 8;

/// Files larger than this are skipped whole (see the module docs' scope
/// section); 8 MiB mirrors `scrub`'s scan bound.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Config format version. Bumping lets a future format change fail loudly on
/// old readers instead of silently linting with a misread policy.
const CONFIG_VERSION: u32 = 1;

/// One banned-term hit. Carries the path-scoped waiver fingerprint, the
/// repo-relative path, and the 1-based line of the match's first token —
/// NEVER the matched text (the text is the thing the boundary protects).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainFinding {
    pub fingerprint: String,
    pub path: String,
    pub line: usize,
}

/// The loaded denylist: the salt plus the set of hashed normalized terms.
#[derive(Debug, Clone)]
pub struct Denylist {
    salt: String,
    hashes: BTreeSet<String>,
}

impl Denylist {
    /// Number of banned terms (hashes) — the only thing a report may say
    /// about the list's contents.
    pub fn term_count(&self) -> usize {
        self.hashes.len()
    }
}

/// What a lint run found. `files_skipped` counts binary/oversized/unreadable
/// candidates — visible so a swelling skip count is noticeable, never fatal.
#[derive(Debug, Clone, Default)]
pub struct LintReport {
    pub findings: Vec<DomainFinding>,
    pub files_scanned: usize,
    pub files_skipped: usize,
}

impl LintReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// The on-disk config shape. `hash` records the construction in words so a
/// reader of the JSON never has to guess the input layout.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DenylistConfig {
    version: u32,
    salt: String,
    hash: String,
    terms: BTreeSet<String>,
}

const HASH_SCHEME: &str = "sha256(salt || NUL || normalized-term)";

/// Normalize a term or text span per the module-docs contract: maximal ASCII
/// alphanumeric runs, lowercased, joined with one space.
pub fn normalize_term(text: &str) -> String {
    tokens(text)
        .into_iter()
        .map(|(token, _)| token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split `text` into normalized tokens with the byte offset of each token's
/// start in the ORIGINAL text (offsets feed the line-number lookup; the
/// normalized form alone cannot locate a hit).
fn tokens(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphanumeric() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            out.push((text[start..i].to_ascii_lowercase(), start));
        } else {
            i += 1;
        }
    }
    out
}

/// `sha256(salt || NUL || input)` as lowercase hex — the denylist entry form.
fn salted_hash(salt: &str, input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update([0]);
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The waiver fingerprint for one normalized term in one repo-relative path:
/// the salted hash truncated to 24 hex chars, path-scoped (see module docs).
pub fn waiver_fingerprint(salt: &str, normalized_term: &str, path: &str) -> String {
    let full = salted_hash(salt, &format!("{normalized_term}\0{path}"));
    full[..24].to_string()
}

fn is_64_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Parse and VALIDATE a denylist config. Every form rule fails loudly: a
/// config that does not parse exactly is a policy the lint cannot honestly
/// enforce, and an empty or malformed term set is a silently vacuous guard.
/// Nothing but 64-hex hashes and a 64-hex salt is accepted — the committed
/// config must carry no readable vocabulary.
pub fn load_denylist(text: &str) -> Result<Denylist> {
    let config: DenylistConfig =
        serde_json::from_str(text).context("domain denylist config is not valid JSON")?;
    if config.version != CONFIG_VERSION {
        bail!(
            "domain denylist config version {} is unsupported (expected {CONFIG_VERSION})",
            config.version
        );
    }
    if !is_64_hex(&config.salt) {
        bail!("domain denylist salt must be 64 lowercase hex characters");
    }
    if config.hash != HASH_SCHEME {
        bail!("domain denylist hash scheme is not the documented one");
    }
    if config.terms.is_empty() {
        bail!("domain denylist has no terms — an empty denylist is a vacuous guard");
    }
    for term in &config.terms {
        if !is_64_hex(term) {
            bail!("domain denylist entries must be 64 lowercase hex (salted hashes only)");
        }
    }
    Ok(Denylist {
        salt: config.salt,
        hashes: config.terms,
    })
}

/// Render a denylist config from a plaintext terms file (one term per line,
/// `#` comment lines allowed), keeping an EXISTING config's salt when one is
/// passed so waiver fingerprints survive reseeding; a fresh salt otherwise.
/// The output is hashes only — this function is the one place plaintext and
/// the committed form meet, and nothing plaintext crosses.
pub fn seed_config(existing_config: Option<&str>, terms_text: &str) -> Result<String> {
    let salt = match existing_config {
        Some(existing) => {
            load_denylist(existing)
                .context("existing denylist config cannot be reseeded")?
                .salt
        }
        None => format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
    };

    let mut hashes = BTreeSet::new();
    for (index, line) in terms_text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let normalized = normalize_term(line);
        if normalized.is_empty() {
            // A line with no ASCII-alphanumeric content carries no term;
            // skipping it quietly mirrors blank-line handling.
            continue;
        }
        let count = normalized.split(' ').count();
        if count > MAX_TERM_TOKENS {
            // Named by line, never by content: the error itself must not leak.
            bail!(
                "terms file line {} normalizes to {count} tokens (max {MAX_TERM_TOKENS})",
                index + 1
            );
        }
        hashes.insert(salted_hash(&salt, &normalized));
    }
    if hashes.is_empty() {
        bail!("terms file yielded no terms — refusing to seed an empty (vacuous) denylist");
    }

    let config = DenylistConfig {
        version: CONFIG_VERSION,
        salt,
        hash: HASH_SCHEME.to_string(),
        terms: hashes,
    };
    Ok(serde_json::to_string_pretty(&config)? + "\n")
}

/// True when a repo-relative path is OUT of scope by rule (see the module
/// docs): mission runtime artifacts are operator content, and the lint never
/// reads its own policy files.
pub fn is_excluded_path(path: &Path) -> bool {
    let path = path.to_string_lossy();
    let path = path.strip_prefix("./").unwrap_or(&path);
    path.starts_with(".kranz/missions/")
        || path == DENYLIST_PATH
        || path == ALLOWLIST_PATH
        || path == TERMS_LOCAL_PATH
}

/// Enumerate lint candidates under `repo_root`: `git ls-files --cached
/// --others --exclude-standard`, i.e. tracked plus untracked-but-not-ignored
/// files — git's own ignore engine is the only faithful .gitignore reader,
/// and it keeps the local command and CI on the same scope. Excluded paths
/// ([`is_excluded_path`]) are filtered here so they are never even opened.
pub fn enumerate_scoped_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    // The lint runs operator-side on the operator's own checkout (or a CI
    // checkout of it); plain git with no hook surface is the right posture —
    // ls-files executes nothing from the tree.
    let out = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(repo_root)
        .output()
        .context("spawn git ls-files")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut paths: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .filter(|path| !is_excluded_path(path))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Byte offsets of every line start in `text` (line 1 starts at 0).
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

/// Scan one file's text. Every contiguous token n-gram up to
/// [`MAX_TERM_TOKENS`] is hashed and compared against the denylist — the
/// config stores hashes only, so the scanner cannot know each term's length
/// and must try them all (cost: a handful of short hashes per token).
fn scan_text(denylist: &Denylist, path: &str, text: &str, findings: &mut Vec<DomainFinding>) {
    let tokens = tokens(text);
    let starts = line_starts(text);
    for start in 0..tokens.len() {
        let end = (start + MAX_TERM_TOKENS).min(tokens.len());
        let mut ngram = String::new();
        for (token, _) in &tokens[start..end] {
            if !ngram.is_empty() {
                ngram.push(' ');
            }
            ngram.push_str(token);
            if denylist
                .hashes
                .contains(&salted_hash(&denylist.salt, &ngram))
            {
                let line = starts.partition_point(|offset| *offset <= tokens[start].1);
                findings.push(DomainFinding {
                    fingerprint: waiver_fingerprint(&denylist.salt, &ngram, path),
                    path: path.to_string(),
                    line,
                });
            }
        }
    }
}

/// Lint explicit repo-relative `paths` against the `denylist`. Waivers are
/// applied by the caller ([`filter_allowed`]); this is the raw hit stream.
/// Skips (binary, oversized, unreadable, non-regular) are counted, never
/// fatal — the operator's own tree is the input, and one bad file must not
/// silence the rest of it.
pub fn lint_files(repo_root: &Path, paths: &[PathBuf], denylist: &Denylist) -> LintReport {
    let mut report = LintReport::default();
    for path in paths {
        if is_excluded_path(path) {
            continue;
        }
        let full = repo_root.join(path);
        // symlink_metadata + is_file: never read through a symlink — a
        // checked-in link carries no lintable bytes of its own (scrub's
        // posture, minus the no-follow machinery: this scanner walks the
        // operator's checkout, not a worker-planted tree).
        let Ok(metadata) = std::fs::symlink_metadata(&full) else {
            report.files_skipped += 1;
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            report.files_skipped += 1;
            continue;
        }
        let Ok(bytes) = std::fs::read(&full) else {
            report.files_skipped += 1;
            continue;
        };
        // Binary content is unlintable prose-wise; a NUL byte is the marker.
        if bytes.contains(&0) {
            report.files_skipped += 1;
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let path_str = path.to_string_lossy().replace('\\', "/");
        scan_text(denylist, &path_str, &text, &mut report.findings);
        report.files_scanned += 1;
    }
    report
}

/// Drop findings whose fingerprint is waived. The allowlist text format is
/// the secret allowlist's exactly (`scrub::read_allowlist_text`): one
/// fingerprint per line, `#` comments, comments describe and never quote.
pub fn filter_allowed(
    findings: Vec<DomainFinding>,
    allowed: &BTreeSet<String>,
) -> Vec<DomainFinding> {
    findings
        .into_iter()
        .filter(|finding| !allowed.contains(&finding.fingerprint))
        .collect()
}

/// The full local/CI flow: enumerate the scoped tree, lint it, apply waivers.
pub fn lint_tree(
    repo_root: &Path,
    denylist: &Denylist,
    allowed: &BTreeSet<String>,
) -> Result<LintReport> {
    let paths = enumerate_scoped_files(repo_root)?;
    let mut report = lint_files(repo_root, &paths, denylist);
    report.findings = filter_allowed(report.findings, allowed);
    // Deterministic output order: the same tree always reports the same way.
    report
        .findings
        .sort_by(|a, b| (&a.path, a.line, &a.fingerprint).cmp(&(&b.path, b.line, &b.fingerprint)));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All test vocabulary is synthetic (`zz-` convention) — the committed
    /// fixture vocabulary must never be a real protected term.
    const TEST_TERMS: &str = "# synthetic fixture vocabulary, never real\n\
                              zz-acme lint canary\n\
                              zz other term\n";

    fn test_denylist() -> Denylist {
        let config = seed_config(None, TEST_TERMS).expect("seed test config");
        load_denylist(&config).expect("parse seeded config")
    }

    #[test]
    fn domain_lint_normalize_collapses_case_spacing_and_punctuation() {
        assert_eq!(normalize_term("ZZ  Acme—Corp!"), "zz acme corp");
        assert_eq!(normalize_term("zz-acme"), normalize_term("ZZ ACME"));
        assert_eq!(normalize_term("  x.y  "), "x y");
        // CamelCase compounds are one token (documented contract).
        assert_eq!(normalize_term("zzAcme"), "zzacme");
        // Non-ASCII is a separator, not a token character.
        assert_eq!(normalize_term("café"), "caf");
    }

    #[test]
    fn domain_lint_seed_config_emits_hashes_only_and_roundtrips() {
        let config = seed_config(None, TEST_TERMS).expect("seed");
        // The committed form must not carry the plaintext it was built from.
        for fragment in ["zz", "acme", "canary", "lint"] {
            assert!(
                !config.contains(fragment),
                "seeded config leaks plaintext fragment {fragment:?}: {config}"
            );
        }
        let value: serde_json::Value = serde_json::from_str(&config).expect("valid json");
        let keys: Vec<&str> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["hash", "salt", "terms", "version"]);
        // And it must work: a lint with this config trips on the seeded terms.
        let denylist = load_denylist(&config).expect("load");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("f.md"),
            "mentions the ZZ-Acme lint canary\n",
        )
        .unwrap();
        let report = lint_files(dir.path(), &[PathBuf::from("f.md")], &denylist);
        assert_eq!(report.findings.len(), 1, "{report:?}");
        assert_eq!(report.findings[0].line, 1);
    }

    #[test]
    fn domain_lint_seeded_term_trips_naming_file_and_line_then_removal_goes_green() {
        let denylist = test_denylist();
        let dir = tempfile::tempdir().unwrap();
        let path = PathBuf::from("docs/scratch-fixture.md");
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join(&path),
            "line one\nsecond line plants the zz acme lint canary here\nthird\n",
        )
        .unwrap();

        let report = lint_files(dir.path(), std::slice::from_ref(&path), &denylist);
        assert_eq!(report.findings.len(), 1, "{report:?}");
        assert_eq!(report.findings[0].path, "docs/scratch-fixture.md");
        assert_eq!(report.findings[0].line, 2, "{report:?}");

        // Removing the term goes green.
        std::fs::write(dir.path().join(&path), "line one\nsecond line\nthird\n").unwrap();
        let report = lint_files(dir.path(), &[path], &denylist);
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn domain_lint_phrase_spanning_a_line_break_matches_at_first_token_line() {
        let denylist = test_denylist();
        let dir = tempfile::tempdir().unwrap();
        // "zz acme lint canary" split across lines 2 and 3.
        std::fs::write(
            dir.path().join("wrap.md"),
            "one\nplants the zz\nacme lint canary here\n",
        )
        .unwrap();
        let report = lint_files(dir.path(), &[PathBuf::from("wrap.md")], &denylist);
        assert_eq!(report.findings.len(), 1, "{report:?}");
        assert_eq!(report.findings[0].line, 2, "{report:?}");
    }

    #[test]
    fn domain_lint_waiver_suppresses_only_the_waived_path() {
        let denylist = test_denylist();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "zz acme lint canary\n").unwrap();
        std::fs::write(dir.path().join("b.md"), "zz acme lint canary\n").unwrap();
        let paths = vec![PathBuf::from("a.md"), PathBuf::from("b.md")];

        let report = lint_files(dir.path(), &paths, &denylist);
        assert_eq!(report.findings.len(), 2);
        // The finding's own fingerprint is the waiver token.
        let waiver: BTreeSet<String> = report
            .findings
            .iter()
            .filter(|f| f.path == "a.md")
            .map(|f| f.fingerprint.clone())
            .collect();
        let remaining = filter_allowed(report.findings, &waiver);
        assert_eq!(remaining.len(), 1, "{remaining:?}");
        assert_eq!(remaining[0].path, "b.md", "a waiver is path-scoped");
    }

    #[test]
    fn domain_lint_load_rejects_non_hash_form_configs() {
        // A readable entry is precisely the leak the config form forbids.
        for bad in [
            r#"{"version":1,"salt":"aaaa","hash":"sha256(salt || NUL || normalized-term)","terms":["ab"]}"#,
            r#"{"version":1,"salt":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","hash":"sha256(salt || NUL || normalized-term)","terms":["zz acme"]}"#,
            r#"{"version":2,"salt":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","hash":"sha256(salt || NUL || normalized-term)","terms":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}"#,
            r#"{"version":1,"salt":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","hash":"sha256(salt || NUL || normalized-term)","terms":[]}"#,
        ] {
            assert!(load_denylist(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn domain_lint_seed_refuses_overlong_terms_and_empty_inputs() {
        let long = (0..=MAX_TERM_TOKENS)
            .map(|i| format!("t{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(seed_config(None, &long).is_err(), "overlong term seeded");
        assert!(seed_config(None, "# only comments\n\n").is_err());
    }

    #[test]
    fn domain_lint_seed_preserves_salt_across_reseeds() {
        let first = seed_config(None, "zz one\n").unwrap();
        let second = seed_config(Some(&first), "zz one\nzz two\n").unwrap();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(first["salt"], second["salt"], "reseed must keep the salt");
        assert_eq!(second["terms"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn domain_lint_excluded_paths_cover_missions_and_own_config() {
        assert!(is_excluded_path(Path::new(".kranz/missions/m-zz/plan.md")));
        assert!(is_excluded_path(Path::new(DENYLIST_PATH)));
        assert!(is_excluded_path(Path::new(ALLOWLIST_PATH)));
        assert!(is_excluded_path(Path::new(TERMS_LOCAL_PATH)));
        assert!(!is_excluded_path(Path::new("crates/engine/src/lib.rs")));
        assert!(!is_excluded_path(Path::new(
            ".kranz/tickets/some-ticket.md"
        )));
    }

    /// The COMMITTED config is the boundary's standing policy: it must parse,
    /// carry hash-form entries only (no readable vocabulary can hide in a
    /// 64-hex set with a fixed key allow-list), and never contain the
    /// synthetic test vocabulary — the one direction of contamination a test
    /// can actually assert.
    #[test]
    fn domain_lint_committed_config_is_hash_form_only() {
        let committed = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.kranz/domain-denylist.json"
        );
        let text = std::fs::read_to_string(committed)
            .unwrap_or_else(|err| panic!("read {committed}: {err}"));
        let value: serde_json::Value =
            serde_json::from_str(&text).expect("committed config parses");
        let object = value.as_object().expect("object");
        for key in object.keys() {
            assert!(
                matches!(key.as_str(), "version" | "salt" | "hash" | "terms"),
                "unexpected key {key:?} — nowhere for readable terms to hide"
            );
        }
        let denylist = load_denylist(&text).expect("committed config validates");
        assert!(denylist.hashes.len() >= 3, "committed denylist is seeded");
        let synthetic = salted_hash(&denylist.salt, "zz acme lint canary");
        assert!(
            !denylist.hashes.contains(&synthetic),
            "synthetic test vocabulary must never seed the real config"
        );
    }
}
