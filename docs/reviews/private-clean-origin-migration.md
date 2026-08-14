# Private clean-origin migration receipt

Initial date: 2026-08-13
Identity remediation: 2026-08-14

## Outcome

The active `craigcode/kranz` origin was rebuilt from the reviewed sanitized
history and retained as a **private** repository. On 2026-08-14 it was rotated
after GitHub rebase merges introduced operator committer metadata into the
active line and immutable pull-request refs retained those objects. A second
rotation followed when pull request #1's GitHub merge commit recorded the
operator's private address as its author while the account's email-privacy
setting was disabled. The superseded origins were renamed to
`kranz-private-archive-20260814` and
`kranz-private-archive-20260814-merge-author`, then kept private and archived.
Each fresh active origin imported only the audited `main`: no legacy
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
- Pull request #1's contaminated merge object
  `7f0e048e7adb742aaff9cbfee8aca631a19ee96e` maps to the surgical replacement
  `6c1b1acf5755f2a6b4a2923c0c330041abc282ee`. Both commits have tree
  `04a37fa918888351b6f6ea8389ae335de9a6eee9` and the same two parents. The
  clean, signed Dependabot parent
  `e4d2e5f4f994f6d9a1498cd74e888d2e8cae4311` remains byte-identical.

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

The required post-merge history audit caught the pull request #1 recurrence
before publication. GitHub merge commits use the pull request actor's account
identity for the author, so merge-only policy is insufficient if that account
exposes a private address. The operator enabled GitHub's email-privacy setting,
then disposable pull request #2 proved a real server-created merge commit
(`9e9672844493c5bdb819630ef940c4ed64464a84`) used
`6720093+craigcode@users.noreply.github.com` as author and
`noreply@github.com` as committer. That probe and its refs remain only in the
private superseded origin; they are not imported into the fresh origin.

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

The legacy GitHub origin and both superseded 2026-08-14 active origins remain
private and archived, with separate offline mirrors retained for recovery.
They are historical evidence, not active remotes for development,
distribution, or release automation.

## Continuing rule

All active work starts from the clean private origin. Public visibility,
crates.io publication, Homebrew distribution, and a new version tag require a
new explicit operator decision and the complete fail-closed release checklist.
