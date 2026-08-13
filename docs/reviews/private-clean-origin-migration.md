# Private clean-origin migration receipt

Date: 2026-08-13

## Outcome

The active `craigcode/kranz` origin was rebuilt from the reviewed sanitized
history and retained as a **private** repository. The former origin was renamed
and retained as a private archived repository. No legacy pull-request refs,
tags, releases, deploy keys, webhooks, or repository secrets were imported.

The migration is a privacy and provenance boundary. It is not approval to make
Kranz public or publish packages.

## Verified migration boundary

- Initial protected-main receipt: `73b0bb8dbd63501d157ab44fc7597f458a1dbf9b`.
- At the receipt SHA, the fresh checkout tracked only the active private origin
  and was clean on `main`.
- `git fsck`, Gitleaks, `scripts/audit-public-history.sh`, and
  `scripts/audit-public-tree.sh` passed from a clone created from zero.
- The durable clean clone was pruned and rechecked with no unreachable legacy
  objects.
- Reachable commit identities were restricted to the reviewed no-reply and CI
  identities.
- The final migration CI run (`31674027215`), secret scan (`31674027211`), and
  domain lint (`31674027219`) completed successfully.

## Post-migration private activity

This receipt is point-in-time evidence. The first GitHub rebase merge after the
migration boundary recorded the operator account's non-no-reply committer
identity, and `scripts/audit-public-history.sh` now fails closed on that
metadata. That does not expose the private repository, but it means the active
history is no longer a publication candidate. A future visibility proposal
must remediate the then-current history and produce a new receipt; this record
must not be reused as approval.

## Repository controls

- Visibility remains private.
- Protected `main` accepts pull requests with linear history and the required
  Rust, Windows, macOS, wrapped-sandbox, MSRV, Tauri, dashboard, Docker,
  supply-chain, secret-scan, and domain-lint checks.
- GitHub Actions are SHA-pinned, default workflow permissions are read-only,
  and workflows cannot approve pull requests.
- `KRANZ_PUBLIC_RELEASE_ENABLED` remains `false`.
- GitHub-plan features unavailable to this private repository, including the
  effective CodeQL job, are not treated as passing security evidence; Kranz's
  own secret and supply-chain jobs remain required compensating controls.

## Retained archive

The legacy GitHub origin remains private and archived, with a separate offline
mirror retained for recovery. It is historical evidence, not an active remote
for development, distribution, or release automation.

## Continuing rule

All active work starts from the clean private origin. Public visibility,
crates.io publication, Homebrew distribution, and a new version tag require a
new explicit operator decision and the complete fail-closed release checklist.
