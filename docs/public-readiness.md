# Public-readiness gate

Status (2026-09-13 UTC): **the source repository is public; distribution is
tracked separately**.

The owner authorized the public source snapshot and crates.io publication.
The first public `main` snapshot was one `Initial commit`; older branch and
pull-request references are separate from that history. The [README](../README.md#install)
leads with a source install and pins the registry command to an explicit version so readers
cannot silently install the earlier `0.0.1` placeholder. Check
[crates.io](https://crates.io/crates/kranz/versions) and
[GitHub Releases](https://github.com/craigcode/kranz/releases) for the artifacts
that have actually been published. Source availability is not evidence that
registry packages or prebuilt binaries exist.

Main branch protection is active. The `release` environment has an owner
reviewer, and `KRANZ_PUBLIC_RELEASE_ENABLED` remains `false`; the GitHub binary
release workflow remains locked until that separate publication is ready.
The historical [owner review packet](reviews/2026-09-06-owner-publication-review.md)
and [acceptance receipt](reviews/2026-09-06-unattended-acceptance.md) retain their
original scope and dates. They do not certify artifacts from a later commit.

The checklist below records the evidence needed for publication and artifact
verification. Automated secret scans do not replace confidentiality and
licensing review, and passing source checks do not establish a clean install
from a published distribution.

## Even G2 companion scope

The owner intends to include the Even Realities feature at go-live. The
experimental source/QR client needs its [physical acceptance receipt](even-g2-acceptance.md)
and exact-candidate CI. An installable Even Hub app additionally needs the
[production distribution work](even-g2-distribution.md); building an `.ehpk`
does not establish that it can connect to Kranz. Include the chosen scope,
third-party notices and new files in the final owner review and release audits.
Public source availability does not establish physical-device acceptance or
production distribution.

## Repository content

- `scripts/audit-public-tree.sh` passes on the exact proposed public commit.
- `scripts/audit-public-history.sh` passes with every branch, tag, and GitHub
  pull-request ref fetched.
- The existing secret scan and committed domain-policy check pass. An
  additional confidentiality word list is optional; the owner selected no
  additional list for v0.2.1 and v0.2.2. If one is configured, both audits must pass with
  `KRANZ_REQUIRE_OPERATOR_MARKERS=1` and the matching reviewed
  `KRANZ_PUBLIC_AUDIT_MARKERS` Actions secret.
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
That historical migration did not itself authorize publication. The later
owner-authorized public snapshot is described in the current status above;
new releases still need review of their advertised refs and distribution artifacts.

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
- If additional confidentiality terms are configured, the masked repository
  secret `KRANZ_PUBLIC_AUDIT_MARKERS` contains the same newline-delimited terms
  used locally. A configured but empty/invalid list fails closed. Without that
  optional secret, releases still require the existing secret and domain checks.
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
