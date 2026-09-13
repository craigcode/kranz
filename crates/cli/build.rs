use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=KRANZ_EMBED_DASHBOARD_DIST");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let candidates = dashboard_candidates(&manifest_dir);
    let dashboard_dir = candidates
        .iter()
        .find(|dir| {
            println!("cargo:rerun-if-changed={}", dir.display());
            dir.join("index.html").is_file()
        })
        .unwrap_or_else(|| {
            panic!(
                "dashboard dist not found; refusing to build a UI-less kranz binary. Looked for \
                 index.html in: {}",
                candidates
                    .iter()
                    .map(|dir| dir.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });

    let out_path =
        PathBuf::from(env::var_os("OUT_DIR").expect("out dir")).join("embedded_dashboard.rs");
    let mut out = fs::File::create(out_path).expect("create embedded dashboard module");

    let files = dashboard_files(dashboard_dir).expect("read dashboard files");
    writeln!(
        out,
        "pub const EMBEDDED_DASHBOARD_SOURCE: &str = {:?};",
        dashboard_dir.display().to_string()
    )
    .expect("write embedded dashboard source");
    writeln!(
        out,
        "pub const EMBEDDED_DASHBOARD: &[kranz_server::EmbeddedFile] = &["
    )
    .expect("write embedded dashboard header");
    for file in files {
        println!("cargo:rerun-if-changed={}", file.display());
        let rel = relative_slash_path(dashboard_dir, &file);
        writeln!(
            out,
            "    kranz_server::EmbeddedFile {{ path: {:?}, bytes: include_bytes!({:?}), content_type: {:?} }},",
            rel,
            file.display().to_string(),
            content_type(&file),
        )
        .expect("write embedded dashboard file");
    }
    writeln!(out, "];").expect("write embedded dashboard footer");
}

fn dashboard_candidates(manifest_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = env::var_os("KRANZ_EMBED_DASHBOARD_DIST") {
        candidates.push(PathBuf::from(dir));
    }
    candidates.push(manifest_dir.join("assets").join("dashboard").join("dist"));
    candidates.push(
        manifest_dir
            .join("..")
            .join("..")
            .join("apps")
            .join("dashboard")
            .join("dist"),
    );
    candidates
}

fn dashboard_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn relative_slash_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .expect("dashboard file under root")
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}
