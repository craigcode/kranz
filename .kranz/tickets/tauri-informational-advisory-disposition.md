---
title: Resolve standalone Tauri informational dependency advisories
priority: 2
schedule: once
state: open
---

## Goal

Remove the seven informational RustSec warnings from the standalone desktop
lockfile through compatible upstream dependency upgrades, or record an explicit
per-advisory disposition supported by platform and call-path evidence.

## Context

The September 11 audit and remediation checked
`apps/dashboard/src-tauri/Cargo.lock` independently of the root workspace. Its
audit has no vulnerability-list entries but retains these informational warnings:

- `glib` 0.18.5: RUSTSEC-2024-0429, unsound `VariantStrIter` operations.
- `proc-macro-error` 1.0.4: RUSTSEC-2024-0370, unmaintained.
- `unic-char-property` 0.9.0: RUSTSEC-2025-0081, unmaintained.
- `unic-char-range` 0.9.0: RUSTSEC-2025-0075, unmaintained.
- `unic-common` 0.9.0: RUSTSEC-2025-0080, unmaintained.
- `unic-ucd-ident` 0.9.0: RUSTSEC-2025-0100, unmaintained.
- `unic-ucd-version` 0.9.0: RUSTSEC-2025-0098, unmaintained.

`cargo tree --locked --manifest-path apps/dashboard/src-tauri/Cargo.toml
--target all -i glib` traces glib through GTK/WebKit and Tauri 2.11.5. The
[glib advisory](https://rustsec.org/advisories/RUSTSEC-2024-0429.html) lists the
fix in 0.20.0 and later; a forced incompatible direct version does not replace
the GTK 0.18 dependency graph. No direct glib/VariantStrIter usage was found in
the desktop shell source; transitive reachability has not been exhaustively
excluded. The remediation therefore records this follow-up instead of adding
an advisory ignore or an unverified major GTK migration.

## Scoping answers

- Preserve desktop startup, embedded server, window behavior, and supported platforms.
- Keep the standalone Tauri and root workspace dependency policies explicit.
- Prefer an upstream-compatible upgrade over vendoring or a local fork.

## Acceptance hints

Capture before/after standalone audit JSON and target-specific dependency trees.
If upstream upgrades are available, build and exercise the desktop shell on
the affected platforms, then run the full Rust workspace and dashboard gates.
Any remaining disposition must name the advisory, the actual resolved package,
the platform and call-path evidence, and the condition that warrants re-review.
