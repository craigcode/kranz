//! Role → permission profile mapping (plan §4.7; the table in
//! `docs/design.md` is authoritative).
//!
//! Deny rules take precedence over allows in Claude Code, and in `-p` mode a
//! tool call outside the allowed set simply fails with an error the model can
//! read — that failure IS the read-only guarantee for orchestrator and
//! validators. The profiles here express every restriction through
//! `permission_mode` + `allowed_tools` + `disallowed_tools`: the CLI `--tools`
//! built-in restriction is recorded on the profile for future wiring, but
//! [`apply`] does not map it because [`SessionSpec`] (contract) has no field
//! for it yet.

use crate::backend::SessionSpec;
use crate::types::{MissionConfig, Role};

/// Tool names recognized when deciding whether a config `deny_patterns` entry
/// is already a tool rule (vs. a bare Bash command pattern to wrap).
const KNOWN_TOOLS: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "Task",
    "TodoWrite",
    "SlashCommand",
    "KillShell",
    "BashOutput",
];

/// Built-in worker deny list (§4.7): no pushing, no publishing, no privilege
/// escalation, no raw network access.
const WORKER_DENY: &[&str] = &[
    "Bash(git push*)",
    "Bash(git remote add*)",
    "Bash(npm publish*)",
    "Bash(yarn publish*)",
    "Bash(pnpm publish*)",
    "Bash(cargo publish*)",
    "Bash(twine*)",
    "Bash(gem push*)",
    "Bash(sudo*)",
    "Bash(curl*)",
    "Bash(wget*)",
    "WebFetch",
    "WebSearch",
];

/// Git inspection patterns shared by the orchestrator and both validators.
///
/// `git branch` and `git tag` are deliberately NOT allowed as bare `*`
/// prefixes: `Bash(git branch*)` would also match mutating invocations such
/// as `git branch -D main` or `git branch -f`, and `Bash(git tag*)` would
/// match `git tag -d v1` and tag creation. Only the read-only listing/query
/// forms are enumerated instead (exact match unless the entry ends in `*`).
const GIT_INSPECT: &[&str] = &[
    "Bash(git log*)",
    "Bash(git diff*)",
    "Bash(git show*)",
    "Bash(git status*)",
    "Bash(git rev-parse*)",
    // Read-only `git branch` forms.
    "Bash(git branch)",
    "Bash(git branch --list*)",
    "Bash(git branch --show-current)",
    "Bash(git branch -a)",
    "Bash(git branch -r)",
    "Bash(git branch --contains*)",
    // Read-only `git tag` forms.
    "Bash(git tag)",
    "Bash(git tag --list*)",
    "Bash(git tag -l*)",
    "Bash(git tag --contains*)",
];

/// Deny list for the read-only roles (orchestrator, validators).
const READ_ONLY_DENY: &[&str] =
    &["Write", "Edit", "NotebookEdit", "WebFetch", "WebSearch", "Bash(git push*)"];

/// The CLI `--tools` restriction for read-only roles (design.md table).
const INSPECT_TOOLS: &[&str] = &["Bash", "Read", "Glob", "Grep"];

/// Everything the engine passes to the CLI to sandbox one role's session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionProfile {
    /// `--permission-mode` value ("default", "acceptEdits", "bypassPermissions").
    pub permission_mode: Option<String>,
    /// The CLI `--tools` built-in restriction; `None` = default tool set.
    /// NOT yet wired into [`SessionSpec`] — the restriction is folded into
    /// `allowed_tools`/`disallowed_tools` instead (see module docs).
    pub tools: Option<Vec<String>>,
    /// `--allowedTools` patterns.
    pub allowed_tools: Vec<String>,
    /// `--disallowedTools` patterns (deny wins over allow).
    pub disallowed_tools: Vec<String>,
}

