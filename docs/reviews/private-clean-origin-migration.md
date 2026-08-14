# Private clean-origin migration receipt

Initial date: 2026-08-13
Identity remediation: 2026-08-14

## Outcome

The active `craigcode/kranz` origin was rebuilt from the reviewed sanitized
history and retained as a **private** repository. On 2026-08-14 it was rotated
again after GitHub rebase merges introduced operator committer metadata into
the active line and immutable pull-request refs retained those objects. The
former active origin was renamed to `kranz-private-archive-20260814` and kept
private. The fresh active origin imported only the rewritten `main`: no legacy
pull-request refs, tags, releases, deploy keys, webhooks, or repository secrets
were imported.

The migration is a privacy and provenance boundary. It is not approval to make
Kranz public or publish packages.

## Verified migration boundary

- Initial protected-main receipt: `73b0bb8dbd63501d157ab44fc7597f458a1dbf9b`.
- The 2026-08-14 identity-rewrite source tip
  `df991262517ae3abdb17babab0ce0bf8d70bb1a1` maps to rewritten boundary
  `f4a7508055b67ae6851b154a237a5dec8456a137`.
- `git filter-repo` parsed all 1,236 `main` commits and replaced the flagged
  operator address in exactly nine author/committer headers. The old and
  rewritten boundary trees are both
  `329b911dd24603b534b1b694d349162082255955`; no source byte changed in the
  metadata rewrite.
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

## 2026-08-14 identity remediation

A force-update of `main` alone was rejected as incomplete: the active GitHub
repository advertised immutable `refs/pull/*` objects containing the same
flagged identity. The repository was therefore rotated instead of claiming a
partial cleanup. The superseded origin remains private evidence; the fresh
origin contains only the audited rewritten `main`.

Two controls close the recurrence path. Required supply-chain CI runs
`scripts/audit-public-history.sh` with the expensive duplicate Gitleaks pass
disabled, so reachable identity/content markers fail the existing required
check. The fresh repository accepts GitHub merge commits rather than
server-side rebase commits; the latter recorded the operator account address
after the first migration, while GitHub merge commits use the reviewed GitHub
no-reply committer identity.

This receipt remains point-in-time evidence, not publication approval. A
future visibility proposal must re-audit every then-current advertised ref
from a new clone and produce its own receipt.

## Repository controls

- Visibility remains private.
- Protected `main` accepts pull requests through merge commits and the required
  Rust, Windows, macOS, wrapped-sandbox, MSRV, Tauri, dashboard, Docker,
  supply-chain, secret-scan, and domain-lint checks.
- GitHub Actions are SHA-pinned, default workflow permissions are read-only,
  and workflows cannot approve pull requests.
- `KRANZ_PUBLIC_RELEASE_ENABLED` remains `false`.
- GitHub-plan features unavailable to this private repository, including the
  effective CodeQL job, are not treated as passing security evidence; Kranz's
  own secret and supply-chain jobs remain required compensating controls.

## Retained archive

The legacy GitHub origin and the superseded 2026-08-14 active origin remain
private and archived, with a separate offline mirror retained for recovery.
They are historical evidence, not active remotes for development,
distribution, or release automation.

## Continuing rule

All active work starts from the clean private origin. Public visibility,
crates.io publication, Homebrew distribution, and a new version tag require a
new explicit operator decision and the complete fail-closed release checklist.
