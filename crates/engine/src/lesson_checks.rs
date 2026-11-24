//! Lesson check records: a hand-readable companion file per lesson that
//! records whether a deterministic check could catch the same mistake.
//!
//! The companion file is `.kranz/lessons/<mission-id>.check` — deliberately
//! not `.md`, so [`crate::lessons::render_lessons_manifest`], which only ever
//! reads `.md` names out of the index, can never be made to inject it. This
//! module only reads; it never writes, creates, or deletes anything, and it
//! never follows a symlink.

use crate::error::{EngineError, Result};
use crate::lessons::{
    atomic_replace, ensure_absent_or_regular, first_nonempty_line, open_lessons_dir,
    parse_manifest_line, read_existing_regular,
};
use crate::paths::MissionPaths;
use cap_std::fs::Dir;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Classification of whether a deterministic check could have caught the
/// mistake a lesson describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckClass {
    Checkable,
    NotCheckable,
    Unclassified,
}

impl CheckClass {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckClass::Checkable => "checkable",
            CheckClass::NotCheckable => "not-checkable",
            CheckClass::Unclassified => "unclassified",
        }
    }

    fn parse(value: &str) -> Option<CheckClass> {
        match value.to_ascii_lowercase().as_str() {
            "checkable" => Some(CheckClass::Checkable),
            "not-checkable" => Some(CheckClass::NotCheckable),
            "unclassified" => Some(CheckClass::Unclassified),
            _ => None,
        }
    }
}

/// Which side of the record produced the effective classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckSource {
    Operator,
    Orchestrator,
    None,
}

/// A parsed `.check` companion record. A classification value of
/// `unclassified` is equivalent to the field being unset, and an empty
/// `link:` value is equivalent to no link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LessonCheckRecord {
    pub orchestrator: Option<CheckClass>,
    pub operator: Option<CheckClass>,
    pub link: Option<String>,
}

impl LessonCheckRecord {
    /// Effective classification: operator wins if set, else orchestrator,
    /// else [`CheckClass::Unclassified`].
    fn effective(&self) -> (CheckClass, CheckSource) {
        if let Some(class) = self.operator {
            return (class, CheckSource::Operator);
        }
        if let Some(class) = self.orchestrator {
            return (class, CheckSource::Orchestrator);
        }
        (CheckClass::Unclassified, CheckSource::None)
    }
}

/// Write a `.check` companion record for `mission_id`, following the same
/// no-follow, symlink-refusing, atomic-replace discipline as
/// [`crate::lessons::write_lesson`]. Returns the path written.
///
/// Never used to clear an existing record: a symlink or other non-regular
/// file already at `<mission_id>.check` is refused rather than replaced, the
/// same as [`ensure_absent_or_regular`] refuses for lesson bodies.
pub(crate) fn write_check_record(
    repo_root: &Path,
    mission_id: &str,
    record: &LessonCheckRecord,
) -> Result<PathBuf> {
    if !MissionPaths::is_safe_id(mission_id) {
        return Err(EngineError::InvalidState(format!(
            "unsafe mission id for lesson check record: {mission_id}"
        )));
    }
    let lessons = open_lessons_dir(repo_root, true)?;
    let name = format!("{mission_id}.check");
    ensure_absent_or_regular(&lessons, &name)?;
    let text = render_check_record(record, mission_id);
    atomic_replace(&lessons, &name, text.as_bytes())?;
    Ok(repo_root.join(".kranz").join("lessons").join(name))
}

/// Parse a `.check` companion file's text.
///
/// Blank lines and lines starting with `#` are ignored. Every other line
/// must be `key: value` (split on the first `:`, both sides trimmed).
/// Unknown keys, lines without a `:`, and unknown classification values each
/// produce a warning naming the 1-based line number and the line's text; the
/// offending field is left unset. A duplicate key keeps the last value and
/// warns about the earlier one being overwritten.
pub fn parse_check_record(text: &str) -> (LessonCheckRecord, Vec<String>) {
    let mut record = LessonCheckRecord::default();
    let mut warnings = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            warnings.push(format!("line {line_no}: missing ':' separator: {raw_line}"));
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "orchestrator" => match CheckClass::parse(value) {
                Some(class) => {
                    if record.orchestrator.is_some() {
                        warnings.push(format!(
                            "line {line_no}: duplicate key 'orchestrator' overwrites earlier value"
                        ));
                    }
                    record.orchestrator = if class == CheckClass::Unclassified {
                        None
                    } else {
                        Some(class)
                    };
                }
                None => {
                    warnings.push(format!(
                        "line {line_no}: unknown classification value '{value}': {raw_line}"
                    ));
                }
            },
            "operator" => match CheckClass::parse(value) {
                Some(class) => {
                    if record.operator.is_some() {
                        warnings.push(format!(
                            "line {line_no}: duplicate key 'operator' overwrites earlier value"
                        ));
                    }
                    record.operator = if class == CheckClass::Unclassified {
                        None
                    } else {
                        Some(class)
                    };
                }
                None => {
                    warnings.push(format!(
                        "line {line_no}: unknown classification value '{value}': {raw_line}"
                    ));
                }
            },
            "link" => {
                if record.link.is_some() {
                    warnings.push(format!(
                        "line {line_no}: duplicate key 'link' overwrites earlier value"
                    ));
                }
                record.link = if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            other => {
                warnings.push(format!("line {line_no}: unknown key '{other}': {raw_line}"));
            }
        }
    }

    (record, warnings)
}