/// Build the permission profile for a role (plan §4.7).
///
/// `validator_commands` are the contract `command` strings for the milestone
/// under validation (ignored for non-validator roles); each becomes a
/// `Bash(<command>*)` allow. Config `allow_validator_commands` entries are
/// folded in the same way, so callers may pass either just the contract
/// commands or the full union — duplicates are removed.
///
/// `cfg.dangerously_allow_all` short-circuits every role to
/// `bypassPermissions` with empty lists (loud escape hatch, never default).
pub fn for_role(role: Role, cfg: &MissionConfig, validator_commands: &[String]) -> PermissionProfile {
    if cfg.dangerously_allow_all {
        return PermissionProfile {
            permission_mode: Some("bypassPermissions".to_string()),
            tools: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
        };
    }

    match role {
        Role::Worker => {
            let mut disallowed = to_strings(WORKER_DENY);
            for pattern in &cfg.deny_patterns {
                disallowed.push(as_tool_rule(pattern));
            }
            dedup_preserving_order(&mut disallowed);
            PermissionProfile {
                permission_mode: Some("acceptEdits".to_string()),
                tools: None,
                allowed_tools: vec!["Bash".to_string()],
                disallowed_tools: disallowed,
            }
        }

        Role::Orchestrator => {
            let mut allowed = to_strings(&["Read", "Glob", "Grep"]);
            allowed.extend(to_strings(GIT_INSPECT));
            PermissionProfile {
                permission_mode: Some("default".to_string()),
                tools: Some(to_strings(INSPECT_TOOLS)),
                allowed_tools: allowed,
                disallowed_tools: to_strings(READ_ONLY_DENY),
            }
        }

        Role::ValidatorScrutiny | Role::ValidatorFunctional => {
            let mut allowed = to_strings(&["Read", "Glob", "Grep"]);
            allowed.extend(to_strings(GIT_INSPECT));
            for command in
                validator_commands.iter().chain(cfg.allow_validator_commands.iter())
            {
                allowed.extend(command_allow_patterns(command));
            }
            dedup_preserving_order(&mut allowed);
            PermissionProfile {
                permission_mode: Some("default".to_string()),
                tools: Some(to_strings(INSPECT_TOOLS)),
                allowed_tools: allowed,
                disallowed_tools: to_strings(READ_ONLY_DENY),
            }
        }
    }
}

/// Copy a profile onto a [`SessionSpec`]. `profile.tools` is intentionally
/// not mapped: the spec (contract) has no `--tools` field, and the profiles
/// already express the restriction through allowed/disallowed patterns.
pub fn apply(profile: PermissionProfile, spec: &mut SessionSpec) {
    spec.permission_mode = profile.permission_mode;
    spec.allowed_tools = profile.allowed_tools;
    spec.disallowed_tools = profile.disallowed_tools;
}

/// Allow patterns for one contract/validator command.
///
/// A verbatim `Bash(<full command>*)` alone is far too brittle in practice
/// (observed live): a contract command `python3 extract_links.py && echo
/// EXIT_OK` never matches the validator's natural `python3 extract_links.py`,
/// and heredoc commands never re-match at all — the validator gets denied its
/// own checks, denials surface as findings, and the fix-cycle guard blocks
/// the milestone on what is really a permissions artifact.
///
/// So each command yields, besides the verbatim prefix rule:
/// - one rule per `&&` / `||` / `;` / `|` segment (trimmed, `*`-suffixed);
/// - for every segment, a leading-two-token prefix rule (`python3
///   extract_links.py*`, `python3 -m*`) so natural reinvocations and heredoc
///   forms (`python3 -`) match.
///
/// This deliberately widens what a validator may execute (e.g. `python3 -*`
/// admits arbitrary interpreter use when the contract itself runs the
/// interpreter). That is the §4.7 intent — validators run the mapped checks —
/// and the read-only guarantee continues to rest on the denied Write/Edit
/// tools and the deny list, not on Bash pattern precision.
pub fn command_allow_patterns(command: &str) -> Vec<String> {
    let command = command.trim();
    if command.is_empty() {
        return Vec::new();
    }
    let mut patterns = vec![format!("Bash({command}*)")];
    for segment in command
        .split("&&")
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split(';'))
        .flat_map(|s| s.split('|'))
    {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        patterns.push(format!("Bash({segment}*)"));
        let head: Vec<&str> = segment.split_whitespace().take(2).collect();
        if !head.is_empty() {
            patterns.push(format!("Bash({}*)", head.join(" ")));
        }
    }
    patterns
}

/// Wrap a config `deny_patterns` entry as `Bash(<pattern>)` unless it already
/// looks like a tool rule: contains `(` (e.g. `Bash(dd*)`) or exactly matches
/// a known tool name (e.g. `WebFetch`).
fn as_tool_rule(pattern: &str) -> String {
    let trimmed = pattern.trim();
    if trimmed.contains('(') || KNOWN_TOOLS.contains(&trimmed) {
        trimmed.to_string()
    } else {
        format!("Bash({trimmed})")
    }
}

fn to_strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Remove duplicates, keeping the first occurrence of each entry.
fn dedup_preserving_order(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}
