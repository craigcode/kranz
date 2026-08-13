# Public-readiness gate

Making Kranz public exposes every reachable Git object and every existing
GitHub release asset. It is an operator action, separate from merging ordinary
hardening changes. Keep the repository private until every item below is true.

## Repository content

- `scripts/audit-public-tree.sh` passes on the exact proposed public commit.
- `scripts/audit-public-history.sh` passes with every branch, tag, and GitHub
  pull-request ref fetched.
- Mission records, tickets, fixtures, and review documents have received a
  human confidentiality/licensing review; automated secret scans do not detect
  external-party identifiers, private prompts, or proprietary prose reliably.
- A fresh anonymous-equivalent clone contains no required private submodule,
  credential, local path, or untracked generated asset.

If the history audit finds operator metadata, classify it before rewriting.
Create a mirror backup, place the literal replacement mappings in an untracked
file outside the repository, and use `git filter-repo --replace-text` in a
disposable mirror clone. Commit author/committer headers need a separate
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

This repository already has pull-request refs whose ancestry contains the
operator markers caught by the audit. GitHub makes those refs read-only, so a
normal force-push of rewritten heads and tags cannot make the in-place audit
pass. The preferred launch topology is therefore:

1. keep this repository private as the historical archive under a new private
   name;
2. create a fresh `craigcode/kranz` repository from the reviewed, rewritten
   history without importing `refs/pull/*` or old releases;
3. reapply the ruleset, Actions restrictions, release lock, security settings,
   labels, Discussions, and protected environment; and
4. run both audits from the new origin before making that origin public.

An in-place visibility change is acceptable only if GitHub Support confirms
that the relevant pull-request/cached refs were removed and a fresh audit of
every advertised ref passes. Renaming or replacing the origin is a coordinated,
explicitly approved operator migration; this pull request deliberately does
not perform it.

## Existing releases

The v0.1.0 private-preview binaries predate substantial security hardening. On
the preferred clean-origin route, leave them only in the private archive and
do not import their tag or release. On an in-place route, delete the release or
mark it as a clearly unsupported prerelease and verify that GitHub no longer
presents it as the latest stable download. A history rewrite also requires
recreating or removing tags whose release assets no longer correspond to their
source.

## GitHub controls

- The main ruleset requires pull requests, conversation resolution, current
  required checks, and includes the supply-chain and policy jobs.
- Actions require full-length commit SHAs. On the public origin, set
  `allowed_actions` to `selected` and apply
  `.github/actions-allowlist.json`; GitHub only applies repository-level
  `patterns_allowed` to a personal-account repository after it is public.
- The `release` environment requires the repository owner to approve the
  publish job.
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
