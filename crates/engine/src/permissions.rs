//! Role → permission profile mapping (plan §4.7; the table in
//! `docs/design.md` is authoritative).
//!
//! Deny rules take precedence over allows in Claude Code, and in `-p` mode a
//! tool call outside the allowed set simply fails with an error the model can
//! read — that failure IS the read-only guarantee for orchestrator and
//! validators, backstopped for validators by the `validator.tamper`
//! HEAD/index/worktree identity assertion around every session
//! ([`crate::validator_integrity`]): a validator that slips a write past the
//! patterns fails its round honestly. The profiles here express every
//! restriction through `permission_mode` + `allowed_tools` +
//! `disallowed_tools`: the CLI `--tools` built-in restriction is recorded on
//! the profile for future wiring, but [`apply`] does not map it —
//! [`SessionSpec`] carries a separate `tools` field populated straight from
//! per-role config (see [`MissionConfig`]), by design left disconnected from
//! this read-only profile field.

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

/// Deny rules protecting the consent substrate, applied to EVERY role
/// (audit 2026-09-01 C1 / H6).
///
/// Two things must stay out of reach of an agent session:
///
/// - `~/.kranz/**` holds the repository authority key that signs control
///   files and MACs `events.jsonl`, plus `serve.token`. A session that can
///   read the key can forge operator consent; one that can write there can
///   substitute a key of its own.
/// - `.kranz/missions/**/control/**` is the control inbox itself. Signature
///   checking already refuses forged files, but a session with no business
///   writing there should not be able to fill the inbox with quarantine
///   fodder, and the deny rule states the boundary where a reader will look
///   for it.
///
/// This is the barrier under the DEFAULT posture, where `sandbox.enforce` is
/// `off` and there is no OS containment at all. It is the agent CLI enforcing
/// its own rules, so it is weaker than the sandbox's read-deny set and does
/// not survive `dangerouslyAllowAll`, which short-circuits every profile to
/// `bypassPermissions` by design.
const AUTHORITY_DENY: &[&str] = &[
    "Read(~/.kranz/**)",
    "Edit(~/.kranz/**)",
    "Write(~/.kranz/**)",
    "Read(.kranz/missions/**/control/**)",
    "Edit(.kranz/missions/**/control/**)",
    "Write(.kranz/missions/**/control/**)",
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
const READ_ONLY_DENY: &[&str] = &[
    "Write",
    "Edit",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "Bash(git push*)",
];

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
/// `grants` are the plan-level `Mission.command_grants` — commands the plan
/// itself has authorized. Each becomes a `Bash(<grant>*)` allow (via
/// [`command_allow_patterns`]) folded into BOTH the worker profile (alongside
/// the worker's existing bare `Bash` allow) and the validator profiles
/// (alongside `validator_commands`), so a command the plan grants is runnable
/// by the worker AND re-runnable by the validators verifying it — one source
/// of truth for both surfaces. The orchestrator role ignores `grants`.
///
/// `cfg.dangerously_allow_all` short-circuits every role to
/// `bypassPermissions` with empty lists (loud escape hatch, never default).
pub fn for_role(
    role: Role,
    cfg: &MissionConfig,
    validator_commands: &[String],
    grants: &[String],
    deny_exceptions: &[String],
) -> PermissionProfile {
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
            // Subtract operator-lifted rules (WorkerDeny grants). Exact-match
            // removal: the event log names the precise rule lifted, and only
            // that rule leaves the worker deny set — a deliberate, auditable
            // erosion of the guardrail. Applied AFTER dedup so a lifted rule is
            // gone whether it came from WORKER_DENY or config deny_patterns.
            if !deny_exceptions.is_empty() {
                disallowed.retain(|rule| !deny_exceptions.contains(rule));
            }
            // AFTER the lift: a `WorkerDeny` grant is an operator decision
            // about a shell command, and must never be able to hand a session
            // the authority key that signs the operator's own approvals.
            disallowed.extend(authority_deny());
            let mut allowed = vec!["Bash".to_string()];
            for grant in grants {
                allowed.extend(command_allow_patterns(grant));
            }
            dedup_preserving_order(&mut allowed);
            PermissionProfile {
                permission_mode: Some("acceptEdits".to_string()),
                tools: None,
                allowed_tools: allowed,
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
                disallowed_tools: read_only_deny(),
            }
        }

        Role::ValidatorScrutiny | Role::ValidatorFunctional => {
            let mut allowed = to_strings(&["Read", "Glob", "Grep"]);
            allowed.extend(to_strings(GIT_INSPECT));
            // Read-only env introspection (ticket validator-env-reads-no-grant;
            // m-83d1ed's ms-2 blocked on a printenv park): exactly
            // `printenv <KRANZ_*>` — never bare `printenv` or `env` (a full
            // dump writes the injected backend auth key into the otherwise
            // sanitized transcript, and `env <cmd>` is a command runner),
            // never `echo` (command substitution). The KRANZ_ prefix IS the
            // disclosure boundary: those vars are engine-injected.
            allowed.push("Bash(printenv KRANZ_*)".to_string());
            // Scrutiny/mechanical split: only the functional validator runs
            // the contract/validator commands. Scrutiny inspects the range
            // read-only (Read/Grep/Glob + plain git) and is neither
            // advertised nor permitted the cargo gates — see the per-role
            // task in runner::run_validator_in. Operator grants still apply
            // to both roles (a grant is an explicit operator decision).
            if role == Role::ValidatorFunctional {
                for command in validator_commands
                    .iter()
                    .chain(cfg.allow_validator_commands.iter())
                {
                    allowed.extend(command_allow_patterns(command));
                }
            }
            for command in grants {
                allowed.extend(command_allow_patterns(command));
            }
            if role == Role::ValidatorFunctional {
                // Live-QA mode (functional only): a browser/computer-use tool
                // configured in `--tools` still needs auto-approval in `-p`
                // mode or every call fails (docs/design.md §4.7). The
                // standard inspect tools are excluded because they are
                // already governed by the precise patterns above — folding
                // a bare `Bash` in here would broaden it to any command.
                for tool in &cfg.validator_functional.tools {
                    if !INSPECT_TOOLS.contains(&tool.as_str()) {
                        allowed.push(tool.clone());
                    }
                }
            }
            dedup_preserving_order(&mut allowed);
            PermissionProfile {
                permission_mode: Some("default".to_string()),
                tools: Some(to_strings(INSPECT_TOOLS)),
                allowed_tools: allowed,
                disallowed_tools: read_only_deny(),
            }
        }
    }
}

