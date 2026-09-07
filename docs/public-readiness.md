# Public-readiness gate

Status (2026-09-06 UTC): **platform CI and unattended acceptance passed;
public release still gated**.
The owner approved v0.2.0 release preparation, and the clean-origin migration
is complete. Commit `d1eafe037df0dc08dd67e94889937fa66f56f7ac` passed all jobs
in the manually dispatched platform workflow, including the first unattended
live smoke attempt. The [acceptance receipt](reviews/2026-09-06-unattended-acceptance.md)
records the artifact digest, two automated waivers, and the limits of that proof.

The Actions credential gap is closed. The repository remains private, the
`release` environment has no reviewer protection, and
`KRANZ_PUBLIC_RELEASE_ENABLED` remains `false`. The
[owner review packet](reviews/2026-09-06-owner-publication-review.md) is prepared
but unsigned. Its open items include package and distribution notices, source
provenance, and confidentiality across every advertised ref. The
[repair-limit investigation](reviews/2026-09-06-repair-limit-investigation.md)
also identifies stale decision context that needs a focused correction before
release-candidate closure.

The active-origin hardening and local verification are recorded in the
[v0.2.0 candidate review](reviews/2026-09-05-release-candidate.md). That evidence
does not replace the final release commit's platform CI, public visibility review,
protected release environment, or published-artifact checks below.

The passing run is evidence for the pinned commit above. Subsequent runtime,
dependency, workflow, or fixture changes need their own relevant checks; the
eventual tag must have exact-commit release evidence. Preparation and scans do
not constitute owner approval to publish.

Making Kranz public exposes every reachable Git object and every existing
GitHub release asset. It is an operator action, separate from merging ordinary
hardening changes. Keep the repository private until every item below is true.

## Even G2 companion scope

The owner intends to include the Even Realities feature at go-live. The
experimental source/QR client needs its [physical acceptance receipt](even-g2-acceptance.md)
and exact-candidate CI. An installable Even Hub app additionally needs the
[production distribution work](even-g2-distribution.md); building an `.ehpk`
does not establish that it can connect to Kranz. Include the chosen scope,
third-party notices and new files in the final owner review and release audits.
Neither scope changes the current private-repository/publication hold.

## Repository content

- `scripts/audit-public-tree.sh` passes on the exact proposed public commit.
- `scripts/audit-public-history.sh` passes with every branch, tag, and GitHub
  pull-request ref fetched.
- Both audits pass with `KRANZ_REQUIRE_OPERATOR_MARKERS=1` and the owner's
  reviewed private vocabulary supplied outside the checkout. The matching
  `KRANZ_PUBLIC_AUDIT_MARKERS` Actions secret is configured. Ordinary CI's
  built-in-marker scan does not substitute for this owner-supplied vocabulary.
- Mission records, tickets, fixtures, and review documents have received a
  human confidentiality/licensing review; automated secret scans do not detect
  external-party identifiers, private prompts, or proprietary prose reliably.
- The owner has signed the pinned scope in the
  [review packet](reviews/2026-09-06-owner-publication-review.md), and package,
  binary, and embedded-dashboard notices have been verified in the actual
  distribution payloads. An SPDX label or dependency-policy pass alone does
  not close that evidence gap.
- A fresh anonymous-equivalent clone contains no required private submodule,
  credential, local path, or untracked generated asset.

If the history audit finds operator metadata, classify it before rewriting.
Create a mirror backup, place the literal replacement mappings in an untracked
file outside the repository, and use `git filter-repo --replace-text` plus
`--replace-message` in a disposable mirror clone: blob replacement does not
rewrite commit messages. Commit author/committer headers need a separate
`--mailmap` rewrite: map the historical maintainer address to
`6720093+craigcode@users.noreply.github.com`; replacing blob text does not edit
commit identities. Re-run both audits there. Rewriting changes every
affected commit and tag: coordinate the force-update, invalidate old clones,
and never mix it into a normal feature pull request. The rewrite also changes
the commit-derived fingerprints in `.gitleaksignore`; regenerate that file
from redacted scanner output, verify every replacement still identifies only a
synthetic scrubber fixture, and never broaden it to a file- or rule-wide
exception.

GitHub pull-request refs and cached commit views can outlive normal branch
deletion. Fetch `+refs/pull/*:refs/remotes/pull/*` into the disposable audit
clone before scanning. If sensitive content—not merely operator metadata—was
ever pushed, rotate the affected credential immediately and follow GitHub's
[sensitive-data-removal process](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository)
for PR refs, forks, and cached views; a local filter alone is not a containment
claim.

The legacy repository had pull-request refs whose ancestry contained operator
markers caught by the audit. GitHub makes those refs read-only, so the
clean-origin migration used this topology:

1. keep the legacy repository private as the historical archive under a new
   private name;
2. create a fresh private `craigcode/kranz` repository from the reviewed,
   rewritten history without importing legacy `refs/pull/*` or old releases;
3. reapply the ruleset, Actions restrictions, release lock, security settings,
   labels, Discussions, and protected environment; and
4. run both audits from the new origin and keep that origin private.

That migration is recorded in
[`reviews/private-clean-origin-migration.md`](reviews/private-clean-origin-migration.md).
It does not pre-approve a future visibility change. A future proposal must
re-audit every then-current advertised ref, obtain explicit owner approval,
and re-verify all GitHub controls before changing visibility.

## Existing releases

The v0.1.0 private-preview binaries predate substantial security hardening and
remain only in the private archive. The active origin has no imported tag or
release. They must never silently become the supported distribution.

## GitHub controls

- The main ruleset requires pull requests, conversation resolution, current
  required checks, and includes the supply-chain and policy jobs.
- Actions require full-length commit SHAs. On the public origin, set
  `allowed_actions` to `selected` and apply
  `.github/actions-allowlist.json`; GitHub only applies repository-level
  `patterns_allowed` to a personal-account repository after it is public.
- The `release` environment requires the repository owner to approve the
  publish job.
  The 2026-09-06 preparation attempt to add that rule was rejected by GitHub
  because the current plan does not support required reviewers for this
  private repository. Keep the release switch off; after separately approved
  public visibility, configure and verify the rule before enabling releases.
- The repository Actions variable `KRANZ_PUBLIC_RELEASE_ENABLED` remains
  `false` until every item in this document is complete; set it to `true` only
  after the protected release environment is verified.
- Private vulnerability reporting, Dependabot alerts/security updates, secret
  scanning, push protection, and code scanning are enabled where the current
  visibility/plan permits them.
- Discussions, issue templates, `SECURITY.md`, `CONTRIBUTING.md`, support
  policy, and the code of conduct are visible and correct.

## Visibility change

1. Record the passing audit output, exact commit SHA, and chosen origin-migration
   route.
2. Obtain explicit owner approval for the origin migration and public
   visibility.
3. Create or change visibility without pushing a version tag or publishing a
   package.
4. Clone anonymously into a new directory and rerun the public-tree and
   public-history audits.
5. Check releases, default branch/rules, Actions permissions, security
   features, issue links, and private vulnerability reporting as a visitor.
6. Protect the release environment, set `KRANZ_PUBLIC_RELEASE_ENABLED=true`,
   and only then follow `docs/releasing.md` for the first version-aligned
   release.
