# Cutting a release

Kranz distributes a CLI through GitHub release binaries and crates.io.
A Homebrew tap is a follow-on after those two paths are proven on clean hosts,
not a condition of the first public release. The Tauri shell is build-checked
but is not currently a supported release artifact. Do not advertise or attach
desktop bundles until a separate signed, platform-specific bundle/notarization
pipeline exists.

The historical v0.1.0 GitHub release is a private preview. It is 1,000+ commits
behind the current source and predates substantial security hardening. Never
reuse that tag or publish current source as 0.1.0. v0.2.2 provides matching
GitHub binaries and registry packages. Choose a new version for every
subsequent release; never replace an existing tag or archive.

## 0. Public-distribution prerequisites

Complete `docs/public-readiness.md`. In particular:

- both public audit scripts pass from a fresh clone containing every branch,
  tag, and GitHub pull-request ref;
- the old v0.1.0 release remains only in the private archive, or is withdrawn
  or visibly marked unsupported on an approved in-place route;
- the repository is public and an anonymous clone has been verified;
- all four reserved crates.io names are controlled by the expected owners;
- the `release` GitHub environment requires owner approval;
- the repository Actions variable `KRANZ_PUBLIC_RELEASE_ENABLED` is `true`;
  and
- no release tag already exists for the chosen version.

The normal release checks require the existing secret scanner and committed
domain policy. An additional confidentiality word list is optional; the owner
selected no additional list for v0.2.1 and v0.2.2, and the v0.2.3 patch retains
that policy. No new secret is needed for that choice.

If additional confidential names or phrases must be blocked, supply one
case-sensitive UTF-8 literal per line
in a file outside the checkout (`KRANZ_PUBLIC_AUDIT_MARKERS_FILE`), or through
`KRANZ_PUBLIC_AUDIT_MARKERS`. Blank lines and `#` comments are ignored. Set
`KRANZ_REQUIRE_OPERATOR_MARKERS=1` for both candidate audits; missing, empty or
unreadable input fails. These checks print counts rather than vocabulary or
matched content. The history check examines every reachable object, including
deleted blobs, commit messages and identity headers, and refuses shallow history.
Configure the repository Actions secret `KRANZ_PUBLIC_AUDIT_MARKERS` with the
same reviewed vocabulary. When configured, the release workflow requires it to
be nonempty and pass both audits. When absent, it explicitly skips only this
additional scan. Gitleaks and the committed domain policy remain mandatory.
The domain policy has reviewed, path-specific exceptions; its private seed file
is not interchangeable with a blanket confidentiality word list.

These are human/operator gates. Neither Kranz nor a coding-agent mission pushes
branches, tags, crates, formulas, or releases.

## 1. Prepare the version pull request

Versions are workspace-inherited. Update:

- `[workspace.package] version` in the root `Cargo.toml`;
- `kranz-engine`, `kranz-server`, and `kranz-slack` version requirements under
  root `[workspace.dependencies]`;
- `apps/dashboard/src-tauri/tauri.conf.json`;
- `apps/dashboard/src-tauri/Cargo.toml`;
- README install commands and versioned registry/release links; and
- `CHANGELOG.md`, moving the relevant Unreleased entries into a dated version
  section.

Do not render the Homebrew template yet: GitHub's tagged tarball and its digest
do not exist until the tag exists.

Run `cargo check --workspace` to record the new workspace package versions in
the lockfile, and refresh the standalone Tauri lockfile with `cargo check` from
its directory. Review both lockfile diffs, then repeat the checks with
`--locked`. Run the exact release check locally without querying remote main:

```sh
KRANZ_RELEASE_SKIP_MAIN_CHECK=1 scripts/check-release-version.sh vX.Y.Z
```

## 2. Run the release-candidate gates

Run commands directly and preserve their exit codes:

```sh
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check
target/debug/kranz domain-lint
scripts/audit-public-tree.sh
scripts/audit-public-history.sh
```

If the owner adds a confidentiality word list, point
`KRANZ_PUBLIC_AUDIT_MARKERS_FILE` at that reviewed file outside the checkout
and set `KRANZ_REQUIRE_OPERATOR_MARKERS=1` for both audit commands. Routine
public CI and the release workflow scan full history with Gitleaks regardless
of whether an additional word list is configured.

Run the full dashboard gate from `apps/dashboard` and the locked Tauri check
from `apps/dashboard/src-tauri`, as described in `AGENTS.md`. The release pull
request must pass every required GitHub check on the exact commit that will be
tagged.

The manually dispatched live-smoke job additionally needs the repository's
`ANTHROPIC_API_KEY` secret. Its Linux setup installs and probes bubblewrap and
enables user namespaces on the disposable hosted runner before spending model
tokens. Validator containment remains required; local OAuth rehearsal evidence
does not establish that the remote job's credentials or host setup work.

The final acceptance audit also executes generated code. Build the test-only
adapter with `cargo build --locked -p kranz-engine --example acceptance_gate`
and set `KRANZ_ACCEPTANCE_GATE_BIN` to that executable when invoking
`scripts/acceptance-smoke.sh`. It uses the existing production gate runner with
`fs` enforcement, a sanitized environment, and mission authority read-denies;
there is no unsandboxed fallback. The filesystem tier permits networking,
including the localhost listener required by the HTTP contract. The offline
harness tests use the debug example by default and require Seatbelt on macOS or working bubblewrap on Linux.

## 3. Rehearse crate packaging honestly

