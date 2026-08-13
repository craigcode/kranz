# M4 distribution and M6 cloud operator readiness

Date: 2026-08-12

## Outcome

The two remaining milestones contain irreversible operator actions. M4 is not
ready for public visibility until the fail-closed gates in
`docs/public-readiness.md` pass; it then needs owner authorization for one
version-aligned release. M6 needs a Railway account/project, bounded spend,
and runtime credentials.

## M4 — public distribution

Verified state:

- the GitHub repository is private;
- the current tree has been sanitized, but the full-history privacy audit
  identifies older operator-path/email/plugin metadata plus the maintainer
  address in commit identity headers; GitHub's read-only PR refs retain part of
  that ancestry, so the preferred public launch uses a fresh sanitized origin
  while the existing repository remains a renamed private archive (the
  alternative is GitHub Support cleanup plus a coordinated in-place mirror
  rewrite);
- GitHub release `v0.1.0` exists with Linux x86_64, macOS arm64, and Windows
  x86_64 assets, but its tag predates substantial current development;
- crates.io names `kranz`, `kranz-engine`, `kranz-server`, and `kranz-slack`
  are reserved by 0.0.1 placeholder packages;
- no `craigcode/homebrew-kranz` tap exists; and
- `packaging/homebrew/kranz.rb.in` is a non-installable, current-wording
  release template, deliberately avoiding a bogus all-zero live formula.

Publishing current source as crates.io 0.1.0 would make it disagree with the
existing v0.1.0 tag and binary assets. The recommended next line is **0.2.0**:
one source commit, one tag, four crates, four binary assets, the Tauri
version, and the rendered Homebrew formula must all agree. Tauri desktop
bundles are not part of this release line.

Operator sequence after approving public visibility:

1. Merge the pre-public hardening, build the sanitized history in a disposable
   mirror, and obtain explicit approval for either the preferred clean-origin
   migration or GitHub-assisted in-place cleanup. Make both public audit scripts
   pass from a fresh clone containing pull refs.
2. Create/change the public repository, keep the old release only in the
   private archive (or withdraw/mark it unsupported in place), reapply the
   public-only security and protected-environment controls, and verify the
   anonymous visitor surface.
3. Freeze the release commit and bump root workspace/package dependency and
   Tauri versions to 0.2.0; add the dated changelog section.
4. Run every workspace/dashboard/security/packaging gate. Cargo can dry-run the
   engine before publication; dependent crates dry-run bottom-up after the
   required sibling version reaches crates.io, or against a disposable local
   registry before that.
5. Tag and push `v0.2.0`; require the protected release workflow, four builds,
   checksums, SBOM, and provenance attestations to pass before packages.
6. Publish bottom-up: engine; server and Slack; CLI. Confirm each index entry
   resolves before publishing its dependants.
7. Create the public `craigcode/homebrew-kranz` tap, render the formula template
   with the real v0.2.0 tarball digest, and test a from-scratch installation.
8. On clean Linux, macOS, and Windows hosts, install without cloning this
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
