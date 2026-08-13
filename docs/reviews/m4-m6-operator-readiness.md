# M4 distribution and M6 cloud operator readiness

Date: 2026-08-12

## Outcome

The repository is ready for the two remaining human decisions, but neither
irreversible action should be performed silently from a development branch.
M4 needs the owner to make the GitHub repository public and authorize one
version-aligned release. M6 needs a Railway account/project, bounded spend,
and runtime credentials. The implementation work that can safely precede
those decisions is either shipped or ticketed below.

## M4 — public distribution

Verified state:

- the GitHub repository is private;
- the pre-public history rewrite and full-history secret review are complete;
- GitHub release `v0.1.0` exists with Linux x86_64, macOS arm64, and Windows
  x86_64 assets, but its tag predates substantial current development;
- crates.io names `kranz`, `kranz-engine`, `kranz-server`, and `kranz-slack`
  are reserved by 0.0.1 placeholder packages;
- no `craigcode/homebrew-kranz` tap exists; and
- `packaging/homebrew/kranz.rb` is a skeleton for v0.1.0 with an all-zero
  digest and obsolete single-backend wording.

Publishing current source as crates.io 0.1.0 would make it disagree with the
existing v0.1.0 tag and binary assets. The recommended next line is **0.2.0**:
one source commit, one tag, four crates, three binary assets, the Tauri
version, and the Homebrew formula must all agree.

Operator sequence after approving public visibility:

1. Change repository visibility to public and verify anonymous clone, README
   links, branch protections, issue settings, and the rewritten public history.
2. Freeze the release commit and bump root workspace/package dependency,
   Tauri, and formula versions together to 0.2.0.
3. Run every workspace/dashboard/security gate plus `cargo publish --dry-run`
   for all four crates.
4. Tag and push `v0.2.0`; require all three release builds and asset uploads to
   pass before writing release notes or publishing packages.
5. Publish bottom-up: engine; server and Slack; CLI. Confirm each index entry
   resolves before publishing its dependants.
6. Create the public `craigcode/homebrew-kranz` tap, replace the formula's
   zero digest with the v0.2.0 source-tarball digest, and test a from-scratch
   installation.
7. On clean Linux, macOS, and Windows hosts, install without cloning this
   repository and reach `kranz --help` plus `kranz init`/`kranz ready` in a
   disposable repository.

No version bump is made in this preparation commit. Versioning before the
release cutoff would create a misleading half-release and complicate any
additional stabilization fixes.

## M6 — Railway live deployment

Shipped foundations:

- `kranz exec --push` is the sole ref-restricted cloud push path and accepts
  only `kranz/*` branches;
- `kranz serve --host 0.0.0.0 --insecure-lan` supports a platform router;
- non-loopback reads are token-gated, with separate mutation and read-only
  authorities; and
- the Dockerfile builds a non-root Kranz runtime with Git and CA roots.

The checked-in image is not yet a mission-runner image: it deliberately lacks
the Claude CLI, the target repository's toolchain, and Linux bubblewrap. A
Railway proof must use a derived, digest-pinned image containing those pieces.
It also needs a persistent `/work` volume containing the Git checkout and
`.kranz` runtime state.

Required operator inputs:

- Railway account, project, region, budget/spend alert, and public domain;
- `ANTHROPIC_API_KEY` with an intentionally bounded provider budget;
- independent high-entropy `KRANZ_TOKEN` and `KRANZ_READ_TOKEN` values;
- repository clone/read access and a push credential restricted to
  `refs/heads/kranz/*` (prefer a narrowly scoped GitHub App over a broad PAT);
- the target repository and derived toolchain image; and
- optional dedicated Slack app credentials if live Slack is part of the
  proof. Do not reuse one Socket Mode app across concurrently connected hosts.

Deployment posture:

```text
volume:       /work
health:       GET /api/health
start:        kranz serve --host 0.0.0.0 --insecure-lan \
                --port "$PORT" --read-auth
transport:    Railway HTTPS only; no raw public TCP port
observer:     KRANZ_READ_TOKEN
operator:     KRANZ_TOKEN
```

The start command may add `--slack` only after dedicated Slack credentials and
an unambiguous repository route are installed.

Minimum live acceptance:

1. A request without a token gets 200 only from `/api/health`; protected GET,
   WebSocket, and POST attempts get 401.
2. The read token opens protected GET/WS access but receives 401 from a POST.
   The mutation token reaches that same POST handler.
3. Restarting/redeploying preserves the repository, events, state, and queue
   on `/work`; both token values remain stable in Railway secrets.
4. One low-cost disposable mission goes browser conversation → approved plan
   → queue → isolated execution → validation → COMPLETE without a local Kranz
   install.
5. Its declared service/data readiness is proven, and only a reviewable
   `kranz/*` branch leaves the container. The credential cannot update `main`
   or force-push even if the in-process guard is bypassed.
6. A leaked URL without a token exposes no transcript, plan, diff, repository
   catalog, or mutation surface. Logs and the deployment receipt contain no
   token or provider credential.

The live action remains operator-gated because it creates a public endpoint,
spend, persistent external state, and credentials. The implementation ticket
is ready to execute as soon as those inputs are supplied.
