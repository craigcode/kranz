# v0.2.0 release-candidate review

The release work is based on active origin commit
`eea2e62d8723d6e2ea3d59890c07820f17a6b36c`, in a fresh clone of
`craigcode/kranz`. The earlier local audit was on the archived lineage.
Its fixes were reconciled as source changes; no archived Git objects were
imported into the clean public-origin history.

## Changes reviewed

- GitHub comment triggers require an explicit `hooks.allowUsers` list. An
  authentic webhook signature alone does not authorize its comment author.
  Workflow failure triggers also verify that the head belongs to the receiving
  repository, rather than trusting a fork's branch name.
- Engine Git operations disable external diffs and text converters and refuse
  custom merge execution. Handles refresh after workers return so new driver
  names do not escape the protected configuration snapshot.
- Mount-backed sandboxes use private authority directory views to hide files
  created or atomically replaced after launch, without creating placeholder
  credentials in the checkout. Sibling mission metadata stays protected.
  Seatbelt pins authority ancestors against rename-based escapes.
- Validator cache copying uses descriptor-relative, no-follow operations and
  rejects symlinks and special files while preserving executable permissions.
- Cursor uses a private Keychain backing store with a local login alias,
  migrates legacy stores without replacing their contents, rejects injected
  passphrase commands, and tests actual native lock-state transitions.
- Container gates retain a separate allowlisted host client environment for
  both launch and timeout cleanup. Worker environment values cannot redirect
  that client, and client proxy settings cannot inject payload credentials.
- Both Cargo lockfiles replace yanked `chacha20` 0.10.1 with 0.10.2. No
  dependency policy or advisory exception was broadened.

The review covered correctness, readability, architecture, security, and
performance. Changes retain the existing event/state schema, environment
clearing, validator containment, default protected Git handles, Windows
boundaries, and cache-only container mounts. New authority views enumerate
small control directories at sandbox construction; warmed cache copying remains
bounded by the existing size ceiling.

## Local evidence

Commands ran with their actual exit codes retained, on macOS arm64 with
Rust 1.97.1 and Node 22.23.2.

| Check | Result |
| --- | --- |
| `cargo test --workspace --no-fail-fast` after the final code/dependency changes | 2,783 passed, 0 failed, 7 ignored; exit 0 |
| Required runtime capabilities | Git, Seatbelt, Keychain, and grep were required; unavailable capabilities could not silently pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | Exit 0 |
| `cargo fmt --all`, then `cargo fmt --all --check` | Exit 0 |
| `cargo build --workspace --locked` | Exit 0 |
| Strict workspace rustdoc | Exit 0 |
| Dashboard clean `npm ci`, audit, TypeScript, tests, build, embedded freshness, lint | 196 tests passed; all commands exit 0; npm reported no vulnerabilities |
| Standalone Tauri `cargo check --locked` | Exit 0 |
| Root `cargo audit --deny unsound` | Exit 0, no advisories or yanked-package warnings |
| Tauri `cargo audit --deny unsound`, from its own workspace | Exit 0 under the existing scoped `glib` exception; 16 unmaintained dependency warnings remain |
| `cargo deny check` | Advisories, bans, licenses, sources pass |
| Four crate package inventories | Reviewed; CLI includes its dashboard assets; no runtime state or credentials |
| Engine crates.io publish dry-run with locked dependencies | Package verification passes; upload was not performed |
| Optimized CLI installation into an empty installation prefix | `cargo install --path crates/cli --locked` succeeds |
| Installed-binary smoke with source-checkout reads denied | v0.2.0, help, idempotent init, readiness JSON, embedded HTML/assets, read/mutation authorization, 0600 tokens, and graceful token cleanup all pass |
| Public-tree and uncommitted-diff Gitleaks scans | Pass |
| Baseline full advertised-ref history audit | 1,239 commits scanned, no leaks; all branch, tag, and pull-request refs fetched |
| Version/changelog alignment | v0.2.0 check passes with remote-main check explicitly skipped for the unmerged candidate |

The skip ledger records container fixtures not exercised on this host because
its VM does not share the system temporary directory, plus Linux-only
integration fixtures. These do not constitute a Linux or Windows receipt.
The supported release artifacts remain the CLI and embedded dashboard; signed
Tauri desktop installers are outside this release.

## Release closure

This report is local candidate evidence, not a publication receipt. The final
commit must pass the required platform CI and advertised-ref audit. The live
five-feature mission rehearsal, protected release
environment, anonymous public clone, and published artifact checks are recorded
as they complete. Dependent crates.io dry-runs must wait for their sibling
0.2.0 versions to become resolvable, as documented in `docs/releasing.md`.
