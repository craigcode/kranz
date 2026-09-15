//! Frozen inputs and mechanical response attribution. Callers are trusted
//! engine code selecting minimal evidence, never a worker's manifest loader.
use super::protocol::*;
use crate::pack::evaluator::PinnedRegistration;
use std::collections::{BTreeMap, BTreeSet};

/// Owns the exact validated bytes. No evaluator-supplied paths are read on
/// the host while constructing inputs. Stage orchestration supplies provenance.
#[derive(Debug)]
pub struct FrozenEvidence {
    pub(crate) request: Request,
    pub(crate) manifest: Manifest,
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) inputs: BTreeMap<Id, Vec<u8>>,
}
impl FrozenEvidence {
    pub fn new(
        request_bytes: Vec<u8>,
        manifest_bytes: Vec<u8>,
        inputs: BTreeMap<Id, Vec<u8>>,
        registration: &PinnedRegistration,
    ) -> Result<Self, String> {
        let request = Request::from_bytes(&request_bytes)?;
        if serde_json::to_vec(&request)
            .map_err(|e| e.to_string())?
            .len() as u64
            > request.params.limits.max_frame_bytes
        {
            return Err("request exceeds selected frame limit".into());
        }
        let manifest = Manifest::from_bytes(&manifest_bytes)?;
        if request.params.binding != manifest.binding
            || request.params.mission_id != manifest.mission_id
            || request.params.evidence.digest != Digest::of(&manifest_bytes)
            || request.params.evidence.bytes != manifest_bytes.len() as u64
        {
            return Err("evidence manifest does not match the engine request".into());
        }
        if request.params.gate_id != registration.declaration.name
            || request.params.binding.registration_digest != registration.digest()
            || !registration
                .declaration
                .stages
                .contains(&request.params.stage)
        {
            return Err("request does not match the approved checker registration".into());
        }
        if inputs.len() != manifest.artifacts.len() || inputs.len() > 1000 {
            return Err("input inventory is incomplete or exceeds the host count limit".into());
        }
        let total: usize = inputs.values().map(Vec::len).sum();
        if total > 128 * 1024 * 1024 {
            return Err("input bytes exceed host limit".into());
        }
        let mut paths = BTreeSet::new();
        paths.insert(request.params.evidence.path.as_str().to_ascii_lowercase());
        for artifact in &manifest.artifacts {
            let bytes = inputs
                .get(&artifact.id)
                .ok_or("manifest input is missing")?;
            if bytes.len() as u64 != artifact.content.bytes
                || Digest::of(bytes) != artifact.content.digest
                || !paths.insert(artifact.content.path.as_str().to_ascii_lowercase())
            {
                return Err("input digest, length or path collision".into());
            }
        }
        super::protocol::validate_paths(
            manifest
                .artifacts
                .iter()
                .map(|a| a.content.path.as_str())
                .chain(std::iter::once(request.params.evidence.path.as_str())),
        )?;
        if manifest.artifacts.iter().any(|a| {
            a.content
                .path
                .as_str()
                .eq_ignore_ascii_case("inputs/engine-mount-proof")
        }) {
            return Err("input uses a reserved engine proof path".into());
        }
        let frozen = Self {
            request,
            manifest,
            manifest_bytes,
            inputs,
        };
        frozen.require_role(
            ArtifactRole::Subject,
            &frozen.request.params.binding.subject_digest,
        )?;
        let subject: Subject = super::protocol::decode(frozen.role_bytes(ArtifactRole::Subject)?)?;
        if subject != frozen.request.params.subject {
            return Err("inline subject differs from retained subject bytes".into());
        }
        frozen.require_role(
            ArtifactRole::Plan,
            &frozen.request.params.binding.plan_digest,
        )?;
        frozen.require_role(
            ArtifactRole::Policy,
            &frozen.request.params.binding.policy_digest,
        )?;
        frozen.require_role(ArtifactRole::Registration, &registration.digest())?;
        if frozen.role_bytes(ArtifactRole::Registration)? != registration.bytes() {
            return Err("registration bytes differ from approved checker".into());
        }
        // These are JSON authority objects; reject duplicate keys here too.
        for role in [
            ArtifactRole::Subject,
            ArtifactRole::Plan,
            ArtifactRole::Policy,
            ArtifactRole::Registration,
        ] {
            if !crate::strict_json::parse(frozen.role_bytes(role)?)
                .map_err(|e| e.to_string())?
                .is_object()
            {
                return Err("authority input must be a JSON object".into());
            }
        }
        match &frozen.request.params.subject {
            Subject::Plan { plan_digest, .. } => {
                frozen.require_role(ArtifactRole::Plan, plan_digest)?
            }
            Subject::Invocation {
                action_digest,
                options_digest,
                ..
            } => {
                frozen.require_role(ArtifactRole::Action, action_digest)?;
                frozen.require_role(ArtifactRole::PermissionOptions, options_digest)?;
            }
            Subject::Milestone {
                snapshot_digest,
                criteria_digest,
                ..
            } => {
                frozen.require_snapshot(snapshot_digest)?;
                frozen.require_role(ArtifactRole::Criteria, criteria_digest)?;
            }
            Subject::Deliverable {
                snapshot_digest,
                feature_receipt_digest,
                ..
            } => {
                frozen.require_snapshot(snapshot_digest)?;
                frozen.require_role(ArtifactRole::FeatureReceipts, feature_receipt_digest)?;
            }
            Subject::Integration {
                snapshot_digest, ..
            } => frozen.require_snapshot(snapshot_digest)?,
        }
        for role in &registration.declaration.evidence {
            if !frozen.manifest.artifacts.iter().any(|a| a.role == *role) {
                return Err("declared evidence role is missing".into());
            }
        }
        Ok(frozen)
    }
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }
    pub fn request(&self) -> &Request {
        &self.request
    }
    fn role(&self, role: ArtifactRole) -> Result<&InputArtifact, String> {
        let mut matches = self.manifest.artifacts.iter().filter(|a| a.role == role);
        let artifact = matches.next().ok_or("required evidence role is missing")?;
        if matches.next().is_some() {
            return Err("authority evidence role must be unique".into());
        }
        Ok(artifact)
    }
    fn role_bytes(&self, role: ArtifactRole) -> Result<&[u8], String> {
        Ok(&self.inputs[&self.role(role)?.id])
    }
    fn require_role(&self, role: ArtifactRole, digest: &Digest) -> Result<(), String> {
        let artifact = self.role(role)?;
        if artifact.producer.kind != ProducerKind::Engine || &artifact.content.digest != digest {
            return Err(
                "required authority evidence lacks engine provenance or the bound digest".into(),
            );
        }
        Ok(())
    }
    fn require_snapshot(&self, digest: &Digest) -> Result<(), String> {
        self.require_role(ArtifactRole::SnapshotInventory, digest)?;
        let snapshot: SnapshotInventory =
            super::protocol::decode(self.role_bytes(ArtifactRole::SnapshotInventory)?)?;
        if snapshot.schema_version != 1 || snapshot.entries.len() > 100_000 {
            return Err("unsupported snapshot inventory".into());
        }
        super::protocol::validate_paths(snapshot.entries.iter().map(|entry| match entry {
            SnapshotEntry::File { path, .. } | SnapshotEntry::Deleted { path } => path.as_str(),
        }))?;
        let mut paths = BTreeSet::new();
        for entry in &snapshot.entries {
            let path = match entry {
                SnapshotEntry::File { path, .. } | SnapshotEntry::Deleted { path } => path,
            };
            if !paths.insert(path.as_str().to_ascii_lowercase()) {
                return Err("duplicate snapshot path".into());
            }
            if let SnapshotEntry::File {
                artifact_id,
                digest,
                ..
            } = entry
            {
                let artifact = self
                    .manifest
                    .artifacts
                    .iter()
                    .find(|a| a.id == *artifact_id)
                    .ok_or("snapshot source bytes missing")?;
                if artifact.role != ArtifactRole::Source || artifact.content.digest != *digest {
                    return Err("snapshot source binding mismatch".into());
                }
            }
        }
        Ok(())
    }
    pub fn validate_findings(&self, result: &EvaluationResult) -> Result<(), String> {
        for finding in result.findings.iter().flatten() {
            for anchor in &finding.evidence {
                let artifact = self
                    .manifest
                    .artifacts
                    .iter()
                    .find(|a| a.id == anchor.artifact_id)
                    .ok_or("finding references a missing input")?;
                if artifact.content.digest != anchor.digest {
                    return Err("finding digest mismatch".into());
                }
                if let Some(end) = anchor.line_end {
                    let bytes = &self.inputs[&artifact.id];
                    std::str::from_utf8(bytes)
                        .map_err(|_| "line anchor requires UTF-8 evidence")?;
                    let lines = if bytes.is_empty() {
                        0
                    } else {
                        bytes.iter().filter(|b| **b == b'\n').count()
                            + usize::from(!bytes.ends_with(b"\n"))
                    };
                    if end > lines as u64 {
                        return Err("finding line is outside its evidence".into());
                    }
                }
            }
        }
        Ok(())
    }
}

/// Source selection is host policy. v1 supports regular files and deletions;
/// symlink/submodule/non-portable paths fail readiness rather than traversing.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotInventory {
    pub schema_version: u64,
    pub entries: Vec<SnapshotEntry>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SnapshotEntry {
    File {
        path: WirePath,
        artifact_id: Id,
        digest: Digest,
        executable: bool,
    },
    Deleted {
        path: WirePath,
    },
}
