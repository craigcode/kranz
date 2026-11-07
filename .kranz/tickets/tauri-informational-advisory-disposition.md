---
title: Resolve standalone Tauri informational dependency advisories
priority: 2
schedule: once
state: done
state-note: "Closed 2026-10-18. 5 of 7 advisories (unic-*) were eliminated by the tauri 2.12.2 / tauri-build 2.6.3 / wry 0.57.0 upgrade; the remaining 2 Linux-only transitive advisories (RUSTSEC-2024-0429 on glib 0.18.5 and RUSTSEC-2024-0370 on proc-macro-error 1.0.4) have explicit per-advisory platform, call-path, and re-review dispositions in apps/dashboard/src-tauri/.cargo/audit.toml."
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
excluded. The remediation therefore records this follow-up. The existing
`apps/dashboard/src-tauri/.cargo/audit.toml` already accepts RUSTSEC-2024-0429
until Tauri moves to a fixed glib series; the standalone CI command
`cargo audit --deny unsound` passes with that policy. This audit added no new
ignore and made no unverified major GTK migration. Re-review the existing
acceptance when upgrading the GTK/Tauri dependency graph.

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

## Resolution & Evidence (2026-10-18)

### 1. Upstream Upgrade Elimination (5 of 7 advisories removed)

Upgrading the standalone Tauri lockfile (`apps/dashboard/src-tauri/Cargo.lock`,
634 dependencies audited) to `tauri 2.12.2`, `tauri-build 2.6.3`, and
`wry 0.57.0` completely removed the `unic-*` dependency chain from
`Cargo.lock`:

- `RUSTSEC-2025-0081` (`unic-char-property 0.9.0`): removed from `Cargo.lock`.
- `RUSTSEC-2025-0075` (`unic-char-range 0.9.0`): removed from `Cargo.lock`.
- `RUSTSEC-2025-0080` (`unic-common 0.9.0`): removed from `Cargo.lock`.
- `RUSTSEC-2025-0100` (`unic-ucd-ident 0.9.0`): removed from `Cargo.lock`.
- `RUSTSEC-2025-0098` (`unic-ucd-version 0.9.0`): removed from `Cargo.lock`.

### 2. Per-Advisory Dispositions for Remaining Linux-Only Transitive Warnings

The two remaining informational entries in un-ignored `cargo audit --json`
(`vulnerabilities.count = 0`) are recorded in
`apps/dashboard/src-tauri/.cargo/audit.toml`:

1. **`RUSTSEC-2024-0429` (`glib 0.18.5` — unsound `VariantStrIter` impls)**
   - **Target dependency trees (`cargo tree --locked --manifest-path apps/dashboard/src-tauri/Cargo.toml --target <triple> -i glib`)**:
     - `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`: empty (not compiled or linked on macOS or Windows).
     - `x86_64-unknown-linux-gnu`: `glib v0.18.5` $\leftarrow$ `atk v0.18.2` / `cairo-rs v0.18.5` / `gdk v0.18.2` / `gdk-pixbuf v0.18.5` / `gio v0.18.5` / `gtk v0.18.2` / `javascriptcore-rs v1.1.2` / `pango v0.18.3` / `soup3 v0.5.0` / `webkit2gtk v2.0.2` / `x11-dl` $\leftarrow$ `tauri v2.12.2` / `wry v0.57.0` / `tao v0.35.0` / `muda v0.17.2` / `tray-icon v0.22.0`.
   - **Call-path evidence**: `apps/dashboard/src-tauri/src/{lib,main}.rs` contains zero direct references to `glib`, `Variant`, or `VariantStrIter`. Upstream resolved this in `glib >= 0.20.0`, which requires leaving the GTK 3 (`gtk 0.18`) series used by Tauri 2.x on Linux.
   - **Re-review condition**: Re-evaluate and remove when Tauri/wry migrates its Linux WebKitGTK stack to `glib >= 0.20.0`, or if `src-tauri` introduces any direct `glib` call.

2. **`RUSTSEC-2024-0370` (`proc-macro-error 1.0.4` — unmaintained)**
   - **Target dependency trees (`cargo tree --locked --manifest-path apps/dashboard/src-tauri/Cargo.toml --target <triple> -i proc-macro-error`)**:
     - `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`: empty.
     - `x86_64-unknown-linux-gnu`: `proc-macro-error v1.0.4` $\leftarrow$ `glib-macros v0.18.5` ($\leftarrow$ `glib v0.18.5`) and `gtk3-macros v0.18.2` ($\leftarrow$ `gtk v0.18.2`).
   - **Call-path evidence**: Host-only procedural macro helper for `glib-macros`/`gtk3-macros` during Linux compilation; not linked into the desktop binary at runtime.
   - **Re-review condition**: Remove alongside `RUSTSEC-2024-0429` when Tauri/wry upgrades past `gtk 0.18` / `glib 0.18`.
