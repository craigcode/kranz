//! Import only declared, bounded regular files through pinned no-follow
//! handles, then scrub UTF-8 bytes. Binary outputs are unavailable in v1.
use super::protocol::{ArtifactRef, Digest, Limits};
use cap_fs_ext::{DirExt, OpenOptionsFollowExt};
use cap_primitives::fs::FollowSymlinks;
use cap_std::fs::{Dir, OpenOptions};
use serde::Serialize;
use std::io::Read;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedArtifact {
    pub source: ArtifactRef,
    pub retained_digest: Digest,
    pub transformation: String,
    pub retained_bytes: Vec<u8>,
}

pub fn import(
    root: &Dir,
    artifacts: &[ArtifactRef],
    limits: &Limits,
) -> Result<Vec<ImportedArtifact>, String> {
    if artifacts.len() as u64 > limits.max_artifacts {
        return Err("output artifact count exceeded".into());
    }
    let mut total = 0u64;
    let mut imported = Vec::new();
    for artifact in artifacts {
        total = total
            .checked_add(artifact.bytes)
            .ok_or("artifact byte overflow")?;
        if total > limits.max_artifact_bytes {
            return Err("output artifact bytes exceeded".into());
        }
        let mut parent = root.try_clone().map_err(|e| e.to_string())?;
        let mut components = artifact.path.as_str().split('/').peekable();
        let name = loop {
            let part = components.next().ok_or("empty output path")?;
            if components.peek().is_none() {
                break part;
            }
            parent = parent
                .open_dir_nofollow(part)
                .map_err(|_| "output parent is missing or not a real directory")?;
        };
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_fs_ext::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let mut file = parent
            .open_with(name, &options)
            .map_err(|_| "output is missing or cannot be opened without following links")?
            .into_std();
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() != artifact.bytes {
            return Err("output is not a regular file with the declared size".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err("hard-linked output is not accepted".into());
            }
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(artifact.bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 != artifact.bytes || Digest::of(&bytes) != artifact.digest {
            return Err("output digest or opened-file size mismatch".into());
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| "binary output redaction is unavailable; return UTF-8 evidence")?;
        let retained_bytes = crate::scrub::scrub(text).into_bytes();
        imported.push(ImportedArtifact {
            source: artifact.clone(),
            retained_digest: Digest::of(&retained_bytes),
            transformation: transformation(),
            retained_bytes,
        });
    }
    Ok(imported)
}

/// Names the actual scrub implementation, not just a mutable product version.
pub(crate) fn transformation() -> String {
    format!(
        "utf8;kranz-scrub-source={}",
        Digest::of(include_bytes!("../scrub.rs")).as_str()
    )
}
