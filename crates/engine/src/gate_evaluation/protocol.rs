//! Typed wire contract matching the shipped v1 schemas. Structural checks
//! complement, and never replace, byte binding and filesystem checks.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_JSON_INTEGER: u64 = 9_007_199_254_740_991;

fn require(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct Id(String);
impl TryFrom<String> for Id {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        require(
            !value.is_empty()
                && value.len() <= 128
                && value.as_bytes()[0].is_ascii_alphanumeric()
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
            "invalid gate identifier",
        )?;
        Ok(Self(value))
    }
}
impl Id {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct Digest(String);
impl TryFrom<String> for Digest {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        require(
            value.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }),
            "invalid SHA-256 digest",
        )?;
        Ok(Self(value))
    }
}
impl Digest {
    pub fn of(bytes: &[u8]) -> Self {
        use sha2::{Digest as _, Sha256};
        Self(format!(
            "sha256:{}",
            Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct WirePath(String);
impl TryFrom<String> for WirePath {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        require(
            !value.is_empty() && value.len() <= 1024,
            "invalid wire path length",
        )?;
        for component in value.split('/') {
            require(
                !component.is_empty()
                    && component != "."
                    && component != ".."
                    && !component.starts_with(' ')
                    && component
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b" _.-".contains(&b))
                    && !component.ends_with(['.', ' ']),
                "invalid wire path component",
            )?;
            let base = component
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            require(
                !["CON", "PRN", "AUX", "NUL"].contains(&base.as_str())
                    && !(base.len() == 4
                        && (base.starts_with("COM") || base.starts_with("LPT"))
                        && matches!(base.as_bytes()[3], b'1'..=b'9')),
                "reserved wire path component",
            )?;
        }
        Ok(Self(value))
    }
}
impl WirePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reject file/directory prefix conflicts and case aliases at every component,
/// including distinct files under differently cased directory spellings.
pub(crate) fn validate_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let paths: Vec<_> = paths.into_iter().collect();
    let mut spelling = std::collections::BTreeMap::new();
    let mut leaves = BTreeSet::new();
    for path in &paths {
        require(
            leaves.insert(path.to_ascii_lowercase()),
            "duplicate filesystem path",
        )?;
        let mut prefix = String::new();
        for part in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(prior) = spelling.insert(prefix.to_ascii_lowercase(), prefix.clone()) {
                require(prior == prefix, "case-ambiguous filesystem component")?;
            }
        }
    }
    for path in paths {
        for (index, _) in path.match_indices('/') {
            require(
                !leaves.contains(&path[..index].to_ascii_lowercase()),
                "file/directory path conflict",
            )?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    PlanApproval,
    CommandPermission,
    MilestoneValidation,
    FinalGate,
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitObject {
    pub algorithm: String,
    pub value: String,
}
impl GitObject {
    fn validate(&self) -> Result<(), String> {
        let length = match self.algorithm.as_str() {
            "sha1" => 40,
            "sha256" => 64,
            _ => return Err("unknown Git object algorithm".into()),
        };
        require(
            self.value.len() == length
                && self
                    .value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid Git object digest",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    pub subject_digest: Digest,
    pub plan_digest: Digest,
    pub policy_digest: Digest,
    pub registration_digest: Digest,
    pub workspace_id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PeerRequestId {
    Text(String),
    Number(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Subject {
    Plan {
        revision: u64,
        plan_digest: Digest,
        base_commit: GitObject,
    },
    Invocation {
        run_id: Id,
        peer_session_id: String,
        tool_call_id: String,
        peer_request_id: PeerRequestId,
        action_digest: Digest,
        options_digest: Digest,
        cwd_id: Id,
    },
    Milestone {
        milestone_id: Id,
        start_commit: GitObject,
        snapshot_digest: Digest,
        criteria_digest: Digest,
    },
    Deliverable {
        base_commit: GitObject,
        snapshot_digest: Digest,
        feature_receipt_digest: Digest,
    },
    Integration {
        live_base_commit: GitObject,
        candidate_commit: GitObject,
        integration_tree: GitObject,
        snapshot_digest: Digest,
    },
}
impl Subject {
    pub fn stage(&self) -> Stage {
        match self {
            Self::Plan { .. } => Stage::PlanApproval,
            Self::Invocation { .. } => Stage::CommandPermission,
            Self::Milestone { .. } => Stage::MilestoneValidation,
            Self::Deliverable { .. } => Stage::FinalGate,
            Self::Integration { .. } => Stage::Merge,
        }
    }
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Plan {
                revision,
                base_commit,
                ..
            } => {
                require(
                    (1..=MAX_JSON_INTEGER).contains(revision),
                    "invalid plan revision",
                )?;
                base_commit.validate()
            }
            Self::Invocation {
                peer_session_id,
                tool_call_id,
                peer_request_id,
                ..
            } => {
                bounded_text(peer_session_id, 256)?;
                bounded_text(tool_call_id, 256)?;
                match peer_request_id {
                    PeerRequestId::Text(text) => bounded_text(text, 256),
                    PeerRequestId::Number(n) => {
                        require(*n <= MAX_JSON_INTEGER, "invalid peer request id")
                    }
                }
            }
            Self::Milestone { start_commit, .. } => start_commit.validate(),
            Self::Deliverable { base_commit, .. } => base_commit.validate(),
            Self::Integration {
                live_base_commit,
                candidate_commit,
                integration_tree,
                ..
            } => {
                live_base_commit.validate()?;
                candidate_commit.validate()?;
                integration_tree.validate()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub path: WirePath,
    pub digest: Digest,
    pub bytes: u64,
}
impl ArtifactRef {
    fn validate(&self, input: bool) -> Result<(), String> {
        require(
            self.bytes <= 64 * 1024 * 1024,
            "artifact exceeds wire byte limit",
        )?;
        require(
            if input {
                self.path.as_str().starts_with("inputs/")
            } else {
                !self.path.as_str().starts_with("outputs/")
            },
            "artifact path has the wrong input/output root",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limits {
    pub wall_time_ms: u64,
    pub write_time_ms: u64,
    pub max_frame_bytes: u64,
    pub max_stdout_bytes: u64,
    pub max_stderr_bytes: u64,
    pub max_artifact_bytes: u64,
    pub max_artifacts: u64,
}
impl Limits {
    fn validate(&self) -> Result<(), String> {
        require(
            (1..=3_600_000).contains(&self.wall_time_ms)
                && (1..=60_000).contains(&self.write_time_ms),
            "invalid gate time limits",
        )?;
        require(
            [
                self.max_frame_bytes,
                self.max_stdout_bytes,
                self.max_stderr_bytes,
            ]
            .iter()
            .all(|n| (1..=1_048_576).contains(n)),
            "invalid gate stream limits",
        )?;
        require(
            (1..=67_108_864).contains(&self.max_artifact_bytes) && self.max_artifacts <= 128,
            "invalid gate artifact limits",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Parameters {
    pub schema_version: u64,
    pub evaluation_id: Id,
    pub attempt_id: Id,
    pub gate_id: Id,
    pub mission_id: Id,
    pub binding: Binding,
    pub stage: Stage,
    pub subject: Subject,
    pub evidence: ArtifactRef,
    pub deadline: String,
    pub limits: Limits,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Id,
    pub method: String,
    pub params: Parameters,
}
impl Request {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        require(bytes.len() <= 1_048_576, "request exceeds frame limit")?;
        let request: Self = decode(bytes)?;
        request.validate()?;
        Ok(request)
    }
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.jsonrpc == "2.0"
                && self.method == "gate/evaluate"
                && self.params.schema_version == 1,
            "unsupported gate request protocol/version",
        )?;
        require(
            self.id == self.params.attempt_id,
            "JSON-RPC id must equal attemptId",
        )?;
        require(
            self.params.stage == self.params.subject.stage(),
            "stage/subject mismatch",
        )?;
        self.params.subject.validate()?;
        self.params.evidence.validate(true)?;
        self.params.limits.validate()?;
        require(
            self.params.deadline.len() == 20
                && self
                    .params
                    .deadline
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| match i {
                        4 | 7 => b == b'-',
                        10 => b == b'T',
                        13 | 16 => b == b':',
                        19 => b == b'Z',
                        _ => b.is_ascii_digit(),
                    }),
            "deadline must have UTC second precision",
        )?;
        chrono::DateTime::parse_from_rfc3339(&self.params.deadline)
            .map_err(|_| "invalid gate deadline".to_string())?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactRole {
    Scope,
    Criteria,
    Subject,
    Policy,
    Registration,
    Plan,
    Action,
    PermissionOptions,
    FeatureReceipts,
    Diff,
    SnapshotInventory,
    Source,
    CheckReceipt,
    PriorFinding,
    Context,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProducerKind {
    Engine,
    IndependentChecker,
    Worker,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Producer {
    pub kind: ProducerKind,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub run_id: Option<Id>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputArtifact {
    pub id: Id,
    pub role: ArtifactRole,
    pub content: ArtifactRef,
    pub producer: Producer,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogRange {
    pub first: u64,
    pub last: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u64,
    pub mission_id: Id,
    pub binding: Binding,
    pub artifacts: Vec<InputArtifact>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_log_range: Option<LogRange>,
}
impl Manifest {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        require(bytes.len() <= 67_108_864, "manifest exceeds byte limit")?;
        let manifest: Self = decode(bytes)?;
        require(
            manifest.schema_version == 1
                && !manifest.artifacts.is_empty()
                && manifest.artifacts.len() <= 100_000,
            "unsupported or empty evidence manifest",
        )?;
        if let Some(range) = &manifest.source_log_range {
            require(
                range.first <= range.last && range.last <= MAX_JSON_INTEGER,
                "invalid source log range",
            )?;
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for artifact in &manifest.artifacts {
            artifact.content.validate(true)?;
            require(
                ids.insert(&artifact.id)
                    && paths.insert(artifact.content.path.as_str().to_ascii_lowercase()),
                "duplicate artifact ID or case-ambiguous path",
            )?;
        }
        require(
            manifest.artifacts.windows(2).all(|a| a[0].id < a[1].id),
            "manifest artifacts must be sorted by ID",
        )?;
        validate_paths(manifest.artifacts.iter().map(|a| a.content.path.as_str()))?;
        Ok(manifest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Anchor {
    pub artifact_id: Id,
    pub digest: Digest,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub line_start: Option<u64>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub line_end: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: Id,
    pub severity: Severity,
    pub summary: String,
    pub evidence: Vec<Anchor>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Judged,
    Escalate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationResult {
    pub schema_version: u64,
    pub evaluation_id: Id,
    pub attempt_id: Id,
    pub binding: Binding,
    pub evidence_digest: Digest,
    pub status: Status,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub verdict: Option<Verdict>,
    pub rationale: String,
    pub artifacts: Vec<ArtifactRef>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub findings: Option<Vec<Finding>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub confidence: Option<f64>,
}
impl EvaluationResult {
    fn validate(&self) -> Result<(), String> {
        require(
            self.schema_version == 1 && ((self.status == Status::Judged) == self.verdict.is_some()),
            "invalid judged/escalate result",
        )?;
        bounded_text(&self.rationale, 8192)?;
        require(
            self.artifacts.len() <= 128 && self.findings.as_ref().is_none_or(|f| f.len() <= 128),
            "too many result artifacts/findings",
        )?;
        let mut paths = BTreeSet::new();
        for artifact in &self.artifacts {
            artifact.validate(false)?;
            require(
                paths.insert(artifact.path.as_str().to_ascii_lowercase()),
                "duplicate output artifact path",
            )?;
        }
        validate_paths(self.artifacts.iter().map(|a| a.path.as_str()))?;
        if let Some(confidence) = self.confidence {
            require(
                confidence.is_finite() && (0.0..=1.0).contains(&confidence),
                "invalid confidence",
            )?;
        }
        let mut ids = BTreeSet::new();
        for finding in self.findings.iter().flatten() {
            require(ids.insert(&finding.id), "duplicate finding ID")?;
            bounded_text(&finding.summary, 4096)?;
            require(
                !finding.evidence.is_empty() && finding.evidence.len() <= 32,
                "finding requires bounded evidence anchors",
            )?;
            for anchor in &finding.evidence {
                match (anchor.line_start, anchor.line_end) {
                    (None, None) => {}
                    (Some(start), Some(end)) => require(
                        start >= 1 && start <= end && end <= MAX_JSON_INTEGER,
                        "invalid finding line range",
                    )?,
                    _ => return Err("finding line bounds must be paired".into()),
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorData {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub evaluation_id: Option<Id>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub attempt_id: Option<Id>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub data: Option<ErrorData>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuccessResponse {
    pub jsonrpc: String,
    pub id: Id,
    pub result: EvaluationResult,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub jsonrpc: String,
    #[serde(deserialize_with = "required_nullable")]
    pub id: Option<Id>,
    pub error: RpcError,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Result(Box<SuccessResponse>),
    Error(ErrorResponse),
}
impl Response {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        require(bytes.len() <= 1_048_576, "response exceeds frame limit")?;
        let response: Self = decode(bytes)?;
        match &response {
            Self::Result(r) => {
                require(r.jsonrpc == "2.0", "invalid JSON-RPC version")?;
                r.result.validate()?;
            }
            Self::Error(r) => {
                require(
                    r.jsonrpc == "2.0"
                        && [
                            -32700, -32600, -32601, -32602, -32603, 1001, 1002, 1003, 1004,
                        ]
                        .contains(&r.error.code),
                    "invalid gate RPC error",
                )?;
                require(
                    r.id.is_some() || [-32700, -32600].contains(&r.error.code),
                    "null ID only identifies a malformed request error",
                )?;
                bounded_text(&r.error.message, 4096)?;
            }
        }
        Ok(response)
    }
    pub fn correlate(&self, request: &Request) -> Result<(), String> {
        match self {
            Self::Result(r) => require(
                r.id == request.id
                    && r.result.evaluation_id == request.params.evaluation_id
                    && r.result.attempt_id == request.params.attempt_id
                    && r.result.binding == request.params.binding
                    && r.result.evidence_digest == request.params.evidence.digest,
                "gate response correlation or evidence binding mismatch",
            ),
            Self::Error(_) => Err("evaluator could not judge the evidence".into()),
        }
    }
}

fn bounded_text(text: &str, max: usize) -> Result<(), String> {
    require(
        !text.is_empty() && text.chars().count() <= max,
        "invalid bounded text length",
    )
}
pub(super) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let mut value =
        crate::strict_json::parse(bytes).map_err(|e| format!("invalid gate JSON: {e}"))?;
    normalize_integral_numbers(&mut value);
    serde_json::from_value(value).map_err(|e| format!("invalid gate wire shape: {e}"))
}

// JSON Schema integers include 1.0 and 1e0. Normalize only exact integers
// inside the contract's safe range; hashes still cover original input bytes.
fn normalize_integral_numbers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => values.iter_mut().for_each(normalize_integral_numbers),
        serde_json::Value::Object(values) => {
            values.values_mut().for_each(normalize_integral_numbers)
        }
        serde_json::Value::Number(number) if number.is_f64() => {
            if let Some(n) = number
                .as_f64()
                .filter(|n| n.fract() == 0.0 && n.abs() <= MAX_JSON_INTEGER as f64)
            {
                *value = serde_json::Value::from(n as i64);
            }
        }
        _ => {}
    }
}

// Missing optional fields are allowed; explicit null is not a schema value.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
fn required_nullable<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Id>, D::Error> {
    Option::<Id>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    fn fixture(stage: &str, file: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("schemas/fixtures/gate-v1")
                .join(stage)
                .join(file),
        )
        .unwrap()
    }
    #[test]
    fn gate_eval_v1_all_shipped_stage_fixtures_decode_and_correlate() {
        for stage in [
            "plan-approval",
            "command-permission",
            "milestone-validation",
            "final-gate",
            "merge",
        ] {
            let request = Request::from_bytes(&fixture(stage, "request.json")).unwrap();
            let manifest = Manifest::from_bytes(&fixture(stage, "inputs/manifest.json")).unwrap();
            assert_eq!(manifest.binding, request.params.binding);
            for verdict in ["pass.json", "fail.json", "escalate.json"] {
                let response = Response::from_bytes(&fixture(stage, verdict)).unwrap();
                response.correlate(&request).unwrap();
            }
        }
    }
    #[test]
    fn gate_eval_v1_rejects_authority_fields_nulls_versions_and_correlations() {
        let original: Value = serde_json::from_slice(&fixture("final-gate", "pass.json")).unwrap();
        let request = Request::from_bytes(&fixture("final-gate", "request.json")).unwrap();
        for (key, value) in [
            ("actor", json!("human:spoof")),
            ("verdict", Value::Null),
            ("findings", Value::Null),
            ("confidence", Value::Null),
            ("schemaVersion", json!(2)),
        ] {
            let mut invalid = original.clone();
            invalid["result"][key] = value;
            assert!(
                Response::from_bytes(&serde_json::to_vec(&invalid).unwrap()).is_err(),
                "{key}"
            );
        }
        let mut wrong = original.clone();
        wrong["result"]["attemptId"] = json!("another-attempt");
        assert!(Response::from_bytes(&serde_json::to_vec(&wrong).unwrap())
            .unwrap()
            .correlate(&request)
            .is_err());
        assert!(Response::from_bytes(
            br#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"bad"}}"#
        )
        .is_err());
        assert!(Response::from_bytes(
            br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad"}}"#
        )
        .is_ok());
        assert!(Response::from_bytes(
            br#"{"jsonrpc":"2.0","id":null,"error":{"code":1004,"message":"bad"}}"#
        )
        .is_err());
        let mut numeric_request: Value =
            serde_json::from_slice(&fixture("final-gate", "request.json")).unwrap();
        numeric_request["params"]["schemaVersion"] = json!(1.0);
        assert!(Request::from_bytes(&serde_json::to_vec(&numeric_request).unwrap()).is_ok());
        let mut wrong_request: Value =
            serde_json::from_slice(&fixture("final-gate", "request.json")).unwrap();
        wrong_request["params"]["stage"] = json!("merge");
        assert!(Request::from_bytes(&serde_json::to_vec(&wrong_request).unwrap()).is_err());
    }
    #[test]
    fn gate_eval_v1_rejects_aliases_at_parent_components_and_output_prefixes() {
        assert!(validate_paths(["inputs/A/one", "inputs/a/two"]).is_err());
        assert!(validate_paths(["inputs/file", "inputs/file/child"]).is_err());
        let mut result: Value =
            serde_json::from_slice(&fixture("final-gate", "pass.json")).unwrap();
        result["result"]["artifacts"] =
            json!([{"path":"outputs/claim.txt","digest":Digest::of(b""),"bytes":0}]);
        assert!(Response::from_bytes(&serde_json::to_vec(&result).unwrap()).is_err());
    }
    #[test]
    fn gate_eval_v1_rejects_ambiguous_paths_and_duplicate_json() {
        for path in [
            "../x",
            "inputs/../x",
            "/absolute",
            "inputs/CON.txt",
            "inputs/nul",
            "inputs/a.",
            "a\\b",
            "inputs/a:b",
            "inputs/a\n",
        ] {
            assert!(WirePath::try_from(path.to_string()).is_err(), "{path}");
        }
        assert_eq!(
            Digest::of(b"abc").as_str(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(Response::from_bytes(br#"{"jsonrpc":"2.0","jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad"}}"#).is_err());
    }
}
