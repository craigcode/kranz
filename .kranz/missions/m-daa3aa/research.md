# Research — m-daa3aa

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- .github/workflows/release.yml
- .github/workflows/ci.yml
- .github/actions-allowlist.json
- .github/dependabot.yml
- scripts/check-actions-allowlist.py
- scripts/check-macos-ci.sh
- crates/cli/src/exec.rs
- crates/cli/src/cli.rs
- crates/engine/src/git_ops.rs
- docs/public-readiness.md
- docs/deploy.md
- AGENTS.md
- CHANGELOG.md
- .kranz/tickets/hardening-release-and-push.md

## External sources

- GitHub REST API 'Set allowed actions and reusable workflows for a repository' (github_owned_allowed / verified_allowed / patterns_allowed), as referenced by docs/public-readiness.md

## Facts

- The artifact-producing build job restores Swatinem/rust-cache before cargo build --release — `.github/workflows/release.yml:267,291`
- The verify job restores rust-cache and setup-node's npm cache — `.github/workflows/release.yml:43,71-77`
- The allowlist checker accepts any 40-hex commit of actions/* or github/* when github_owned_allowed is true, which the committed allowlist sets — `scripts/check-actions-allowlist.py:213-214; .github/actions-allowlist.json:2`
- Third-party allowlist entries are @* wildcards matched with fnmatch — `.github/actions-allowlist.json:5-9; scripts/check-actions-allowlist.py:216`
- The allowlist JSON mirrors GitHub's repository selected-actions setting and is applied there by the operator — `docs/public-readiness.md:121-124`
- exec checks the --push remote only after run(); all remote checks live in push_mission_branch — `crates/cli/src/exec.rs:191-329,446-451; crates/engine/src/git_ops.rs:2288-2337`
- A read-only network probe pattern exists (ls-remote via probe_network) — `crates/engine/src/git_ops.rs:2215-2247`
- No existing test name contains push_preflight — `grep 'fn [a-z_]*push_preflight' crates/ -> no matches`
- Contract files are events.rs, types.rs, backend.rs, error.rs (additive-only) — `AGENTS.md:75-77`

## Ambiguities & stale docs

- Planning questions went unanswered; the plan uses the stated defaults: exact commits for third-party actions too, a cacheless whole release workflow (verify job included), and a preflight of offline checks plus read-only ls-remote with exit code 4
- The ls-remote preflight proves reachability and read access, not write permission; a remote that is read-only for the credential still fails only at push time
- Re-applying the allowlist to the GitHub repository setting is an operator step outside the mission (no network)
- Each Dependabot github-actions bump will now fail the supply-chain job until the new commit is added to the allowlist (intended review step)

## Candidate knowledge updates

- docs/knowledge/architecture/mission-pipeline.md: kranz exec --push preflights the remote (GitRepo::preflight_push_remote) before MissionEngine::create; refusal exits 4 with no mission
- docs/knowledge/decisions/inviolable-invariants.md: release workflow restores no cache (guarded by scripts/check-release-workflow.py); Actions allowlist lists exact reviewed commits only
