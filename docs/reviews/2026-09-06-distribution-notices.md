# Distribution notice review — 2026-09-06

This change closes the notice-delivery gaps observed on the earlier candidate
in [the owner packet](2026-09-06-owner-publication-review.md). It does not grant
owner confidentiality, authorship or publication approval. Final platform CI,
release archive verification and the owner's content review remain required.

## Delivered notices

- All four Cargo packages include a byte-identical copy of the root MIT
  license. The package check verifies their Cargo file lists and both embedded
  notice files in the CLI package.
- `kranz licenses` prints the project license, Rust dependency notices and
  embedded dashboard notices from the executable. Its regression copies the
  binary to a fresh directory and checks complete notice contents without a
  checkout, a sidecar or a running agent.
- Vite emits the licenses of the dependencies actually bundled in the
  dashboard. The embedded bundle includes `THIRD_PARTY_NOTICES.txt`, linked
  from the HTML head and served with a plain-text content type. The current
  file contains React, React DOM, Scheduler and Zustand notices.
- Each of the five release targets is packaged as an archive: `.tar.gz` on
  Linux/macOS and `.zip` on Windows. Every archive includes the executable,
  MIT license, Rust and dashboard notices, README and the build toolchain's
  Rust library copyright inventory. Archive provenance and checksums cover
  these accompanying files. Docker retains the same project/dependency
  notices and library inventory under `/usr/share/doc/kranz`.

## Generation and review boundary

Pinned `cargo-about 0.9.2` resolves the locked CLI graph for all five targets,
including build dependencies and excluding dev-only dependencies. This is a
conservative union, not a claim that each executable links every listed crate.
The current inventory has 288 crates. The Rust library inventory comes from
that build's compiler distribution, not from Cargo dependency metadata.

`cargo-about --fail` still permits fallback to generic SPDX text when it
cannot harvest the actual upstream license. That loses copyright details in
several packages. The checked configuration binds those cases to upstream
files with SHA-256 checksums, preserving the declared license expressions;
OpenTelemetry's missing packaged license is retrieved from its recorded
upstream Git revision. The check also inspects the generated JSON and fails
on an empty inventory or any text without a source. The reviewed graph has
zero generic-text fallbacks. Line endings and trailing whitespace are normalized
only in the generated output so a Git checkout cannot change the comparison.

The crate roots and nested NOTICE/NOTICES files were inspected for this
inventory; no additional NOTICE/NOTICES files were present. Composite upstream
license files, including AWS-LC and ring, are retained by the generator. The
configuration's license selection is not an ownership attestation for Kranz
or for separately copied prose/artwork. The unsupported Tauri application's
Rust dependency graph is outside these CLI archives and still needs its own
notice inventory before shipping desktop installers.

Regenerate with `scripts/check-rust-notices.sh --write` after a lockfile or
configuration change. Both CI and the release verification job rerun the
pinned generator and package checks. Dashboard regeneration uses the existing
build/sync/check pipeline. Review generated changes before accepting them.

Primary tool references: [cargo-about configuration](https://embarkstudios.github.io/cargo-about/cli/generate/config.html),
[cargo-about generation](https://embarkstudios.github.io/cargo-about/cli/generate/index.html),
and [Vite build.license](https://vite.dev/config/build-options#build-license).

## Validation

All dashboard gates passed (196 tests). The license inventory and package
checks passed. Archive tests inspect all five formats for notice bytes and
Unix executable permissions; missing runtime notices prevent packaging.
The copied-executable notice test passed. Full Rust workspace tests, clippy,
formatting, build, platform CI and actual archive construction are recorded
in the PR's final validation receipt as they finish; this document does not
predeclare their success.
