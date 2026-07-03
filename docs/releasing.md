# Cutting a release

Kranz ships three ways: prebuilt binaries attached to a GitHub release
(automated), a from-source `cargo install`, and — once the project is public —
crates.io and a Homebrew tap. This runbook is the end-to-end procedure.

## 0. One-time setup (before the very first release)

The repo ships with a literal `OWNER` placeholder in several files. Replace it
with the real GitHub org/user everywhere before the first publish:

- `Cargo.toml` — `[workspace.package] repository = "https://github.com/OWNER/kranz"`
- `packaging/homebrew/kranz.rb` — `homepage` and `url`
- `README.md` — the `brew tap OWNER/kranz` line

`repository`, `keywords`, and `categories` live in `[workspace.package]` and are
inherited by each publishable crate (`repository.workspace = true`, etc.), so
you only edit the slug in one place for the Cargo metadata.

## 1. Bump the version

Versions are workspace-inherited. Bump `[workspace.package] version` in the
root `Cargo.toml` once and every crate follows (they all use
`version.workspace = true`). Also bump:

- `apps/dashboard/src-tauri/tauri.conf.json` — `"version"` (the Tauri desktop
  app is a standalone workspace and does not inherit the root version).
- `apps/dashboard/src-tauri/Cargo.toml` — `version` (same reason).
- `packaging/homebrew/kranz.rb` — `version` and the `v#{version}` in `url`
  (updated again in step 4 once the tag exists and its sha256 is known).

Run `cargo update -p kranz-engine -p kranz-server -p kranz-slack -p kranz-cli`
(or `cargo check --workspace`) so `Cargo.lock` records the new version, and
commit the bump.

## 2. Tag and push

```sh
git tag vX.Y.Z          # tag name MUST start with "v" — release.yml triggers on v*
git push origin main
git push origin vX.Y.Z  # this push is what fires the release build
```

Pushing the tag triggers `.github/workflows/release.yml`:

- Matrix build on `ubuntu-latest`, `macos-latest`, `windows-latest`.
- Each job runs `cargo build --release --locked -p kranz-cli --bin kranz`,
  renames the binary to `kranz-<os>-<arch>` (`.exe` on Windows), and attaches
  it to the release for the tag via `softprops/action-gh-release`.
- `release.yml` only *builds*; `ci.yml` already gates PRs and pushes to `main`
  with clippy + the full test suite, so the release path does not re-test.

Confirm all three matrix jobs are green and the three assets are attached to
the GitHub release before continuing. (CI-green + the tag push are the
human-gated step — do not automate past here without checking the run.)

## 3. Write release notes

Edit the GitHub release created by the tag: summarize changes, and point users
at the attached `kranz-<os>-<arch>` assets and the install options in the
README.

## 4. Update the Homebrew formula

The formula (`packaging/homebrew/kranz.rb`) is from-source: it fetches the tag's
source tarball and runs `cargo install`. After the tag exists, compute the
tarball sha256 and fill the placeholder:

```sh
curl -sL https://github.com/OWNER/kranz/archive/refs/tags/vX.Y.Z.tar.gz \
  | shasum -a 256
```

Set `url` (to `.../tags/vX.Y.Z.tar.gz`), `version` (`X.Y.Z`), and `sha256` in
the formula, then move it to your tap repo (`homebrew-kranz`) or open a
homebrew-core PR. Verify locally with `brew install --build-from-source
./packaging/homebrew/kranz.rb`.

## 5. Publish to crates.io (optional, once public)

`kranz-cli` and `kranz-server`/`kranz-slack` depend on sibling crates by
**path** (e.g. `kranz-engine = { version = "0.1.0", path = "crates/engine" }`).
crates.io ignores the `path` and resolves each dependency by its `version`, but
that only works if the dependency is already published at that version — so the
order matters. The dependency DAG is:

```
engine  (no intra-workspace deps)
  ├── server  (deps: engine)
  ├── slack   (deps: engine)
  └── cli     (deps: engine, server)
```

Publish bottom-up, waiting for each crate to be live on crates.io (index
propagation) before the crate that depends on it:

```sh
cargo publish -p kranz-engine     # 1. base crate, no siblings
cargo publish -p kranz-server     # 2. depends on engine
cargo publish -p kranz-slack      # 2. depends on engine (independent of server)
cargo publish -p kranz-cli        # 3. depends on engine + server
```

Notes:

- The version in each `version = "X.Y.Z", path = ... ` sibling dependency (in
  the root `[workspace.dependencies]`) MUST equal the version being published,
  or the dependent crate will fail to resolve on crates.io. Because everything
  is workspace-versioned, bumping the root `version` in step 1 keeps these in
  sync — just remember to bump `[workspace.dependencies]` entries too if they
  ever pin a different version.
- `apps/dashboard` (the Tauri shell) is **not** published to crates.io; it is a
  standalone workspace shipped only as the desktop bundle (`dmg`/`msi`/
  `appimage` from `tauri build`).
- Dry-run first: `cargo publish -p kranz-engine --dry-run`.

## Recap (order of operations)

1. (first release only) replace `OWNER` everywhere.
2. Bump `version` (root Cargo.toml + tauri.conf.json + src-tauri Cargo.toml +
   formula), commit.
3. `git tag vX.Y.Z` and push the tag → `release.yml` builds + attaches binaries.
4. Verify CI green + assets attached; write release notes.
5. Update the Homebrew formula's `sha256`/`version`/`url`.
6. (optional) `cargo publish` in order: engine → server, slack → cli.