/// Render a `.check` companion record as hand-editable text. Round-trips
/// through [`parse_check_record`].
pub fn render_check_record(record: &LessonCheckRecord, mission_id: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Lesson check record for {mission_id}.\n\
         # The operator may set 'operator:' and 'link:' by hand; the operator's\n\
         # value wins over the orchestrator's classification.\n"
    ));
    if let Some(class) = record.orchestrator {
        out.push_str(&format!("orchestrator: {}\n", class.as_str()));
    }
    if let Some(class) = record.operator {
        out.push_str(&format!("operator: {}\n", class.as_str()));
    }
    if let Some(link) = &record.link {
        out.push_str(&format!("link: {link}\n"));
    }
    out
}

/// One lesson in a [`LessonListing`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LessonEntry {
    pub lesson: String,
    pub mission_id: String,
    pub summary: String,
    pub classification: CheckClass,
    pub source: CheckSource,
    pub link: Option<String>,
}

/// The result of listing lessons: entries in index (oldest-first) order,
/// plus any warnings encountered while reading companion records or the
/// index itself.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LessonListing {
    pub entries: Vec<LessonEntry>,
    pub warnings: Vec<String>,
}

/// Read-only listing of lessons and their check records, oldest first.
///
/// Never creates, modifies, or deletes a file, and never follows a symlink.
/// A missing `.kranz`, `.kranz/lessons`, or `index.md` yields an empty
/// listing rather than an error.
pub fn list_lessons(repo_root: &Path) -> Result<LessonListing> {
    let lessons = match open_lessons_dir(repo_root, false) {
        Ok(dir) => dir,
        Err(_) => return Ok(LessonListing::default()),
    };
    let Some(manifest) = read_existing_regular(&lessons, "index.md")? else {
        return Ok(LessonListing::default());
    };

    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut known_mission_ids = std::collections::HashSet::new();

    for (idx, raw_line) in manifest.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((filename, index_summary)) = parse_manifest_line(trimmed) else {
            warnings.push(format!(
                "index.md line {line_no}: unparsable manifest line: {raw_line}"
            ));
            continue;
        };
        let mission_id = filename
            .strip_suffix(".md")
            .unwrap_or(&filename)
            .to_string();
        known_mission_ids.insert(mission_id.clone());

        let file_text = read_existing_regular(&lessons, &filename).ok().flatten();
        let summary = file_text
            .as_deref()
            .and_then(first_nonempty_line)
            .map(str::to_string)
            .unwrap_or(index_summary);

        let check_name = format!("{mission_id}.check");
        let record = match read_check_file(&lessons, &check_name) {
            CheckFileRead::Absent => LessonCheckRecord::default(),
            CheckFileRead::NotRegular => {
                warnings.push(format!(
                    "{check_name}: not a regular file (symlink or other); ignored"
                ));
                LessonCheckRecord::default()
            }
            CheckFileRead::Text(text) => {
                let (record, record_warnings) = parse_check_record(&text);
                for warning in record_warnings {
                    warnings.push(format!("{check_name}: {warning}"));
                }
                record
            }
            CheckFileRead::Error(err) => {
                warnings.push(format!("{check_name}: error reading record: {err}"));
                LessonCheckRecord::default()
            }
        };

        let (classification, source) = record.effective();
        entries.push(LessonEntry {
            lesson: filename,
            mission_id,
            summary,
            classification,
            source,
            link: record.link,
        });
    }

    let mut orphan_check_names = list_check_file_names(&lessons);
    orphan_check_names.sort();
    for name in orphan_check_names {
        let mission_id = name.strip_suffix(".check").unwrap_or(&name).to_string();
        if !known_mission_ids.contains(&mission_id) {
            warnings.push(format!(
                "{name}: orphan check record (no matching index entry)"
            ));
        }
    }

    Ok(LessonListing { entries, warnings })
}

enum CheckFileRead {
    Absent,
    NotRegular,
    Text(String),
    Error(crate::error::EngineError),
}

