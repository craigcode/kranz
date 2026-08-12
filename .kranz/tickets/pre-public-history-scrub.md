---
state: done
state-note: verified 2026-07-24: burned serve.token expunged 2026-07-20 (SHAs 9b21172/81159c7 unreachable from any ref); full-history kranz scan run root..HEAD, all 58 findings reviewed as false positives (identifiers, env reads, fixtures, placeholders, detector canaries, pid-derived lock tokens, token plumbing) and fingerprinted in .kranz/secret-allowlist with justifications; re-scan clean (exit 0, 'secret scan passed', demonstrably non-vacuous). operator-gates.md box flipped [x].
title: Scrub the burned serve.token (and audit for other secrets) from git history before going public
priority: 2
schedule: once
---

## Goal
Run the pre-public history scrub tracked as a human gate in
`docs/operator-gates.md`: a live `serve.token` was committed and pushed in
`9b21172`/`81159c7`, so the remote history contains a real credential.
Procedure: scan the full history (`kranz scan --range <root>..HEAD`, plus an
external scanner if D-B of docs/scoping/secret-scanning.md has landed),
review every hit against `.kranz/secret-allowlist`, rewrite history
(git-filter-repo) to expunge the token, coordinate the force-push, and rotate
any credential that could still be valid. Only then flip the operator gate.

## Context
Human gates are deliberately not kranz-automatable, but this one is easy to
lose track of — it lives only as an unchecked box in operator-gates.md, and
it hard-blocks "repo goes public" (which itself gates crates.io/Homebrew per
roadmap M4). The scan tooling is the repo's own (scrub.rs + `kranz scan`);
the merge path already reads the allowlist from the pinned base so the
rewritten history cannot quietly re-waive a real secret. Note the asymmetry
this creates: rewriting history invalidates every open clone/fork — schedule
it when disruption is minimal.

## Acceptance hints
- Full-history scan output attached to the mission report; every finding
  either expunged or fingerprinted in `.kranz/secret-allowlist` with a
  reviewed justification.
- `9b21172`/`81159c7` no longer reachable from any pushed ref; the burned
  token confirmed rotated/invalid.
- operator-gates.md "repo public + history scrub" box flipped to [x].
