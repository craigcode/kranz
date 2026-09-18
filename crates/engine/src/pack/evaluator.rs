//! Explicit schema-5 registration. The approved Git object and OCI digest,
//! not a worker-controlled directory, supply every checker/runtime byte.
use crate::gate_evaluation::protocol::{ArtifactRole, Digest, Id, Stage, WirePath};
use crate::git_ops::GitRepo;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Mechanical,
    Judgment,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforcement {
    Advisory,
    Blocking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Declaration {
    pub name: Id,
    pub image: String,
    pub executable: String,
    pub args: Vec<String>,
    pub files: Vec<WirePath>,
    pub stages: Vec<Stage>,
    pub evidence: Vec<ArtifactRole>,
    pub kind: Kind,
    pub enforcement: Enforcement,
}

pub(super) fn declarations(
    doc: &super::toml::Document,
    schema: u32,
) -> Result<Vec<Declaration>, String> {
    if doc.single("evaluator").is_some() {
        return Err("evaluator must use [[evaluator]] array syntax".into());
    }
    let tables = doc.array("evaluator");
    if !tables.is_empty() && schema != super::SCHEMA_EVALUATORS {
        return Err("[[evaluator]] requires pack schema 5".into());
    }
    let mut declarations = Vec::new();
    for (i, table) in tables.iter().enumerate() {
        let label = super::entry_label("evaluator", i, table);
        super::check_unknown(
            table,
            &label,
            &[
                "name",
                "image",
                "executable",
                "args",
                "files",
                "stages",
                "evidence",
                "kind",
                "enforcement",
            ],
        )?;
        let string = |key: &str| super::required_string(table, &label, key);
        let array = |key: &str| super::optional_string_array(table, &label, key);
        let image = string("image")?;
        let (repository, digest) = image
            .split_once("@sha256:")
            .ok_or("evaluator image requires an OCI sha256 digest pin")?;
        if repository.is_empty()
            || repository.starts_with('-')
            || !repository
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._:-".contains(&b))
        {
            return Err("invalid evaluator image repository".into());
        }
        Digest::try_from(format!("sha256:{digest}"))?;
        let executable = string("executable")?;
        WirePath::try_from(
            executable
                .strip_prefix('/')
                .ok_or("evaluator executable must be absolute inside the image")?
                .to_owned(),
        )?;
        let args = array("args")?;
        if args.len() > 128 || args.iter().any(|s| s.len() > 8192 || s.contains('\0')) {
            return Err("evaluator argv exceeds limits or contains NUL".into());
        }
        let files = array("files")?
            .into_iter()
            .map(WirePath::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let parse = |key: &str| -> Result<Vec<serde_json::Value>, String> {
            Ok(array(key)?
                .into_iter()
                .map(serde_json::Value::String)
                .collect())
        };
        let stages: Vec<Stage> = serde_json::from_value(serde_json::Value::Array(parse("stages")?))
            .map_err(|_| "unsupported evaluator stage")?;
        let evidence: Vec<ArtifactRole> =
            serde_json::from_value(serde_json::Value::Array(parse("evidence")?))
                .map_err(|_| "unsupported evaluator evidence role")?;
        let kind = serde_json::from_value(serde_json::Value::String(string("kind")?))
            .map_err(|_| "evaluator kind must be mechanical or judgment")?;
        let enforcement = serde_json::from_value(serde_json::Value::String(string("enforcement")?))
            .map_err(|_| "evaluator enforcement must be advisory or blocking")?;
        let name = Id::try_from(string("name")?)?;
        if super::RESERVED_GATE_NAMES.contains(&name.as_str()) {
            return Err("evaluator name is reserved by the engine".into());
        }
        if files.is_empty()
            || files.len() > 128
            || stages.is_empty()
            || stages.len() > 5
            || evidence.len() > 16
        {
            return Err("evaluator requires bounded files and supported stages".into());
        }
        crate::gate_evaluation::protocol::validate_paths(files.iter().map(|p| p.as_str()))?;
        if files.iter().any(|p| {
            p.as_str()
                .split('/')
                .next()
                .is_some_and(|p| p.eq_ignore_ascii_case("engine-mount-proof"))
        }) {
            return Err("checker uses a reserved engine proof path".into());
        }
        let paths: BTreeSet<_> = files
            .iter()
            .map(|p| p.as_str().to_ascii_lowercase())
            .collect();
        if paths.len() != files.len()
            || stages
                .iter()
                .enumerate()
                .any(|(i, s)| stages[..i].contains(s))
            || evidence
                .iter()
                .enumerate()
                .any(|(i, e)| evidence[..i].contains(e))
        {
            return Err("duplicate evaluator files/stages/evidence".into());
        }
        declarations.push(Declaration {
            name,
            image,
            executable,
            args,
            files,
            stages,
            evidence,
            kind,
            enforcement,
        });
    }
    super::reject_duplicate_names("evaluator", declarations.iter().map(|d| d.name.as_str()))?;
    Ok(declarations)
}

#[derive(Debug, Clone)]
pub(crate) struct CheckerFile {
    pub path: WirePath,
    pub bytes: Vec<u8>,
    pub executable: bool,
}

/// Can only be constructed by resolving a trusted ref. Caller authority over
/// the approved ref is a host precondition; this is not an approval API.
#[derive(Debug, Clone)]
pub struct PinnedRegistration {
    pub(crate) declaration: Declaration,
    pub(crate) files: Vec<CheckerFile>,
    pub(crate) bytes: Vec<u8>,
}
impl PinnedRegistration {
    pub fn checker_files(&self) -> impl Iterator<Item = (&WirePath, &[u8], bool)> {
        self.files
            .iter()
            .map(|f| (&f.path, f.bytes.as_slice(), f.executable))
    }
    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> Digest {
        Digest::of(&self.bytes)
    }

    pub fn at_ref(
        repo: &GitRepo,
        approved_ref: &str,
        pack_dir: &str,
        name: &str,
    ) -> Result<Self, String> {
        if !pack_dir.is_empty() {
            super::validate_pack_relative_path(pack_dir, "approved pack", "directory")?;
        }
        let join = |path: &str| {
            if pack_dir.is_empty() {
                path.to_string()
            } else {
                format!("{pack_dir}/{path}")
            }
        };
        let oid = repo.rev_parse(approved_ref).map_err(|e| e.to_string())?;
        let read = |path: &str| -> Result<(Vec<u8>, bool), String> {
            let entries = repo
                .ls_tree_recursive(&oid, path)
                .map_err(|e| e.to_string())?;
            let entry = entries
                .iter()
                .find(|entry| entry.path == path)
                .ok_or_else(|| format!("approved checker file missing: {path}"))?;
            if entry.kind != "blob"
                || !["100644", "100755"].contains(&entry.mode.as_str())
                || entry.size.is_none_or(|s| s > 8 * 1024 * 1024)
            {
                return Err(format!(
                    "checker requires a bounded regular Git blob: {path}"
                ));
            }
            let bytes = repo
                .show_file(&oid, path)
                .map_err(|e| e.to_string())?
                .ok_or("checker blob disappeared")?;
            if bytes.len() as u64 != entry.size.unwrap_or(0) {
                return Err("checker blob size mismatch".into());
            }
            Ok((bytes, entry.mode == "100755"))
        };
        let (manifest, _) = read(&join(super::PACK_MANIFEST))?;
        let text = std::str::from_utf8(&manifest).map_err(|_| "pack manifest is not UTF-8")?;
        let doc = super::toml::parse(text)?;
        super::validate_sections(&doc)?;
        let (_, schema) = super::manifest_header(&doc)?;
        let declaration = declarations(&doc, schema)?
            .into_iter()
            .find(|d| d.name.as_str() == name)
            .ok_or("evaluator is not registered at the approved ref")?;
        let mut files = Vec::new();
        let mut inventory = Vec::new();
        let mut total = 0usize;
        for path in &declaration.files {
            let (bytes, executable) = read(&join(path.as_str()))?;
            total += bytes.len();
            if total > 32 * 1024 * 1024 {
                return Err("checker dependency bytes exceed limit".into());
            }
            inventory.push(serde_json::json!({"path":path,"digest":Digest::of(&bytes),"bytes":bytes.len(),"executable":executable}));
            files.push(CheckerFile {
                path: path.clone(),
                bytes,
                executable,
            });
        }
        let bytes = serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"sourceCommit":oid,"packDirectory":pack_dir,"manifestDigest":Digest::of(&manifest),"declaration":declaration,"files":inventory})).map_err(|e| e.to_string())?;
        Ok(Self {
            declaration,
            files,
            bytes,
        })
    }
}