fn read_check_file(dir: &Dir, name: &str) -> CheckFileRead {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return CheckFileRead::NotRegular,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CheckFileRead::Absent;
        }
        Err(error) => return CheckFileRead::Error(error.into()),
    }
    match read_existing_regular(dir, name) {
        Ok(Some(text)) => CheckFileRead::Text(text),
        Ok(None) => CheckFileRead::Absent,
        Err(error) => CheckFileRead::Error(error),
    }
}

fn list_check_file_names(dir: &Dir) -> Vec<String> {
    let Ok(entries) = dir.entries() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".check") {
            names.push(name);
        }
    }
    names
}

/// Entries whose effective classification is [`CheckClass::Checkable`] and
/// which have no linked check, in index (oldest-first) order.
pub fn needs_check(listing: &LessonListing) -> Vec<&LessonEntry> {
    listing
        .entries
        .iter()
        .filter(|entry| entry.classification == CheckClass::Checkable && entry.link.is_none())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lessons::render_lessons_manifest;
    use std::path::Path;

    fn write_lesson(dir: &Path, id: &str, summary: &str) {
        let lessons_dir = dir.join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        std::fs::write(lessons_dir.join(format!("{id}.md")), format!("{summary}\n")).unwrap();
        let index = lessons_dir.join("index.md");
        let line = format!("- {id}.md · {summary}\n");
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(index)
            .unwrap();
        f.write_all(line.as_bytes()).unwrap();
    }

    fn write_check(dir: &Path, id: &str, contents: &str) {
        let lessons_dir = dir.join(".kranz").join("lessons");
        std::fs::write(lessons_dir.join(format!("{id}.check")), contents).unwrap();
    }

    #[test]
    fn lesson_check_checkable_without_link_needs_check() {
        let dir = tempfile::tempdir().unwrap();
        write_lesson(dir.path(), "m-one", "first lesson");
        write_lesson(dir.path(), "m-two", "second lesson");
        write_check(dir.path(), "m-one", "orchestrator: checkable\n");

        let listing = list_lessons(dir.path()).unwrap();
        let needing = needs_check(&listing);
        assert_eq!(needing.len(), 1, "{listing:?}");
        assert_eq!(needing[0].mission_id, "m-one");
    }

    #[test]
    fn lesson_check_link_removes_from_needs_check() {
        let dir = tempfile::tempdir().unwrap();
        write_lesson(dir.path(), "m-one", "first lesson");
        write_check(
            dir.path(),
            "m-one",
            "orchestrator: checkable\nlink: .kranz/tickets/x.md\n",
        );

        let listing = list_lessons(dir.path()).unwrap();
        assert!(needs_check(&listing).is_empty());
        let entry = &listing.entries[0];
        assert_eq!(entry.classification, CheckClass::Checkable);
        assert_eq!(entry.link.as_deref(), Some(".kranz/tickets/x.md"));
    }

    #[test]
    fn lesson_check_legacy_lessons_list_unclassified() {
        let dir = tempfile::tempdir().unwrap();
        write_lesson(dir.path(), "m-one", "first lesson");
        write_lesson(dir.path(), "m-two", "second lesson");

        let listing = list_lessons(dir.path()).unwrap();
        assert_eq!(listing.entries.len(), 2);
        assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
        for entry in &listing.entries {
            assert_eq!(entry.classification, CheckClass::Unclassified);
            assert_eq!(entry.source, CheckSource::None);
        }
    }

    #[test]
    fn lesson_check_operator_overrides_orchestrator() {
        let dir = tempfile::tempdir().unwrap();
        write_lesson(dir.path(), "m-one", "first lesson");
        write_check(
            dir.path(),
            "m-one",
            "orchestrator: checkable\noperator: not-checkable\n",
        );

        let listing = list_lessons(dir.path()).unwrap();
        let entry = &listing.entries[0];
        assert_eq!(entry.classification, CheckClass::NotCheckable);
        assert_eq!(entry.source, CheckSource::Operator);
        assert!(needs_check(&listing).is_empty());

        let dir2 = tempfile::tempdir().unwrap();
        write_lesson(dir2.path(), "m-two", "second lesson");
        write_check(
            dir2.path(),
            "m-two",
            "orchestrator: not-checkable\noperator: checkable\n",
        );
        let listing2 = list_lessons(dir2.path()).unwrap();
        let entry2 = &listing2.entries[0];
        assert_eq!(entry2.classification, CheckClass::Checkable);
        assert_eq!(entry2.source, CheckSource::Operator);
        assert_eq!(needs_check(&listing2).len(), 1);
    }

    #[test]
    fn lesson_check_render_ignores_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            write_lesson(dir.path(), &format!("m{i}"), &format!("summary {i}"));
        }
        let before = render_lessons_manifest(dir.path(), &|_: &str| true);

        for i in 1..=3 {
            write_check(dir.path(), &format!("m{i}"), "orchestrator: checkable\n");
        }
        let after = render_lessons_manifest(dir.path(), &|_: &str| true);

        assert_eq!(before, after);
    }

    #[test]
    fn lesson_check_listing_performs_no_writes() {
        let dir = tempfile::tempdir().unwrap();
        write_lesson(dir.path(), "m-one", "first lesson");
        write_check(dir.path(), "m-one", "orchestrator: checkable\n");

        fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
            let mut out = Vec::new();
            fn walk(dir: &Path, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
                for entry in std::fs::read_dir(dir).unwrap() {
                    let entry = entry.unwrap();
                    let path = entry.path();
                    if path.is_dir() {
                        walk(&path, out);
                    } else {
                        out.push((path.clone(), std::fs::read(&path).unwrap()));
                    }
                }
            }
            walk(root, &mut out);
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        }

        let before = snapshot(dir.path());
        let _ = list_lessons(dir.path()).unwrap();
        let after = snapshot(dir.path());
        assert_eq!(before, after);
    }

    #[test]
    fn lesson_check_malformed_record_warns() {
        let dir = tempfile::tempdir().unwrap();
        write_lesson(dir.path(), "m-one", "first lesson");
        write_check(
            dir.path(),
            "m-one",
            "orchestrator: maybe\nunknown_key: foo\nno-colon-line\n",
        );

        let listing = list_lessons(dir.path()).unwrap();
        assert_eq!(listing.warnings.len(), 3, "{:?}", listing.warnings);
        let entry = &listing.entries[0];
        assert_eq!(entry.classification, CheckClass::Unclassified);
        assert_eq!(entry.source, CheckSource::None);
    }

    #[test]
    fn lesson_check_unclassified_value_is_unset() {
        let (record, warnings) =
            parse_check_record("orchestrator: checkable\noperator: unclassified\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(record.operator, None);
        assert_eq!(
            record.effective(),
            (CheckClass::Checkable, CheckSource::Orchestrator)
        );

        let dir = tempfile::tempdir().unwrap();
        write_lesson(dir.path(), "m-one", "first lesson");
        write_check(
            dir.path(),
            "m-one",
            "orchestrator: checkable\noperator: unclassified\n",
        );
        let listing = list_lessons(dir.path()).unwrap();
        assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
        let needing = needs_check(&listing);
        assert_eq!(needing.len(), 1, "{listing:?}");
        assert_eq!(needing[0].mission_id, "m-one");

        let (record2, warnings2) = parse_check_record("orchestrator: unclassified\n");
        assert!(warnings2.is_empty(), "{warnings2:?}");
        assert_eq!(
            record2.effective(),
            (CheckClass::Unclassified, CheckSource::None)
        );
    }

    #[test]
    fn lesson_check_orphan_records_warn_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let lessons_dir = dir.path().join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        std::fs::write(lessons_dir.join("index.md"), "").unwrap();
        write_check(dir.path(), "m-zz", "orchestrator: checkable\n");
        write_check(dir.path(), "m-aa", "orchestrator: checkable\n");

        let listing = list_lessons(dir.path()).unwrap();
        let orphan_warnings: Vec<&String> = listing
            .warnings
            .iter()
            .filter(|w| w.contains("orphan"))
            .collect();
        assert_eq!(orphan_warnings.len(), 2, "{:?}", listing.warnings);
        assert!(
            orphan_warnings[0].starts_with("m-aa.check"),
            "{orphan_warnings:?}"
        );
        assert!(
            orphan_warnings[1].starts_with("m-zz.check"),
            "{orphan_warnings:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn lesson_check_sidecar_write_refuses_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let lessons_dir = dir.path().join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "original").unwrap();
        symlink(&target, lessons_dir.join("m-one.check")).unwrap();

        let record = LessonCheckRecord {
            orchestrator: Some(CheckClass::Checkable),
            operator: None,
            link: None,
        };
        let result = write_check_record(dir.path(), "m-one", &record);
        assert!(result.is_err(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original",
            "the symlink's target must be untouched"
        );
    }

    #[test]
    fn lesson_check_record_round_trips() {
        let record = LessonCheckRecord {
            orchestrator: Some(CheckClass::Checkable),
            operator: Some(CheckClass::NotCheckable),
            link: Some(".kranz/tickets/x.md".to_string()),
        };
        let rendered = render_check_record(&record, "m-one");
        let (parsed, warnings) = parse_check_record(&rendered);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(parsed, record);

        let empty = LessonCheckRecord::default();
        let rendered_empty = render_check_record(&empty, "m-two");
        let (parsed_empty, warnings_empty) = parse_check_record(&rendered_empty);
        assert!(warnings_empty.is_empty());
        assert_eq!(parsed_empty, empty);
    }
}
