//! Pure Slack routing over the operator-owned repository catalog.

use crate::bridge::{SharedAffinities, SharedThreads};
use crate::host::SharedHost;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SlackRoute {
    pub team_id: String,
    pub channel_id: String,
}

#[derive(Clone)]
pub struct SlackRepo {
    pub id: String,
    pub root: PathBuf,
    pub display_name: String,
    pub routes: Vec<SlackRoute>,
    pub allow_users: Vec<String>,
    pub available: bool,
    pub host: Option<SharedHost>,
    pub unavailable_reason: Option<String>,
}

impl SlackRepo {
    pub fn is_healthy(&self) -> bool {
        self.available && self.unavailable_reason.is_none()
    }

    pub fn primary_route(&self) -> Option<&SlackRoute> {
        self.routes.first()
    }
}

#[derive(Clone)]
pub struct SlackCatalog {
    repos: Arc<BTreeMap<String, Arc<SlackRepo>>>,
    channels: Arc<HashMap<(String, String), String>>,
    affinities: SharedAffinities,
}

pub struct ResolvedEnvelope {
    pub repo: Arc<SlackRepo>,
    pub envelope: Value,
    pub team_id: String,
    pub channel_id: String,
}

impl SlackCatalog {
    pub fn new(repos: Vec<SlackRepo>, affinity_path: PathBuf) -> Result<Self> {
        if repos.is_empty() {
            return Err(anyhow!("Slack repository catalog is empty"));
        }
        let mut by_id = BTreeMap::new();
        let mut channels = HashMap::new();
        for repo in repos {
            if by_id.contains_key(&repo.id) {
                return Err(anyhow!("duplicate Slack repository id '{}'", repo.id));
            }
            for route in &repo.routes {
                let key = (route.team_id.clone(), route.channel_id.clone());
                if let Some(existing) = channels.insert(key.clone(), repo.id.clone()) {
                    return Err(anyhow!(
                        "Slack route {}/{} maps to both '{}' and '{}'",
                        key.0,
                        key.1,
                        existing,
                        repo.id
                    ));
                }
            }
            by_id.insert(repo.id.clone(), Arc::new(repo));
        }
        let affinities = SharedAffinities::load(affinity_path)?;
        for repo in by_id.values() {
            if let Some(route) = repo.primary_route() {
                let imported = affinities.migrate_legacy(
                    &repo.root,
                    &repo.id,
                    &route.team_id,
                    &route.channel_id,
                )?;
                if imported > 0 {
                    tracing::info!(repo = %repo.id, imported, "migrated legacy Slack thread affinity");
                }
            }
        }
        Ok(Self {
            repos: Arc::new(by_id),
            channels: Arc::new(channels),
            affinities,
        })
    }

