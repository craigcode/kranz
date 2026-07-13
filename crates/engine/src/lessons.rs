//! Safe storage and rendering of repo-level lessons.
//!
//! Lesson paths live in a worker-writable mission tree but are read and written
//! by the trusted engine, so every filesystem operation must stay beneath the
//! canonical repo-owned `.kranz/lessons` directory and reject symlinks. The
//! renderer reads the append-only manifest (capture order, oldest first) and
//! emits a byte-capped planning-seed block.

use crate::error::{EngineError, Result};
use crate::paths::MissionPaths;
use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Hard cap (bytes) on the rendered lessons-index string.
pub const LESSONS_INJECT_MAX_BYTES: usize = 2048;

/// At most this many of the most recent lessons appear as manifest entries.
const MAX_INDEX_ENTRIES: usize = 10;

/// Of those, at most this many (the newest) get their full body inlined.
const MAX_FULL_BODIES: usize = 3;

const HEADER: &str = "## Lessons from past missions in this repo\n\n";

static LESSON_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Persist one normalized lesson and append its manifest entry without ever
/// following a worker-authored symlink outside the active repository.
pub(crate) fn write_lesson(
    repo_root: &Path,
    mission_id: &str,
    body: &str,
) -> Result<Vec<std::path::PathBuf>> {
    if !MissionPaths::is_safe_id(mission_id) {
        return Err(EngineError::InvalidState(format!(
            "unsafe mission id for lesson capture: {mission_id}"
        )));
    }
    let lessons = open_lessons_dir(repo_root, true)?;
    write_lesson_in_dir(&lessons, mission_id, body)?;

    let lessons_path = repo_root.join(".kranz").join("lessons");
    Ok(vec![
        lessons_path.join(format!("{mission_id}.md")),
        lessons_path.join("index.md"),
    ])
}

fn write_lesson_in_dir(lessons: &Dir, mission_id: &str, body: &str) -> Result<()> {
    let lesson_name = format!("{mission_id}.md");
    ensure_absent_or_regular(lessons, &lesson_name)?;
    let mut manifest = read_existing_regular(lessons, "index.md")?.unwrap_or_default();
    let summary = first_nonempty_line(body).unwrap_or_default();
    manifest.push_str(&format!("- {mission_id}.md · {summary}\n"));

    atomic_replace(lessons, &lesson_name, body.as_bytes())?;
    atomic_replace(lessons, "index.md", manifest.as_bytes())
}