Cargo removes workspace `path` dependencies when packaging. For a single
dependent crate, its sibling version must already be visible in the target
registry. Current Cargo also supports a workspace-wide dry run, staging the
selected packages together so unpublished siblings can be verified before
upload. This path was exercised with Cargo 1.97.1 for v0.2.3; the application's
Rust 1.88 build floor does not imply every older Cargo has the same publishing
options. See the [Cargo publish reference](https://doc.rust-lang.org/cargo/commands/cargo-publish.html).

Before publication:

```sh
cargo package --list -p kranz-engine
cargo package --list -p kranz-server
cargo package --list -p kranz-slack
cargo package --list -p kranz
cargo publish --workspace --dry-run --locked
```

Inspect every package list for secrets, runtime state, oversized fixtures, and
unintended generated files. Keep `--dry-run` explicit: a workspace publication
without it uploads all selected packages. Actual release uploads below remain
one package at a time, with an immediate dry run and registry verification at
each step. With an older Cargo that cannot stage a workspace, use a disposable
local registry for the pre-publication proof; do not mislabel a missing sibling
version in crates.io as a source defect.

## 4. Merge, tag, and approve GitHub publication

After the version pull request merges, rehearse the release workflow on `main`
before creating the public tag:

```sh
gh workflow run release.yml --ref main -f tag=vX.Y.Z
```

The manual run performs source verification and all platform builds, and
produces temporary workflow artifacts. Its publication job is disabled.
Inspect those artifacts and wait for both this rehearsal and `main` CI to pass.
Run the archive smoke workflow against the rehearsal run and its exact source:

```sh
gh workflow run release-archive-smoke.yml --ref main \
  -f run_id=RELEASE_RUN_ID -f source_ref=refs/heads/main \
  -f source_sha=RELEASE_COMMIT_SHA -f version=X.Y.Z
```

Replace the placeholders with the recorded run ID, full commit SHA, and
version. All five jobs must verify provenance, native binary architecture,
version, help, and embedded licenses outside a source checkout. This workflow
downloads the built archives; it does not rebuild them or publish anything.
After it passes, create an annotated tag on that exact commit and push it as a
separate operator action:

```sh
git switch main
git pull --ff-only origin main
scripts/check-release-version.sh vX.Y.Z
git tag -a vX.Y.Z -m "Kranz vX.Y.Z"
git push origin vX.Y.Z
```

The tag starts `.github/workflows/release.yml`. It rechecks version/main
alignment, all reachable history, Rust/dashboard gates, documentation, and
dependency policy before building. It also verifies upstream license text
and the committed dependency notices using pinned cargo-about 0.9.2. Each
platform archive contains the executable, MIT license, Rust/dashboard
dependency notices, and the build toolchain's Rust library copyright inventory.
Archives receive GitHub build-provenance attestations. The final
protected-environment job assembles the archives, SPDX JSON SBOM and
`SHA256SUMS`; an owner must approve that job before
GitHub creates the release.

After approval, verify all assets, checksums, attestations, generated notes,
and `kranz --version` on clean Linux, macOS, and Windows hosts. Also inspect
the extracted notices, run `kranz licenses` outside a source checkout, and
check `/THIRD_PARTY_NOTICES.txt` from the served embedded dashboard. A failed matrix
or missing evidence means no release—delete the draft/tag only through the
documented operator recovery process.

Repeat `release-archive-smoke.yml` for the tag-triggered release run, using
`source_ref=refs/tags/vX.Y.Z` and the same source SHA. A rehearsal receipt does
not attest a later rebuild. Compare published archive bytes with those tested
workflow artifacts, and verify the checksum manifest and SBOM attestations.

## 5. Publish crates bottom-up

Publishing is irreversible. Run each dry-run immediately before its publish,
then wait until crates.io resolves that exact version before continuing:

```sh
cargo publish --dry-run -p kranz-engine
cargo publish -p kranz-engine

cargo publish --dry-run -p kranz-server
cargo publish -p kranz-server

cargo publish --dry-run -p kranz-slack
cargo publish -p kranz-slack

cargo publish --dry-run -p kranz
cargo publish -p kranz
```

`kranz-server` and `kranz-slack` are independent once `kranz-engine` is live;
the CLI must be last because it depends on all three. Confirm ownership,
package contents, repository URL, license, README rendering, and installability
on crates.io after every step.

## 6. Optional follow-on: render and publish the Homebrew formula

Do this only after the v0.2.0 Cargo and GitHub installations are proven. A
Homebrew failure does not invalidate those published artifacts and does not
block closing the first public release.

Download GitHub's tagged source tarball, compute its SHA-256 digest, and render
`packaging/homebrew/kranz.rb.in` into the `craigcode/homebrew-kranz` tap by
replacing `VERSION` and `SHA256`. Review the resulting diff; the committed file
in this repository remains a template, not an installable all-zero formula.

Test the rendered formula before pushing the tap:

```sh
brew install --build-from-source ./Formula/kranz.rb
brew test kranz
kranz --version
```

Finally verify a clean `brew tap craigcode/kranz && brew install kranz` and a
clean `cargo install kranz --locked` without a Kranz source checkout.

## 7. Close the release

- Update the changelog comparison links if adopted.
- Record clean-install evidence and artifact digests in the release notes.
- Verify GitHub still reports every required security and branch rule.
- Mark the M4 operator-release ticket done after GitHub, crates.io, and
  clean-host smoke tests all agree on the same version. Track Homebrew as a
  separate post-v0.2.0 distribution follow-on.