    pub fn repos(&self) -> impl Iterator<Item = Arc<SlackRepo>> + '_ {
        self.repos.values().cloned()
    }

    pub fn scoped_threads(&self, repo_id: &str, team_id: &str, channel_id: &str) -> SharedThreads {
        SharedThreads::catalog(&self.affinities, repo_id, team_id, channel_id)
    }

    pub fn resolve_envelope(&self, envelope: &Value) -> Result<Option<ResolvedEnvelope>> {
        let mut envelope = envelope.clone();
        let coordinates = Coordinates::from_envelope(&envelope);

        if let (Some(team), Some(channel), Some(thread)) = (
            coordinates.team_id.as_deref(),
            coordinates.channel_id.as_deref(),
            coordinates.thread_ts.as_deref(),
        ) {
            if let Some(target) = self.affinities.target_for(team, channel, thread) {
                return self.finish(
                    &target.repo_id,
                    envelope,
                    Some(team.to_string()),
                    Some(channel.to_string()),
                );
            }
        }

        if let Some(repo_id) = metadata_repo_id(&envelope) {
            return self.finish(
                &repo_id,
                envelope,
                coordinates.team_id,
                coordinates.channel_id,
            );
        }

        if let Some(repo_id) = strip_explicit_repo(&mut envelope) {
            return self.finish(
                &repo_id,
                envelope,
                coordinates.team_id,
                coordinates.channel_id,
            );
        }

        if let (Some(team), Some(channel)) = (
            coordinates.team_id.as_deref(),
            coordinates.channel_id.as_deref(),
        ) {
            if let Some(repo_id) = self.channels.get(&(team.to_string(), channel.to_string())) {
                return self.finish(
                    repo_id,
                    envelope,
                    Some(team.to_string()),
                    Some(channel.to_string()),
                );
            }
        }

        let healthy: Vec<_> = self
            .repos
            .values()
            .filter(|repo| repo.is_healthy())
            .collect();
        if healthy.len() == 1 {
            return self.finish(
                &healthy[0].id,
                envelope,
                coordinates.team_id,
                coordinates.channel_id,
            );
        }

        if envelope.get("envelope_id").is_none() {
            return Ok(None);
        }
        Err(anyhow!(
            "repository is ambiguous; start the command with `repo:<id>`. Valid repositories: {}",
            self.valid_ids()
        ))
    }

    fn finish(
        &self,
        repo_id: &str,
        envelope: Value,
        team_id: Option<String>,
        channel_id: Option<String>,
    ) -> Result<Option<ResolvedEnvelope>> {
        let repo = self.repos.get(repo_id).cloned().ok_or_else(|| {
            anyhow!(
                "unknown repository '{repo_id}'. Valid repositories: {}",
                self.valid_ids()
            )
        })?;
        if !repo.is_healthy() {
            return Err(anyhow!(
                "repository '{}' is unavailable: {}",
                repo.id,
                repo.unavailable_reason.as_deref().unwrap_or("unavailable")
            ));
        }
        let primary = repo.primary_route();
        let team_id = team_id
            .or_else(|| primary.map(|route| route.team_id.clone()))
            .unwrap_or_default();
        let channel_id = channel_id
            .or_else(|| primary.map(|route| route.channel_id.clone()))
            .unwrap_or_default();
        Ok(Some(ResolvedEnvelope {
            repo,
            envelope,
            team_id,
            channel_id,
        }))
    }

    fn valid_ids(&self) -> String {
        self.repos.keys().cloned().collect::<Vec<_>>().join(", ")
    }
}

#[derive(Default)]
struct Coordinates {
    team_id: Option<String>,
    channel_id: Option<String>,
    thread_ts: Option<String>,
}

