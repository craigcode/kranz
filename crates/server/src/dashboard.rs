//! Explicit operator-selected dashboard directories. Pin the root once, then
//! open every component relative to directory handles without following links.
//! Canonicalize-then-ServeDir would leave a check/open race on every request.

use axum::body::Body;
use axum::http::{header, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions};
use std::io::{self, Read as _};
use std::path::Path;
use std::sync::Arc;

const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct DashboardDir(Option<Arc<Dir>>);

impl DashboardDir {
    pub(crate) fn open(path: &Path) -> Self {
        match Dir::open_ambient_dir(path, cap_std::ambient_authority()) {
            Ok(dir) => Self(Some(Arc::new(dir))),
            Err(error) => {
                tracing::error!(%error, "cannot open the selected dashboard directory");
                Self(None)
            }
        }
    }

    pub(crate) async fn respond(&self, method: Method, uri: Uri) -> Response {
        if method != Method::GET && method != Method::HEAD {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let Some(root) = self.0.clone() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let Ok(path) = percent_encoding::percent_decode_str(uri.path()).decode_utf8() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let path = path.strip_prefix('/').unwrap_or(&path);
        // Reject alternate separators, dot components, Windows drive/ADS names,
        // and embedded NULs before any filesystem operation on any platform.
        if path.contains(['\\', ':', '\0'])
            || path.split('/').any(|part| part == "." || part == "..")
        {
            return StatusCode::NOT_FOUND.into_response();
        }
        let path = if path.is_empty() { "index.html" } else { path }.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            match read_asset(&root, &path) {
                Ok(bytes) => Ok((path, bytes)),
                // SPA navigation falls back only for missing paths. A refused
                // symlink, non-regular file, or permission error stays refused.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    read_asset(&root, "index.html").map(|bytes| ("index.html".into(), bytes))
                }
                Err(error) => Err(error),
            }
        })
        .await;
        match result {
            Ok(Ok((path, bytes))) => Response::builder()
                .header(
                    header::CONTENT_TYPE,
                    mime_guess::from_path(path).first_or_octet_stream().as_ref(),
                )
                .header(header::CONTENT_LENGTH, bytes.len())
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                .body(if method == Method::HEAD {
                    Body::empty()
                } else {
                    Body::from(bytes)
                })
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
            Ok(Err(_)) => StatusCode::NOT_FOUND.into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

fn read_asset(root: &Dir, path: &str) -> io::Result<Vec<u8>> {
    let mut directory = root.try_clone()?;
    let mut components = path.split('/').peekable();
    while let Some(name) = components.next() {
        if name.is_empty() {
            return Err(io::ErrorKind::NotFound.into());
        }
        if components.peek().is_some() {
            directory = directory.open_dir_nofollow(name)?;
        } else {
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            #[cfg(unix)]
            {
                use cap_fs_ext::OpenOptionsSyncExt as _;
                options.nonblock(true);
            }
            let file = directory.open_with(name, &options)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.len() > MAX_ASSET_BYTES {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let mut bytes = Vec::new();
            file.take(MAX_ASSET_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_ASSET_BYTES {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            return Ok(bytes);
        }
    }
    Err(io::ErrorKind::NotFound.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;

    async fn get(dashboard: &DashboardDir, path: &str) -> (StatusCode, Vec<u8>) {
        let response = dashboard.respond(Method::GET, path.parse().unwrap()).await;
        let status = response.status();
        (
            status,
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
    }

    #[tokio::test]
    async fn serves_assets_and_spa_navigation_but_rejects_traversal() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "dashboard").unwrap();
        std::fs::write(root.path().join("app.js"), "javascript").unwrap();
        let dashboard = DashboardDir::open(root.path());
        assert_eq!(
            get(&dashboard, "/app.js").await,
            (StatusCode::OK, b"javascript".to_vec())
        );
        assert_eq!(
            get(&dashboard, "/missions/example").await,
            (StatusCode::OK, b"dashboard".to_vec())
        );
        for path in [
            "/../outside",
            "/%2e%2e/outside",
            "/%5coutside",
            "/file%3asecret",
            "/%00",
        ] {
            assert_eq!(
                get(&dashboard, path).await.0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        let head = dashboard
            .respond(Method::HEAD, "/app.js".parse().unwrap())
            .await;
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "10");
        assert!(head
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_leaf_directory_and_fallback_symlinks_and_keeps_pinned_root() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("dist");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("marker"), "outside-marker").unwrap();
        std::fs::write(root.join("index.html"), "dashboard").unwrap();
        symlink(outside.join("marker"), root.join("leaf")).unwrap();
        symlink(&outside, root.join("directory")).unwrap();
        let dashboard = DashboardDir::open(&root);
        for path in ["/leaf", "/directory/marker"] {
            assert_eq!(
                get(&dashboard, path).await.0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        std::fs::remove_file(root.join("index.html")).unwrap();
        symlink(outside.join("marker"), root.join("index.html")).unwrap();
        assert_eq!(
            get(&dashboard, "/missing/spa-route").await.0,
            StatusCode::NOT_FOUND
        );
        std::fs::remove_file(root.join("index.html")).unwrap();
        std::fs::write(root.join("index.html"), "pinned-root").unwrap();
        std::fs::rename(&root, temp.path().join("original-dist")).unwrap();
        symlink(&outside, &root).unwrap();
        assert_eq!(
            get(&dashboard, "/").await,
            (StatusCode::OK, b"pinned-root".to_vec())
        );
    }
}
