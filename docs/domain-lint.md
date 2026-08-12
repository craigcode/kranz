# Clean-room domain lint (KRZ-314)

The positioning ADR
(`docs/knowledge/decisions/positioning-governance-evidence-layer.md`) freezes
an IP boundary: kranz core is domain-free — the protected vocabulary classes
(consumer names, legacy-platform terms, consumer schema identifiers) never
appear in core, and domain knowledge ships in private packs behind the pack
contract (KRZ-313). `kranz domain-lint` is the mechanical guard that makes
the boundary auditable instead of aspirational: it scans the scoped tree and
fails when a banned term appears, naming file and line. It is NOT secret
scanning — that is `kranz scan` / `scrub`; this lint guards a vocabulary
boundary, with its own config, waiver file, and CI job.

## Running it

```bash
kranz domain-lint              # text report; exit 0 clean, 1 on unwaived hits
kranz domain-lint --json       # the same report as JSON
```

A hit prints as `<fingerprint> <path>:<line>` — never the matched text,
which is the vocabulary the boundary protects.

## Scope

Candidates are `git ls-files --cached --others --exclude-standard`: tracked
files plus untracked-but-not-ignored files. git's own ignore engine is the
only faithful `.gitignore` reader, and it keeps the local command and the CI
job on the same tree. Excluded on top of that:

- `.kranz/missions/` — mission runtime artifacts are operator content (they
  quote whatever a mission was about; the boundary governs kranz core);
- the lint's own policy files (`.kranz/domain-denylist.json`,
  `.kranz/domain-allowlist`, `.kranz/domain-terms.local`);
- binary files (NUL byte present) and files over 8 MiB, skipped whole.

## Matching rule (normalization)

A term — or a span of scanned text — normalizes to a token sequence: maximal
runs of ASCII alphanumeric characters, lowercased, joined with one space.
Everything else is a separator. Consequences:

- case, spacing, and punctuation variants of a term all match (`a-b` ==
  `A B` == `a.b`);
- a phrase may span a line break; the hit is reported at its first token's
  line;
- camelCase compounds are ONE token (`zzAcme` → `zzacme`) and do not match
  the two-token form `zz acme` — seed both forms when both matter;
- terms are at most 8 normalized tokens (seed refuses longer ones loudly).

## The denylist: salted hashes, plaintext outside the repo

`.kranz/domain-denylist.json` is committed and contains NO readable terms:

```json
{
  "version": 1,
  "salt": "<64 hex>",
  "hash": "sha256(salt || NUL || normalized-term)",
  "terms": ["<64 hex sha256>", ...]
}
```

The plaintext vocabulary lives OUTSIDE the repo in the gitignored
`.kranz/domain-terms.local` (one term per line, `#` comments). Regenerate
the config after editing it:

```bash
kranz domain-lint --seed-config .kranz/domain-terms.local
```

The salt is generated once and preserved across reseeds so waiver
fingerprints stay valid. The salt is committed, so it is not a secret: a
guesser who suspects a term can recompute the hash and confirm it. What the
hash form buys is that the committed file is not a readable copy of the
vocabulary — no rainbow-table or search-engine lookup resolves bare
`sha256(term)` values — while the seed command keeps regeneration a
one-step, auditable operation. A test asserts the committed config carries
hash-form entries only.

## Waivers: `.kranz/domain-allowlist`

Same reviewed-waiver idiom as `.kranz/secret-allowlist`: one fingerprint per
line, `#` comments, and comments DESCRIBE the waived hit without ever
quoting it (a quoted term here is itself the leak). A finding's fingerprint
is printed by the lint; add it only after review. Fingerprints are
path-scoped salted hashes
(`sha256(salt || NUL || term || NUL || path)`, truncated to 24 hex): a
waiver covers every occurrence of one term in one file — present and future
— so re-review on any edit that leans on one. The seeded waivers cover the
two governance docs whose job is to DEFINE the boundary by naming its
protected classes; every other mention needs its own review.

## CI: trusted base, full tree

`.github/workflows/domain-lint.yml` mirrors `secret-scan.yml`'s trusted-base
posture (`pull_request_target` + push to `main`, status published on the
proposed SHA) because the self-judgment hole is identical: linter, denylist,
and allowlist all travel with the repository, so a plain `pull_request` job
would let a change weaken its own judge. The one difference is scope — this
lint judges the whole tree, not a diff range — so the job checks out the
proposed tree as inert data, overlays the TRUSTED policy files
(`.kranz/domain-denylist.json`, `.kranz/domain-allowlist`) onto it, and runs
the TRUSTED-built linter against it. Consequences, both deliberate:

- a change cannot un-ban a term or waive its own leak — policy always comes
  from the base side;
- a LEGITIMATE policy change (new term seeded, new reviewed waiver) takes
  effect for CI only after it lands on the base branch, exactly like the
  secret allowlist. Land policy first, then the content that needs it.