fn open_lessons_dir(repo_root: &Path, create: bool) -> Result<Dir> {
    let repo = Dir::open_ambient_dir(repo_root, ambient_authority())?;
    let kranz_metadata = repo.symlink_metadata(".kranz")?;
    if !kranz_metadata.file_type().is_dir() {
        return Err(unsafe_lessons_path(&repo_root.join(".kranz")));
    }
    let kranz = repo
        .open_dir_nofollow(".kranz")
        .map_err(|_| unsafe_lessons_path(&repo_root.join(".kranz")))?;

    match kranz.symlink_metadata("lessons") {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(unsafe_lessons_path(&repo_root.join(".kranz/lessons"))),
        Err(error) if error.kind() == ErrorKind::NotFound && create => {
            match kranz.create_dir("lessons") {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    kranz
        .open_dir_nofollow("lessons")
        .map_err(|_| unsafe_lessons_path(&repo_root.join(".kranz/lessons")))
}

fn ensure_absent_or_regular(dir: &Dir, name: &str) -> Result<()> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(unsafe_lessons_path(Path::new(name))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_existing_regular(dir: &Dir, name: &str) -> Result<Option<String>> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(unsafe_lessons_path(Path::new(name))),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    match dir.open_with(name, &options) {
        Ok(mut file) => {
            if !file.metadata()?.is_file() {
                return Err(unsafe_lessons_path(Path::new(name)));
            }
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            Ok(Some(text))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn atomic_replace(dir: &Dir, name: &str, bytes: &[u8]) -> Result<()> {
    let tmp = format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        LESSON_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let mut file = dir.open_with(&tmp, &options)?;
        file.write_all(bytes)?;
        drop(file);
        match dir.rename(&tmp, dir, name) {
            Ok(()) => Ok(()),
            #[cfg(windows)]
            Err(_) => {
                ensure_absent_or_regular(dir, name)?;
                match dir.symlink_metadata(name) {
                    Ok(_) => dir.remove_file_or_symlink(name)?,
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                dir.rename(&tmp, dir, name)?;
                Ok(())
            }
            #[cfg(not(windows))]
            Err(error) => Err(error.into()),
        }
    })();
    if result.is_err() {
        let _ = dir.remove_file(&tmp);
    }
    result
}

fn unsafe_lessons_path(path: &Path) -> EngineError {
    EngineError::InvalidState(format!(
        "refusing lesson path outside the repo-owned regular-file tree: {}",
        path.display()
    ))
}

/// Render the byte-capped recent-lessons block for a planning seed: a
/// manifest (id + one-line summary) of the most recent provenance-clean
/// lessons, plus the full body of the newest [`MAX_FULL_BODIES`] of them.
/// `None` when no provenance-clean lesson survives.
///
/// `is_provenance_clean` is called with each lesson's filename (`<id>.md`);
/// the caller supplies the git-history check (was this file added by a
/// `[kranz] mission report` commit for that mission?) so this module stays
/// filesystem-only and unit-testable. That filter is what stops a
/// worker-dropped or otherwise arbitrary file in `.kranz/lessons/` from
/// injecting text into a future planner. Bodies are recency-selected (a
/// mission has no touch_set at planning time to rank relevance against).
pub fn render_lessons_manifest(
    repo_root: &Path,
    is_provenance_clean: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let lessons = open_lessons_dir(repo_root, false).ok()?;
    render_lessons_manifest_in_dir(&lessons, is_provenance_clean)
}

fn render_lessons_manifest_in_dir(
    lessons: &Dir,
    is_provenance_clean: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let manifest = read_existing_regular(lessons, "index.md").ok()??;

    let lines: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }

    struct Entry {
        filename: String,
        first_line: String,
        body: Option<String>,
    }

    // Manifest is append-only, oldest first; collect the most recent
    // provenance-clean lessons, newest first, up to the index cap. The body
    // is read for the newest few (recency-selected, since a mission has no
    // touch_set at planning time to rank relevance against).
    let mut entries: Vec<Entry> = Vec::new();
    for line in lines.iter().rev() {
        if entries.len() >= MAX_INDEX_ENTRIES {
            break;
        }
        let Some((filename, summary)) = parse_manifest_line(line) else {
            continue;
        };
        if !is_provenance_clean(&filename) {
            continue;
        }
        let file_text = read_existing_regular(lessons, &filename).ok().flatten();
        // Prefer the actual (provenance-checked) file's first line over the
        // manifest summary, which a worker could have rewritten.
        let first_line = file_text
            .as_deref()
            .and_then(first_nonempty_line)
            .map(str::to_string)
            .unwrap_or(summary);
        let body = if entries.len() < MAX_FULL_BODIES {
            file_text
        } else {
            None
        };
        entries.push(Entry {
            filename,
            first_line,
            body,
        });
    }
    if entries.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(LESSONS_INJECT_MAX_BYTES);
    out.push_str(HEADER);

    // Manifest lines are highest priority: newest-first, stopping (and
    // truncating the last) the moment the cap would be exceeded.
    for entry in &entries {
        let remaining = LESSONS_INJECT_MAX_BYTES.saturating_sub(out.len());
        if remaining == 0 {
            break;
        }
        let line = format!("- {} — {}\n", entry.filename, entry.first_line);
        if line.len() <= remaining {
            out.push_str(&line);
        } else {
            out.push_str(&truncate_to_bytes(&line, remaining));
            break;
        }
    }

    // Full bodies for the newest few clean lessons: lower priority than the
    // manifest, so they are dropped/truncated first when space is tight.
    for entry in entries.iter().filter(|e| e.body.is_some()) {
        let remaining = LESSONS_INJECT_MAX_BYTES.saturating_sub(out.len());
        if remaining == 0 {
            break;
        }
        let body = entry.body.as_deref().unwrap_or_default();
        let chunk = format!("\n### {}\n{}\n", entry.filename, body.trim_end());
        if chunk.len() <= remaining {
            out.push_str(&chunk);
        } else {
            out.push_str(&truncate_to_bytes(&chunk, remaining));
            break;
        }
    }

    debug_assert!(out.len() <= LESSONS_INJECT_MAX_BYTES);
    Some(out)
}

/// Split a manifest line of the form `- <filename> · <summary>` into its
/// filename and summary parts. Only a single `.md` basename is accepted:
/// manifest contents are repository-controlled and must never escape the
/// lessons directory when joined.
fn parse_manifest_line(line: &str) -> Option<(String, String)> {
    let line = line.trim_start_matches('-').trim();
    let (filename, summary) = match line.split_once('·') {
        Some((filename, summary)) => (filename.trim(), summary.trim()),
        None => (line, ""),
    };
    if filename.is_empty()
        || !filename.ends_with(".md")
        || filename.contains(['/', '\\'])
        || Path::new(filename)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(filename)
    {
        return None;
    }
    Some((filename.to_string(), summary.to_string()))
}

fn first_nonempty_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| !l.is_empty())
}

/// Truncate `s` to at most `limit` bytes, cutting on a UTF-8 char boundary.
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

    fn write_lesson(dir: &Path, id: &str, first_line: &str, rest: &str) {
        let lessons_dir = dir.join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        let body = if rest.is_empty() {
            format!("{first_line}\n")
        } else {
            format!("{first_line}\n{rest}\n")
        };
        std::fs::write(lessons_dir.join(format!("{id}.md")), body).unwrap();
        let index = lessons_dir.join("index.md");
        let line = format!("- {id}.md · {first_line}\n");
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(index)
            .unwrap();
        f.write_all(line.as_bytes()).unwrap();
    }

    #[test]
    fn returns_none_when_index_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(render_lessons_manifest(dir.path(), &|_: &str| true).is_none());
    }

    #[test]
    fn returns_none_when_index_empty() {
        let dir = tempfile::tempdir().unwrap();
        let lessons_dir = dir.path().join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        std::fs::write(lessons_dir.join("index.md"), "").unwrap();
        assert!(render_lessons_manifest(dir.path(), &|_: &str| true).is_none());
    }

    #[test]
    fn ignores_manifest_paths_outside_the_lessons_directory() {
        let dir = tempfile::tempdir().unwrap();
        let lessons_dir = dir.path().join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "LOCAL SECRET\n").unwrap();
        std::fs::write(lessons_dir.join("safe.md"), "SAFE LESSON\n").unwrap();
        std::fs::write(
            lessons_dir.join("index.md"),
            format!(
                "- ../../outside.md · traversal\n- {} · absolute\n- safe.md · safe\n",
                outside.display()
            ),
        )
        .unwrap();

        let rendered =
            render_lessons_manifest(dir.path(), &|_: &str| true).expect("safe lesson remains");
        assert!(rendered.contains("SAFE LESSON"), "{rendered}");
        assert!(!rendered.contains("LOCAL SECRET"), "{rendered}");
        assert!(!rendered.contains("../../outside.md"), "{rendered}");
        assert!(
            !rendered.contains(&outside.display().to_string()),
            "{rendered}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinked_lesson_files() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let lessons_dir = dir.path().join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "LOCAL SECRET\n").unwrap();
        symlink(&outside, lessons_dir.join("linked.md")).unwrap();
        std::fs::write(lessons_dir.join("index.md"), "- linked.md · fallback\n").unwrap();

        let rendered =
            render_lessons_manifest(dir.path(), &|_: &str| true).expect("summary remains safe");
        assert!(rendered.contains("fallback"), "{rendered}");
        assert!(!rendered.contains("LOCAL SECRET"), "{rendered}");
    }

    #[cfg(unix)]
    #[test]
    fn ignores_a_symlinked_manifest() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let lessons_dir = dir.path().join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        let outside = dir.path().join("outside-index.md");
        std::fs::write(&outside, "- safe.md · EXTERNAL MANIFEST TEXT\n").unwrap();
        std::fs::write(lessons_dir.join("safe.md"), "SAFE LESSON\n").unwrap();
        symlink(&outside, lessons_dir.join("index.md")).unwrap();

        assert!(render_lessons_manifest(dir.path(), &|_: &str| true).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn ignores_a_symlinked_lessons_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("index.md"), "- linked.md · fallback\n").unwrap();
        std::fs::write(outside.path().join("linked.md"), "LOCAL SECRET\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".kranz")).unwrap();
        symlink(outside.path(), dir.path().join(".kranz/lessons")).unwrap();

        assert!(render_lessons_manifest(dir.path(), &|_: &str| true).is_none());
    }

    #[test]
    fn lesson_write_replaces_the_existing_manifest_without_temp_residue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".kranz")).unwrap();

        super::write_lesson(dir.path(), "m-one", "FIRST LESSON\n").unwrap();
        super::write_lesson(dir.path(), "m-two", "SECOND LESSON\n").unwrap();

        let lessons_dir = dir.path().join(".kranz/lessons");
        assert_eq!(
            std::fs::read_to_string(lessons_dir.join("index.md")).unwrap(),
            "- m-one.md · FIRST LESSON\n- m-two.md · SECOND LESSON\n"
        );
        assert_eq!(
            std::fs::read_to_string(lessons_dir.join("m-two.md")).unwrap(),
            "SECOND LESSON\n"
        );
        assert!(
            std::fs::read_dir(lessons_dir).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
            "atomic replacement must clean temporary files"
        );
    }

    #[cfg(unix)]
    #[test]
    fn lesson_write_refuses_symlinked_destinations_without_touching_targets() {
        use std::os::unix::fs::symlink;

        for destination in ["m-safe.md", "index.md"] {
            let dir = tempfile::tempdir().unwrap();
            let lessons_dir = dir.path().join(".kranz").join("lessons");
            std::fs::create_dir_all(&lessons_dir).unwrap();
            let outside = dir.path().join("outside.md");
            std::fs::write(&outside, "DO NOT CHANGE\n").unwrap();
            symlink(&outside, lessons_dir.join(destination)).unwrap();

            let error = super::write_lesson(dir.path(), "m-safe", "SAFE LESSON").unwrap_err();

            assert!(
                error.to_string().contains("refusing lesson path"),
                "{error}"
            );
            assert_eq!(std::fs::read_to_string(outside).unwrap(), "DO NOT CHANGE\n");
        }
    }

    #[cfg(unix)]
    #[test]
    fn lesson_write_refuses_a_symlinked_lessons_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".kranz")).unwrap();
        symlink(outside.path(), dir.path().join(".kranz/lessons")).unwrap();

        let error = super::write_lesson(dir.path(), "m-safe", "SAFE LESSON").unwrap_err();

        assert!(
            error.to_string().contains("refusing lesson path"),
            "{error}"
        );
        assert!(!outside.path().join("m-safe.md").exists());
        assert!(!outside.path().join("index.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn lesson_write_stays_bound_to_the_open_directory_after_path_swap() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let lessons_path = dir.path().join(".kranz/lessons");
        std::fs::create_dir_all(&lessons_path).unwrap();
        let lessons = open_lessons_dir(dir.path(), false).unwrap();
        let held_path = dir.path().join(".kranz/lessons-held");
        std::fs::rename(&lessons_path, &held_path).unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), &lessons_path).unwrap();

        write_lesson_in_dir(&lessons, "m-safe", "SAFE LESSON").unwrap();

        assert_eq!(
            std::fs::read_to_string(held_path.join("m-safe.md")).unwrap(),
            "SAFE LESSON"
        );
        assert!(held_path.join("index.md").is_file());
        assert!(!outside.path().join("m-safe.md").exists());
        assert!(!outside.path().join("index.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn lesson_render_stays_bound_to_the_open_directory_after_path_swap() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let lessons_path = dir.path().join(".kranz/lessons");
        std::fs::create_dir_all(&lessons_path).unwrap();
        std::fs::write(lessons_path.join("index.md"), "- safe.md · SAFE SUMMARY\n").unwrap();
        std::fs::write(lessons_path.join("safe.md"), "SAFE LESSON\n").unwrap();
        let lessons = open_lessons_dir(dir.path(), false).unwrap();
        let held_path = dir.path().join(".kranz/lessons-held");
        std::fs::rename(&lessons_path, &held_path).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("index.md"),
            "- evil.md · EXTERNAL SUMMARY\n",
        )
        .unwrap();
        std::fs::write(outside.path().join("evil.md"), "LOCAL SECRET\n").unwrap();
        symlink(outside.path(), &lessons_path).unwrap();

        let rendered = render_lessons_manifest_in_dir(&lessons, &|_: &str| true).unwrap();

        assert!(rendered.contains("SAFE LESSON"), "{rendered}");
        assert!(!rendered.contains("LOCAL SECRET"), "{rendered}");
        assert!(!rendered.contains("EXTERNAL SUMMARY"), "{rendered}");
    }

    #[test]
    fn lists_up_to_ten_newest_first_with_three_clean_bodies() {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=13 {
            write_lesson(
                dir.path(),
                &format!("m{i:02}"),
                &format!("SUMMARY-{i:02}-END"),
                &format!("DETAIL-{i:02}-END"),
            );
        }

        let rendered =
            render_lessons_manifest(dir.path(), &|_: &str| true).expect("lessons present");
        assert!(rendered.starts_with("## Lessons from past missions in this repo"));

        // Newest-first, at most the 10 most recent as manifest entries.
        let pos_m13 = rendered.find("m13.md").expect("m13 listed");
        let pos_m12 = rendered.find("m12.md").expect("m12 listed");
        assert!(pos_m13 < pos_m12, "newest lesson must appear first");
        assert!(
            !rendered.contains("m03.md"),
            "only the 10 most recent listed"
        );
        assert!(
            rendered.contains("m04.md"),
            "the 10th most recent still listed"
        );
        for i in 1..=13 {
            assert_eq!(
                rendered.contains(&format!("SUMMARY-{i:02}-END")),
                i >= 4,
                "manifest summary present only for the 10 most recent (mission {i})"
            );
        }

        // Full bodies only for the 3 newest (recency-selected), never beyond.
        for i in [13, 12, 11] {
            assert!(
                rendered.contains(&format!("DETAIL-{i:02}-END")),
                "full body expected for the newest lessons (mission {i})"
            );
        }
        for i in [10, 9, 4] {
            assert!(
                !rendered.contains(&format!("DETAIL-{i:02}-END")),
                "no body beyond the 3 newest (mission {i})"
            );
        }
    }

    #[test]
    fn body_selection_skips_lessons_the_provenance_predicate_rejects() {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=5 {
            write_lesson(
                dir.path(),
                &format!("m{i:02}"),
                &format!("SUMMARY-{i:02}"),
                &format!("DETAIL-{i:02}-END"),
            );
        }
        // Reject the two newest — their bodies must not slip in, and the body
        // budget applies to the CLEAN lessons only (m03, m02, m01).
        let rejected = ["m05.md", "m04.md"];
        let rendered = render_lessons_manifest(dir.path(), &|f: &str| !rejected.contains(&f))
            .expect("clean lessons remain");

        assert!(!rendered.contains("m05.md") && !rendered.contains("DETAIL-05-END"));
        assert!(!rendered.contains("m04.md") && !rendered.contains("DETAIL-04-END"));
        for i in [3, 2, 1] {
            assert!(
                rendered.contains(&format!("DETAIL-{i:02}-END")),
                "clean lesson body expected (mission {i})"
            );
        }
    }

    #[test]
    fn provenance_predicate_filters_out_unclean_lessons() {
        let dir = tempfile::tempdir().unwrap();
        write_lesson(dir.path(), "m-clean", "CLEAN SUMMARY", "clean detail");
        write_lesson(dir.path(), "m-forged", "FORGED SUMMARY", "forged detail");

        // Predicate accepts only the clean lesson (as the git-history check
        // would for a genuine `[kranz] mission report` commit).
        let rendered =
            render_lessons_manifest(dir.path(), &|filename: &str| filename == "m-clean.md")
                .expect("the clean lesson survives");

        assert!(rendered.contains("m-clean.md"), "{rendered}");
        assert!(rendered.contains("CLEAN SUMMARY"), "{rendered}");
        assert!(
            !rendered.contains("m-forged.md") && !rendered.contains("FORGED SUMMARY"),
            "a lesson the predicate rejects must never reach the prompt: {rendered}"
        );

        // When the predicate rejects everything, nothing is injected (not even
        // a bare header).
        assert!(render_lessons_manifest(dir.path(), &|_: &str| false).is_none());
    }

    #[test]
    fn hard_byte_cap_holds_with_many_huge_first_lines() {
        let dir = tempfile::tempdir().unwrap();
        let huge_line = "y".repeat(5_000);
        for i in 1..=15 {
            write_lesson(dir.path(), &format!("m{i:02}"), &huge_line, "");
        }

        let rendered =
            render_lessons_manifest(dir.path(), &|_: &str| true).expect("lessons present");
        assert!(rendered.len() <= LESSONS_INJECT_MAX_BYTES);
    }

    #[test]
    fn render_performs_no_filesystem_writes() {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=5 {
            write_lesson(
                dir.path(),
                &format!("m{i:02}"),
                &format!("summary {i}"),
                "detail",
            );
        }
        let lessons_dir = dir.path().join(".kranz").join("lessons");

        let before: Vec<_> = std::fs::read_dir(&lessons_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let index_before = std::fs::read_to_string(lessons_dir.join("index.md")).unwrap();

        let _ = render_lessons_manifest(dir.path(), &|_: &str| true);

        let mut after: Vec<_> = std::fs::read_dir(&lessons_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let mut before_sorted = before.clone();
        before_sorted.sort();
        after.sort();
        assert_eq!(before_sorted, after, "no files created or removed");
        let index_after = std::fs::read_to_string(lessons_dir.join("index.md")).unwrap();
        assert_eq!(index_before, index_after, "manifest must not be modified");
    }
}