/// Copy a profile onto a [`SessionSpec`]. `profile.tools` is intentionally
/// not mapped here: `spec.tools` is populated separately from per-role
/// config (opt-in `--tools` allow-list), while the profiles already express
/// this read-only restriction through allowed/disallowed patterns.
pub fn apply(profile: PermissionProfile, spec: &mut SessionSpec) {
    spec.permission_mode = profile.permission_mode;
    spec.allowed_tools = profile.allowed_tools;
    spec.disallowed_tools = profile.disallowed_tools;
}

/// Allow patterns for one contract/validator command: the exact command forms
/// the contract declares — the verbatim command and each of its
/// `&&` / `||` / `;` / `|` segments, each as a `Bash(<form>*)` prefix rule.
/// Prefix-suffix matching still admits the natural reinvocations that made
/// verbatim-only rules untenable (observed live): `python3 extract_links.py`
/// matches the segment rule from `python3 extract_links.py && echo EXIT_OK`,
/// and a trailing extra flag matches the declared prefix.
///
/// Nothing wider (ticket `validator-immutability-proof`, review P1 #5). The
/// old leading-two-token rule widened `python3 -m pytest` to
/// `Bash(python3 -m*)` and heredoc contracts to `Bash(python3 -*)` —
/// arbitrary interpreter use (`python3 -c '<any write>'`) under a "read-only"
/// role. A validator needs to RUN the declared commands, nothing else, and
/// engine-run contract commands (validation_round's captured PASS/FAIL
/// evidence) mean heredoc forms need no validator Bash rule at all. The
/// read-only guarantee now rests on the denied Write/Edit tools, the deny
/// list, and the `validator.tamper` identity assertion
/// ([`crate::validator_integrity`]) — with Bash precision no longer working
/// against it.
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
    }
    patterns
}

