# M4 distribution and M6 cloud operator readiness

Date: 2026-08-13

## Outcome

M4 public distribution is parked by explicit owner decision: the active clean
origin remains private and no package publication is authorized. M6 is the
only remaining operator-gated milestone; it needs a Railway account/project,
bounded spend, and runtime credentials.

## M4 — public distribution

Verified state:

- `craigcode/kranz` is the active private clean origin;
- its fresh-clone public-tree and public-history audits passed at the
  2026-08-14 identity-remediation boundaries, and its protected `main` CI was
  green at the first boundary;
- the legacy repository and superseded clean origins are retained separately
  as private archived evidence;
- the active origin has no imported tag or release; the historical v0.1.0
  binaries remain only in the private archive and are unsupported;
- crates.io names `kranz`, `kranz-engine`, `kranz-server`, and `kranz-slack`
  are reserved by 0.0.1 placeholder packages;
- no `craigcode/homebrew-kranz` tap exists; and
- `packaging/homebrew/kranz.rb.in` is a non-installable, current-wording
  release template, deliberately avoiding a bogus all-zero live formula.

The migration receipt is point-in-time evidence, not a promise that later
private-only commits remain publishable. The 2026-08-14 clean-origin rotations
removed operator identity metadata retained first by `main` and immutable
pull-request refs, then by a later GitHub merge commit. The required audit
caught that recurrence, the account email-privacy setting was corrected, and a
disposable GitHub merge proved the no-reply author path before the second
rewrite. A future policy reversal still requires another complete audit at the
proposed visibility boundary before any visibility change.

No public version is currently planned. If the private-repository decision is
reversed, publishing current source as 0.1.0 would still create false
provenance relative to the archived preview. The new proposal must choose one
version-aligned release across source, crates, binaries, Tauri metadata, and
the rendered Homebrew formula.

Dormant operator sequence after a future explicit public-visibility approval:

1. Open a new release ticket and re-run both public audit scripts from a fresh
   clone containing every then-current advertised ref.
2. Obtain explicit visibility approval, apply the public-only security and
   protected-environment controls, and verify the anonymous visitor surface.
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
