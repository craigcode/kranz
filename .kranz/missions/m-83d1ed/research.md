# Research — m-83d1ed

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- docs/scoping/beads-workstore.md
- .kranz/tickets/beads-bridge-translator-correctness.md
- .kranz/tickets/beads-bridge-provenance-return-path.md
- .kranz/tickets/queue-liveness-over-age.md
- packaging/gascity/bin/kranz-dispatch
- packaging/gascity/bin/kranz-run-bead
- packaging/gascity/bin/kranz-city-worker
- packaging/gascity/pack.toml
- packaging/gascity/orders/kranz-dispatch.toml
- packaging/gascity/agents/kranz-worker/agent.toml
- docs/gascity.md
- AGENTS.md
- .kranz/merge-gates.json
- .kranz/lessons/m-73ada5.md
- .kranz/lessons/m-b66d34.md
- .kranz/lessons/m-7820b9.md
- .kranz/lessons/m-fe212a.md
- crates/engine/src/command_exec.rs
- scripts/check-macos-ci.sh
- .claude/settings.local.json

## External sources

- docs/scoping/beads-workstore.md D-BW-2 (accepted 2026-07-29)
- github.com/gastownhall/beads (upstream, per scoping doc citations)

## Facts

- The stale `set-state` verb survives in exactly ONE place, not throughout the spike: kranz-run-bead line 25. kranz-dispatch already uses the correct `gc bd update --status in_progress` form at line 75. — `packaging/gascity/bin/kranz-run-bead:25 and packaging/gascity/bin/kranz-dispatch:75`
- There is no claim mechanism at all in the bridge today — the ticket's framing of a claim path 'with no lease semantics' understates it. Dispatch reads ready beads then flips status, which is not first-wins, so two dispatchers can both spool the same bead. — `packaging/gascity/bin/kranz-dispatch:43 (`gc bd ready --label`) and :75 (`gc bd update --status in_progress`); no `--claim` anywhere under packaging/gascity/bin/`
- The dispatcher structurally cannot heartbeat its own claim: `gc order exec` enforces a context deadline that killed a mid-mission dispatcher at ~60s, so dispatch must spool and exit. The long-lived processes are kranz-city-worker (15s drain loop) and kranz-run-bead (per-mission). — `docs/gascity.md:24-30; packaging/gascity/orders/kranz-dispatch.toml (trigger=cooldown, interval=5m)`
- A fixture Gas City must never be created by an autonomous test: `gc init` registers a machine-wide launchd supervisor and immediately spawns a live `claude --dangerously-skip-permissions --effort max` session, and `gc stop` has been observed to orphan tmux servers twice. — `docs/gascity.md:48-53`
- A live round-trip gate is achievable rather than mock-only: both `bd` and `gc` resolve on this host at /opt/homebrew/bin, and PATH is on the env-clear allowlist so workers and validators in throwaway snapshots can reach them. — ``type gc bd` resolved both to /opt/homebrew/bin; crates/engine/src/command_exec.rs:352 (PATH in allowlist) and :553 (`assert!(dump.contains("PATH="))`)`
- Contract assertion commands DO run through a real shell (`sh -c` / `cmd /C`), so pipes and greps are valid; the `!`-negation trap from lesson m-73ada5 comes from the PREFLIGHT PATH probe reading the leading token, not from runtime argv splitting. — `crates/engine/src/command_exec.rs:73-78 ('DELIBERATE shell usage (the one place in the engine)'); crates/engine/src/preflight.rs:422 (leading-program-token extraction for PATH lookup)`
- Shell scripts under packaging/ are entirely ungated today — merge-gates.json runs only cargo (fmt/clippy/test) and the apps/dashboard npm gates. Nothing would catch bridge-script rot. — `.kranz/merge-gates.json`
- This mission touches no Rust or dashboard source, so a scope assertion must cover BOTH crates and apps per lesson m-6a20dc, and must diff against $KRANZ_BASE_SHA rather than a bare branch name per the m-c9c915 lesson. — `AGENTS.md layout section; .kranz/lessons index`

## Ambiguities & stale docs

- The ticket's design item 3 ('acceptance-criteria array unwrapping kept, now covered by test') appears to misread the code. kranz-dispatch:59-61's `if type == "array" then .[0]` guard unwraps the OUTER `bd show --json` envelope, not the criteria value; `.acceptance_criteria // ""` performs no array unwrapping at all. Treated as a latent bug and brought into scope, since raw JSON leaking into `## Acceptance hints` is precisely the brief-corruption failure docs/gascity.md:31 records.
- The ticket's own test gate specifies `! grep -R 'set-state' packaging/gascity/bin/`, which trips lesson m-73ada5 — preflight probes the leading token and reports `'!' not found on PATH`. Replaced with a committed check script whose exit code carries the negation.
- The ticket requires the round-trip test to 'skip with a clear note when bd is absent', but a skip exits 0 and would read green as a contract assertion. Resolved with PASS/SKIP marker discipline: the contract greps the PASS marker so a skip fails the gate while humans and CI still see a clean skip.
- The ticket's status map lists four bead statuses against four kranz states, but the script has four exit-code paths of which TWO (exit 3 underspecified, exit 1/other failed) both return the bead to `open`. The map is many-to-one; specced to be documented as such rather than forced into a false bijection.
- UNRESOLVED and deliberately absorbed by milestone 1: whether the installed `bd` supports `bd update --claim` and any lease/TTL/heartbeat mechanism. This session's Bash permissions denied `bd --help`, and routing around that via the allowlisted `python3` interpreter would evade a permission boundary. Milestone 3 therefore carries a hard precondition to block rather than invent a substitute.
- The scoping doc flags the gc-vendored `bd` dialect as unverified (the spike speaks `gc bd`, not upstream `bd`). Milestone 1 records whether `gc bd <sub> --help` is a faithful pass-through; the mitigation rests on docs/gascity.md:45, which records that gc's wrapper aborts on unverifiable args rather than substring-resolving.

## Candidate knowledge updates

- docs/knowledge/surfaces/gascity-bridge.md — the bidirectional bead-status/kranz-state map, the dispatch-must-exit-fast constraint, and where the claim heartbeat lives, so a future planner does not rediscover the order-deadline architecture from docs/gascity.md's lesson list.
- docs/knowledge/validation/gates.md — add the marker-discipline pattern for skippable live-binary tests: a script that skips cleanly must emit a distinct PASS marker only on a real run, and the contract must grep the marker, because exit-0-on-skip is a vacuous gate.
- docs/knowledge/operations/gascity-hazards.md — `gc init` registers a machine-wide launchd supervisor and spawns a live max-effort Claude session; `gc stop` orphans tmux servers. Never permit either in autonomous test setup.
