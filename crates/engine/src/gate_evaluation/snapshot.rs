//! A source-only snapshot for independent evaluators. No checkout, shared Git
//! metadata, worker transcript, provider state or authority file is mounted.
use super::evidence::{SnapshotEntry, SnapshotInventory};
use super::protocol::{Digest, Id, WirePath};
use crate::git_ops::GitRepo;
use cap_fs_ext::{DirExt, OpenOptionsFollowExt};
use cap_primitives::fs::FollowSymlinks;
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;

pub const MAX_SOURCE_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_SOURCE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Conventional private/runtime paths are never source inputs. The snapshot
/// binds this selection and lists excluded paths; stage policy must retain
/// that coverage limitation. Selected mission scope/criteria/receipts travel
/// separately, never by mounting the mission directory.
pub const EXCLUDED_PREFIXES: &[&str] = &[
    ".git",
    ".claude",
    ".codex",
    ".ssh",
    ".aws",
    ".azure",
    ".config",
    ".git-credentials",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".kranz/missions",
    ".kranz/control",
    ".kranz/runs",
    ".kranz/queue",
    ".kranz/hook-status",
    ".kranz/config.json",
    ".kranz/serve.token",
    ".kranz/serve.read.token",
    ".kranz/domain-terms.local",
];

pub fn excluded(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.split('/')
        .any(|part| part == ".env" || part.starts_with(".env."))
        || EXCLUDED_PREFIXES.iter().any(|prefix| {
            path == *prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('/'))
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub head: String,
    pub base: String,
    pub inventory_digest: Digest,
    pub exclusions_digest: Digest,
}

#[derive(Debug)]
pub struct SourceSnapshot {
    pub identity: Identity,
    pub inventory: Vec<u8>,
    /// Exact selection bytes for the future stage input builder to retain.
    pub selection: Vec<u8>,
    /// Source artifact IDs are derived from their complete relative path. The
    /// inventory is the authoritative mapping back to source labels and modes.
    pub files: BTreeMap<Id, Vec<u8>>,
    pub excluded_paths: Vec<String>,
}

impl SourceSnapshot {
    pub fn capture(repo: &GitRepo, base: &str) -> Result<Self, String> {
        let repo = repo.with_hooks_disabled().map_err(|e| e.to_string())?;
        let base = repo.rev_parse(base).map_err(|e| e.to_string())?;
        let head = repo.head_sha().map_err(|e| e.to_string())?;
        let root = Dir::open_ambient_dir(repo.root(), cap_std::ambient_authority())
            .map_err(|e| e.to_string())?;
        let paths = repo.gate_snapshot_paths(&base).map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        let mut files = BTreeMap::new();
        let mut total = 0usize;
        let mut excluded_paths = Vec::new();
        for path in paths {
            if excluded(&path) {
                excluded_paths.push(path);
                continue;
            }
            let path = WirePath::try_from(path)?;
            let Some((bytes, executable)) = read_source(&root, &path)? else {
                entries.push(SnapshotEntry::Deleted { path });
                continue;
            };
            total = total
                .checked_add(bytes.len())
                .ok_or("source size overflow")?;
            if total > MAX_SOURCE_BYTES {
                return Err("gate source snapshot exceeds byte limit".into());
            }
            let label_digest = Digest::of(path.as_str().as_bytes());
            let artifact_id = Id::try_from(format!("source-{}", &label_digest.as_str()[7..]))?;
            entries.push(SnapshotEntry::File {
                path,
                artifact_id: artifact_id.clone(),
                digest: Digest::of(&bytes),
                executable,
            });
            files.insert(artifact_id, bytes);
        }
        super::protocol::validate_paths(entries.iter().map(|entry| match entry {
            SnapshotEntry::File { path, .. } | SnapshotEntry::Deleted { path } => path.as_str(),
        }))?;
        if repo.head_sha().map_err(|e| e.to_string())? != head {
            return Err("candidate HEAD moved during gate snapshot capture".into());
        }
        let inventory = serde_json::to_vec(&SnapshotInventory {
            schema_version: 1,
            entries,
        })
        .map_err(|e| e.to_string())?;
        let selection = serde_json::to_vec(&serde_json::json!({
            "prefixes": EXCLUDED_PREFIXES,
            "environmentFiles": ".env and .env.* at any depth, ASCII case-insensitive",
            "ignoredFiles": "Git standard excludes at capture; ignored caches are not evidence",
            "excludedPaths": excluded_paths,
        }))
        .map_err(|e| e.to_string())?;
        Ok(Self {
            identity: Identity {
                head,
                base,
                inventory_digest: Digest::of(&inventory),
                exclusions_digest: Digest::of(&selection),
            },
            inventory,
            selection,
            files,
            excluded_paths,
        })
    }

    pub fn verify_current(&self, repo: &GitRepo) -> Result<(), String> {
        let current = Self::capture(repo, &self.identity.base)?;
        if current.identity != self.identity {
            return Err("candidate bytes changed after gate evidence was frozen".into());
        }
        Ok(())
    }
}

fn read_source(root: &Dir, path: &WirePath) -> Result<Option<(Vec<u8>, bool)>, String> {
    let mut parent = root.try_clone().map_err(|e| e.to_string())?;
    let mut components = path.as_str().split('/').peekable();
    let name = loop {
        let part = components.next().ok_or("empty source path")?;
        if components.peek().is_none() {
            break part;
        }
        parent = match parent.open_dir_nofollow(part) {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(format!(
                    "source parent is not a real directory: {}",
                    path.as_str()
                ))
            }
        };
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match parent.open_with(name, &options) {
        Ok(file) => file.into_std(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(format!(
                "source cannot be opened without following links: {}",
                path.as_str()
            ))
        }
    };
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.len() > MAX_SOURCE_FILE_BYTES {
        return Err(format!(
            "source requires a bounded regular file (submodules are unsupported): {}",
            path.as_str()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.nlink() != 1 {
            return Err(format!(
                "hard-linked source is unsupported: {}",
                path.as_str()
            ));
        }
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_SOURCE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    if bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
        || after.permissions() != before.permissions()
    {
        return Err(format!("source changed during capture: {}", path.as_str()));
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        before.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    Ok(Some((bytes, executable)))
}
