# Research — m-c5cd5f

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/signals/sentry.rs
- crates/engine/src/signals/source.rs
- crates/engine/src/signals/triage.rs
- crates/engine/src/config.rs
- crates/engine/src/hooks.rs
- crates/server/src/hooks.rs
- crates/server/tests/hooks_test.rs
- crates/engine/tests/signal_intake_test.rs
- crates/engine/tests/config_trust_test.rs
- docs/scoping/signal-intake.md
- AGENTS.md

## External sources

- 2026-11-10 code review items (as quoted in the mission goal)
- Operator decision 2026-11-13 on replay retention (mission goal)

## Facts

- The Sentry signature covers the raw body only; the replay key comes from unsigned Request-ID/X-Sentry-Delivery, with a fallback to the hash of the signature header as sent — `crates/engine/src/signals/sentry.rs:128-143`
- ReplayCache keeps entries by the sender-supplied timestamp and evicts them after the 300s skew window — `crates/engine/src/signals/source.rs:87-118`
- The server keeps one process-wide static Sentry replay cache, so body-keyed replay makes identical test bodies collide across tests — `crates/server/src/hooks.rs:31-34`
- An existing test re-sends the same body with a new request-id and expects it accepted as a distinct delivery — `crates/engine/tests/signal_intake_test.rs:340-354`
- SignalsConfig::load replaces the whole sentry block from the project layer and bypasses CONFIG_TRUST_RULES — `crates/engine/src/signals/triage.rs:130-168`
- Precedents: hooks.secret is deliberately project-settable; hooks.autoQueue is refused from the project layer by a dedicated loader — `crates/engine/src/config.rs:463-470; crates/engine/src/hooks.rs:111-144`
- The server test fixture sets signals.sentry.allowedHosts in the project layer — `crates/server/tests/hooks_test.rs:774-777`

## Ambiguities & stale docs

- Operator did not answer whether allowedHosts is global-only; the plan takes the recommended default (global-only, since it bounds write-back hosts)
- Operator did not answer whether signals.sentry.secret stays project-settable; the plan keeps it settable per the hooks.secret precedent
- Refuse versus ignore for project-layer global-only fields: the plan refuses (load_hooks precedent), which breaks any repository config that sets them
- GitHubSignalSource::verify has the same unsigned x-github-delivery key shape but is not on the live GitHub route; left out of scope as a follow-up
- The ledger's informational delivery_id still records the unsigned Request-ID; replay protection no longer depends on it

## Candidate knowledge updates

- docs/knowledge/decisions: record that every config loader reading raw JSON (hooks::load_hooks, SignalsConfig::load) must enforce project-layer refusals itself, because CONFIG_TRUST_RULES only covers config::load