/// The worker deny rule that blocks `command`, if any — used to name the rule a
/// `WorkerDeny` grant would lift. Best-effort: parses `Bash(<pattern>*)` rules
/// and prefix-matches the command against `<pattern>`. Non-`Bash(...)` rules
/// (WebFetch/WebSearch/tool names) never match a shell command.
///
/// When several rules match, returns the MOST SPECIFIC (longest-prefix) one —
/// so a narrow config rule (`Bash(git push --force*)`) is offered over the broad
/// built-in (`Bash(git push*)`), keeping the operator's lift as narrow as the
/// rule that actually blocked the command. A wrong or absent match just means
/// the grant offers the wrong/no rule and the command stays denied (cap-bounded);
/// the authoritative enforcement is Claude Code removing the exact rule string
/// from `disallowed_tools`.
pub fn matching_deny_rule(command: &str, deny_rules: &[String]) -> Option<String> {
    let cmd = command.trim();
    deny_rules
        .iter()
        .filter_map(|rule| {
            let pat = rule
                .strip_prefix("Bash(")
                .and_then(|r| r.strip_suffix(')'))?;
            let prefix = pat.strip_suffix('*').unwrap_or(pat);
            (!prefix.is_empty() && cmd.starts_with(prefix)).then_some((rule, prefix.len()))
        })
        .max_by_key(|(_, len)| *len)
        .map(|(rule, _)| rule.clone())
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

/// The read-only roles' deny set with [`AUTHORITY_DENY`] folded in. The
/// read-only roles already deny `Write`/`Edit` outright, so the authority
/// rules add the `Read` half: an orchestrator or validator has no business
/// reading the key that signs the operator's approvals either.
fn read_only_deny() -> Vec<String> {
    let mut deny = to_strings(READ_ONLY_DENY);
    deny.extend(authority_deny());
    deny
}

/// [`AUTHORITY_DENY`] plus the same rules in ABSOLUTE form for the global
/// kranz directory that actually resolves at runtime. `~` in a rule relies
/// on the agent CLI expanding it, and `KRANZ_HOME` moves the key directory
/// somewhere `~/.kranz/**` never covers; the absolute rule follows
/// `paths::global_kranz_dir` so the deny and the key writer cannot drift
/// (follow-up review F-9 and F-17). Backends without a permission-rule
/// plane (codex, droid) never see these rules; there the OS sandbox is the
/// only barrier, and the module docs say so.
pub fn authority_deny() -> Vec<String> {
    let mut deny = to_strings(AUTHORITY_DENY);
    if let Some(global) = crate::paths::global_kranz_dir() {
        let global = global.to_string_lossy().replace('\\', "/");
        let global = global.trim_end_matches('/');
        for tool in ["Read", "Edit", "Write"] {
            deny.push(format!("{tool}(/{global}/**)"));
        }
    }
    dedup_preserving_order(&mut deny);
    deny
}

fn to_strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Remove duplicates, keeping the first occurrence of each entry.
fn dedup_preserving_order(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit 2026-09-01 C1/H6: every role denies the authority material, and
    /// the deny survives the one operator lever that edits the deny list.
    #[test]
    fn every_role_denies_the_authority_key_and_the_control_inbox() {
        let cfg = MissionConfig::default();
        for role in [
            Role::Worker,
            Role::Orchestrator,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ] {
            let profile = for_role(role, &cfg, &[], &[], &[]);
            for rule in AUTHORITY_DENY {
                assert!(
                    profile.disallowed_tools.iter().any(|r| r == rule),
                    "{role:?} must deny `{rule}`: {:?}",
                    profile.disallowed_tools
                );
            }
        }

        // A WorkerDeny grant lifts the rule it names, never the authority
        // rules: an operator approving `git push` must not hand the session
        // the key that signs their own approvals.
        let lifted = for_role(
            Role::Worker,
            &cfg,
            &[],
            &[],
            &[
                "Bash(git push*)".to_string(),
                "Read(~/.kranz/**)".to_string(),
            ],
        );
        assert!(!lifted
            .disallowed_tools
            .iter()
            .any(|r| r == "Bash(git push*)"));
        assert!(
            lifted
                .disallowed_tools
                .iter()
                .any(|r| r == "Read(~/.kranz/**)"),
            "the authority deny is not liftable: {:?}",
            lifted.disallowed_tools
        );
    }

    /// The escape hatch stays an escape hatch: `dangerouslyAllowAll` clears
    /// every list including this one, and the docs say so. Pinned so the
    /// interaction is a decision on the record rather than an oversight.
    #[test]
    fn dangerously_allow_all_still_clears_the_authority_deny() {
        let cfg = MissionConfig {
            dangerously_allow_all: true,
            ..MissionConfig::default()
        };
        let profile = for_role(Role::Worker, &cfg, &[], &[], &[]);
        assert!(profile.disallowed_tools.is_empty());
        assert_eq!(
            profile.permission_mode.as_deref(),
            Some("bypassPermissions")
        );
    }

    #[test]
    fn worker_deny_exceptions_lift_exactly_the_named_rule() {
        let cfg = MissionConfig::default();
        // Baseline: git push is denied.
        let base = for_role(Role::Worker, &cfg, &[], &[], &[]);
        assert!(base.disallowed_tools.iter().any(|r| r == "Bash(git push*)"));

        // Lifting `Bash(git push*)` removes exactly that rule; the other rails
        // (sudo, curl, …) stay in force.
        let lifted = for_role(
            Role::Worker,
            &cfg,
            &[],
            &[],
            &["Bash(git push*)".to_string()],
        );
        assert!(!lifted
            .disallowed_tools
            .iter()
            .any(|r| r == "Bash(git push*)"));
        assert!(lifted.disallowed_tools.iter().any(|r| r == "Bash(sudo*)"));
        assert!(lifted.disallowed_tools.iter().any(|r| r == "Bash(curl*)"));
    }

    #[test]
    fn matching_deny_rule_maps_a_command_to_the_rule_that_blocks_it() {
        let deny = to_strings(WORKER_DENY);
        assert_eq!(
            matching_deny_rule("git push origin main", &deny).as_deref(),
            Some("Bash(git push*)")
        );
        assert_eq!(
            matching_deny_rule("sudo rm -rf /", &deny).as_deref(),
            Some("Bash(sudo*)")
        );
        // A command no deny rule blocks maps to nothing.
        assert_eq!(matching_deny_rule("cargo build", &deny), None);
        // Non-Bash rules (WebFetch/WebSearch/tool names) never match a command.
        assert_eq!(
            matching_deny_rule("anything at all", &["WebFetch".to_string()]),
            None
        );
        // Most specific wins: a narrower config rule is offered over the broad
        // built-in, so the operator's lift stays as narrow as what blocked it.
        let mixed = vec![
            "Bash(git push*)".to_string(),
            "Bash(git push --force*)".to_string(),
        ];
        assert_eq!(
            matching_deny_rule("git push --force origin main", &mixed).as_deref(),
            Some("Bash(git push --force*)")
        );
    }

    #[test]
    fn grants_reach_worker_and_validator() {
        let cfg = MissionConfig::default();
        let grants = vec!["gc lint".to_string()];

        // Granting a base command also covers `<cmd> --help`: the pattern is
        // a `*`-suffixed prefix match, not a verbatim match.
        assert!(command_allow_patterns("gc lint").contains(&"Bash(gc lint*)".to_string()));

        let worker = for_role(Role::Worker, &cfg, &[], &grants, &[]);
        assert!(worker.allowed_tools.contains(&"Bash(gc lint*)".to_string()));
        // Grants are additive: the bare worker Bash allow must survive.
        assert!(worker.allowed_tools.contains(&"Bash".to_string()));
        assert_eq!(worker.permission_mode, Some("acceptEdits".to_string()));

        let validator = for_role(Role::ValidatorScrutiny, &cfg, &[], &grants, &[]);
        assert!(validator
            .allowed_tools
            .contains(&"Bash(gc lint*)".to_string()));
    }

    /// Narrowed widening (ticket `validator-immutability-proof`): only the
    /// verbatim command and its exact shell segments become allow rules — no
    /// leading-two-token catch-alls like `python3 -*` / `python3 -m*`.
    #[test]
    fn command_allow_patterns_stick_to_the_declared_command_forms() {
        // Verbatim + exact segments, nothing wider.
        assert_eq!(
            command_allow_patterns("python3 extract_links.py && echo EXIT_OK"),
            vec![
                "Bash(python3 extract_links.py && echo EXIT_OK*)".to_string(),
                "Bash(python3 extract_links.py*)".to_string(),
                "Bash(echo EXIT_OK*)".to_string(),
            ]
        );
        // No binary/flag catch-alls: neither the heredoc form's `python3 -*`
        // nor a `-m` widening survives.
        let heredoc = command_allow_patterns("python3 - <<'PY'\nprint('ok')\nPY");
        assert!(!heredoc.iter().any(|p| p == "Bash(python3 -*)"));
        let module = command_allow_patterns("python3 -m pytest test_x.py -v");
        assert!(!module.iter().any(|p| p == "Bash(python3 -m*)"));
        assert!(module.contains(&"Bash(python3 -m pytest test_x.py -v*)".to_string()));

        // And the functional validator's profile carries the exact form only:
        // `cargo test --workspace x` must not widen to `cargo test*`.
        let cfg = MissionConfig::default();
        let profile = for_role(
            Role::ValidatorFunctional,
            &cfg,
            &["cargo test --workspace x".to_string()],
            &[],
            &[],
        );
        assert!(profile
            .allowed_tools
            .contains(&"Bash(cargo test --workspace x*)".to_string()));
        assert!(!profile
            .allowed_tools
            .iter()
            .any(|p| p == "Bash(cargo test*)"));
        assert!(!profile.allowed_tools.iter().any(|p| p == "Bash(cargo*)"));
    }

    /// Ticket validator-env-reads-no-grant (m-83d1ed): validators get
    /// exactly `printenv KRANZ_*` — never bare printenv/env/echo forms.
    #[test]
    fn validator_env_reads_allow_kranz_printenv_only() {
        let cfg = MissionConfig::default();
        for role in [Role::ValidatorFunctional, Role::ValidatorScrutiny] {
            let profile = for_role(role, &cfg, &[], &[], &[]);
            assert!(
                profile
                    .allowed_tools
                    .contains(&"Bash(printenv KRANZ_*)".to_string()),
                "{role:?} must allow printenv of KRANZ_ vars"
            );
            for poisoned in [
                "Bash(printenv*)",
                "Bash(env*)",
                "Bash(echo*)",
                "Bash(echo *)",
            ] {
                assert!(
                    !profile.allowed_tools.iter().any(|p| p == poisoned),
                    "{role:?} must NOT allow {poisoned} (auth-key dump / command runner / substitution)"
                );
            }
        }
    }

    #[test]
    fn validator_allowlist_includes_contract_and_worker_commands() {
        let cfg = MissionConfig::default();
        let contract_commands = vec!["cargo test".to_string()];
        let worker_commands = vec!["gc lint".to_string()];

        let mut combined = contract_commands.clone();
        for command in &worker_commands {
            if !combined.contains(command) {
                combined.push(command.clone());
            }
        }

        // Scrutiny/mechanical split: contract/worker commands fold into the
        // functional validator's allow-list only; scrutiny stays read-only.
        let functional = for_role(Role::ValidatorFunctional, &cfg, &combined, &[], &[]);
        assert!(functional
            .allowed_tools
            .contains(&"Bash(cargo test*)".to_string()));
        assert!(functional
            .allowed_tools
            .contains(&"Bash(gc lint*)".to_string()));

        let scrutiny = for_role(Role::ValidatorScrutiny, &cfg, &combined, &[], &[]);
        assert!(!scrutiny
            .allowed_tools
            .contains(&"Bash(cargo test*)".to_string()));
        assert!(!scrutiny
            .allowed_tools
            .contains(&"Bash(gc lint*)".to_string()));
    }

    /// Composition audit (ticket `config-fail-open-audit`): config
    /// `deny_patterns` must EXTEND the built-in worker deny list, never
    /// replace it. This is the audit's most dangerous replace-shaped
    /// regression target: if a custom list ever displaced WORKER_DENY, the
    /// worker could push, publish, sudo, and open raw network sockets while
    /// the operator believed the §4.7 rails were still on.
    #[test]
    fn composition_audit_config_deny_patterns_extend_never_replace_builtin_worker_deny() {
        let cfg = MissionConfig {
            deny_patterns: vec!["rm -rf *".to_string(), "TodoWrite".to_string()],
            ..MissionConfig::default()
        };
        let profile = for_role(Role::Worker, &cfg, &[], &[], &[]);
        for builtin in WORKER_DENY {
            assert!(
                profile.disallowed_tools.iter().any(|r| r == builtin),
                "built-in worker deny {builtin} must survive a custom deny_patterns list"
            );
        }
        // The custom entries ride alongside (wrapped as tool rules as needed).
        assert!(profile
            .disallowed_tools
            .iter()
            .any(|r| r == "Bash(rm -rf *)"));
        assert!(profile.disallowed_tools.iter().any(|r| r == "TodoWrite"));
    }

    /// Composition audit: plan command grants add ALLOWS only — they never
    /// lift a deny rule. Deny precedence in Claude Code (deny rules win over
    /// allows) is what makes this safe: a granted command that still matches
    /// a deny rule stays denied until the operator-approved WorkerDeny grant
    /// lifts the exact rule (deny_exceptions, event-logged). A regression
    /// that lets a grant silently erode the deny list fails here.
    #[test]
    fn composition_audit_grants_add_allows_without_lifting_deny() {
        let cfg = MissionConfig::default();
        let grants = vec!["git push".to_string()];
        let profile = for_role(Role::Worker, &cfg, &[], &grants, &[]);
        // The grant's allow pattern is present...
        assert!(profile
            .allowed_tools
            .contains(&"Bash(git push*)".to_string()));
        // ...but the matching deny rule is NOT removed by the allow, so deny
        // precedence keeps the command blocked at enforcement time.
        assert!(profile
            .disallowed_tools
            .contains(&"Bash(git push*)".to_string()));
    }

    /// Composition audit: `bypassPermissions` is reachable ONLY through the
    /// dangerously-named config key — the naming rule's one escape valve.
    /// No other config shape (custom deny lists, grants, validator command
    /// allows) may flip a role into bypass mode.
    #[test]
    fn composition_audit_bypass_permissions_requires_the_dangerously_named_key() {
        let roles = [
            Role::Worker,
            Role::Orchestrator,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ];
        let mut cfg = MissionConfig {
            deny_patterns: vec!["sudo".to_string()],
            allow_validator_commands: vec!["anything at all".to_string()],
            ..MissionConfig::default()
        };
        for role in roles {
            let profile = for_role(
                role,
                &cfg,
                &["cargo test".to_string()],
                &["git push".to_string()],
                &[],
            );
            assert_ne!(
                profile.permission_mode.as_deref(),
                Some("bypassPermissions"),
                "{role:?} must never reach bypassPermissions without the dangerous key"
            );
        }
        cfg.dangerously_allow_all = true;
        for role in roles {
            let profile = for_role(role, &cfg, &[], &[], &[]);
            assert_eq!(
                profile.permission_mode.as_deref(),
                Some("bypassPermissions"),
                "{role:?}: the dangerously-named key is the sanctioned escape valve"
            );
            assert!(profile.disallowed_tools.is_empty());
            assert!(profile.allowed_tools.is_empty());
        }
    }
}