impl Coordinates {
    fn from_envelope(envelope: &Value) -> Self {
        let payload = envelope.get("payload").unwrap_or(&Value::Null);
        let event = payload.get("event").unwrap_or(&Value::Null);
        let metadata = modal_metadata(payload);
        Self {
            team_id: string_at(payload, &["team_id"])
                .or_else(|| string_at(payload, &["team", "id"]))
                .or_else(|| {
                    metadata
                        .as_ref()
                        .and_then(|value| string_at(value, &["teamId"]))
                }),
            channel_id: string_at(payload, &["channel_id"])
                .or_else(|| string_at(payload, &["channel", "id"]))
                .or_else(|| string_at(payload, &["container", "channel_id"]))
                .or_else(|| string_at(event, &["channel"]))
                .or_else(|| {
                    metadata
                        .as_ref()
                        .and_then(|value| string_at(value, &["channel"]))
                }),
            thread_ts: string_at(payload, &["thread_ts"])
                .or_else(|| string_at(payload, &["container", "thread_ts"]))
                .or_else(|| string_at(payload, &["message", "thread_ts"]))
                .or_else(|| string_at(payload, &["message", "ts"]))
                .or_else(|| string_at(event, &["thread_ts"])),
        }
    }
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut cursor = value;
    for component in path {
        cursor = cursor.get(*component)?;
    }
    cursor
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn modal_metadata(payload: &Value) -> Option<Value> {
    let text = payload.get("view")?.get("private_metadata")?.as_str()?;
    serde_json::from_str(text).ok()
}

fn metadata_repo_id(envelope: &Value) -> Option<String> {
    let payload = envelope.get("payload")?;
    modal_metadata(payload).and_then(|value| string_at(&value, &["repoId"]))
}

fn strip_explicit_repo(envelope: &mut Value) -> Option<String> {
    if envelope.get("type").and_then(Value::as_str) != Some("slash_commands") {
        return None;
    }
    let text = envelope.get("payload")?.get("text")?.as_str()?.trim();
    let mut parts = text.splitn(2, char::is_whitespace);
    let token = parts.next()?;
    let repo_id = token.strip_prefix("repo:")?.trim().to_string();
    if repo_id.is_empty() {
        return None;
    }
    let rest = parts.next().unwrap_or("").trim().to_string();
    envelope["payload"]["text"] = Value::String(rest);
    Some(repo_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn repo(id: &str, team: &str, channel: &str, healthy: bool) -> SlackRepo {
        SlackRepo {
            id: id.into(),
            root: tempdir().unwrap().keep(),
            display_name: id.into(),
            routes: vec![SlackRoute {
                team_id: team.into(),
                channel_id: channel.into(),
            }],
            allow_users: Vec::new(),
            available: healthy,
            host: None,
            unavailable_reason: (!healthy).then(|| "missing".into()),
        }
    }

    fn slash(text: &str, team: &str, channel: &str) -> Value {
        json!({
            "type": "slash_commands",
            "envelope_id": "e1",
            "payload": {
                "command": "/kranz",
                "text": text,
                "team_id": team,
                "channel_id": channel,
                "response_url": "https://example.invalid/response"
            }
        })
    }

    #[test]
    fn explicit_repo_precedes_channel_and_is_removed_before_action_routing() {
        let tmp = tempdir().unwrap();
        let catalog = SlackCatalog::new(
            vec![
                repo("alpha", "T1", "CA", true),
                repo("beta", "T1", "CB", true),
            ],
            tmp.path().join("affinity.json"),
        )
        .unwrap();
        let resolved = catalog
            .resolve_envelope(&slash("repo:beta status", "T1", "CA"))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.repo.id, "beta");
        assert_eq!(resolved.envelope["payload"]["text"], "status");
    }

    #[test]
    fn exact_channel_then_sole_healthy_and_ambiguity_refusal() {
        let tmp = tempdir().unwrap();
        let catalog = SlackCatalog::new(
            vec![
                repo("alpha", "T1", "CA", true),
                repo("beta", "T1", "CB", true),
            ],
            tmp.path().join("affinity.json"),
        )
        .unwrap();
        assert_eq!(
            catalog
                .resolve_envelope(&slash("status", "T1", "CB"))
                .unwrap()
                .unwrap()
                .repo
                .id,
            "beta"
        );
        assert!(catalog
            .resolve_envelope(&slash("status", "T9", "CZ"))
            .err()
            .unwrap()
            .to_string()
            .contains("ambiguous"));

        let catalog = SlackCatalog::new(
            vec![
                repo("alpha", "T1", "CA", true),
                repo("beta", "T1", "CB", false),
            ],
            tmp.path().join("affinity-sole.json"),
        )
        .unwrap();
        assert_eq!(
            catalog
                .resolve_envelope(&slash("status", "T9", "CZ"))
                .unwrap()
                .unwrap()
                .repo
                .id,
            "alpha"
        );
    }

    #[test]
    fn unavailable_explicit_target_never_falls_back() {
        let tmp = tempdir().unwrap();
        let catalog = SlackCatalog::new(
            vec![
                repo("alpha", "T1", "CA", true),
                repo("beta", "T1", "CB", false),
            ],
            tmp.path().join("affinity.json"),
        )
        .unwrap();
        assert!(catalog
            .resolve_envelope(&slash("repo:beta status", "T1", "CA"))
            .err()
            .unwrap()
            .to_string()
            .contains("unavailable"));
    }

    #[test]
    fn thread_affinity_precedes_the_current_channel_mapping() {
        let tmp = tempdir().unwrap();
        let affinity_path = tmp.path().join("affinity.json");
        let mut affinities = crate::threads::AffinityMap::default();
        affinities.set("T1", "CB", "100.1", "alpha", "m-same");
        affinities.save(&affinity_path).unwrap();
        let catalog = SlackCatalog::new(
            vec![
                repo("alpha", "T1", "CA", true),
                repo("beta", "T1", "CB", true),
            ],
            affinity_path,
        )
        .unwrap();
        let envelope = json!({
            "type": "events_api",
            "envelope_id": "e-thread",
            "payload": {
                "team_id": "T1",
                "event": {
                    "type": "message",
                    "channel": "CB",
                    "thread_ts": "100.1",
                    "ts": "100.2",
                    "user": "U1",
                    "text": "continue"
                }
            }
        });

        assert_eq!(
            catalog
                .resolve_envelope(&envelope)
                .unwrap()
                .unwrap()
                .repo
                .id,
            "alpha"
        );
    }
}
