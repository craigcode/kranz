# GTK 3 security backports

Tauri's Linux GTK 3 stack requires GLib 0.18; GLib 0.20 is not ABI/API-compatible.
These copies retain their upstream versions and MIT licenses. They are used only
by the standalone desktop workspace, never by the published Kranz CLI crates.

- `glib` 0.18.5: apply the two-line fix from
  [gtk-rs-core #1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343), merged as
  `05dff0ee696f9bcd8617cd48c4b812d046d440cb`. The C out-argument points to a
  mutable local through `&mut p`, addressing RUSTSEC-2024-0429.
- `glib-macros` 0.18.5 and `gtk3-macros` 0.18.2: replace `proc-macro-error`
  with a small Kranz-maintained diagnostic adapter using existing `syn` and
  standard-library facilities. It retains diagnostic spans, converts only its
  private abort marker into `compile_error!`, and resumes unrelated panics.
  No additional dependency is introduced. Both `proc-macro-error` and its
  `proc-macro-error2` fork are unmaintained (RUSTSEC-2024-0370 / 2026-0173).

The source archives were verified against the checksums in the existing
registry lockfile before extraction. `backports.json` records those archive
checksums, every upstream file hash, and every patched file hash. The normalized
`Cargo.toml` is authoritative; `Cargo.toml.orig` remains the untouched upstream
reference. `scripts/check-desktop-backports.py` refuses source drift, additional
patches, registry fallback, duplicate GLib versions, or the old macro helper.

The separate `../security-tests` workspace exercises all affected iterator
operations in optimized builds and a GLib derive using the replacement helper.
CI also checks the complete Linux desktop dependency graph. Remove these patches
and the accompanying advisory disposition when Tauri leaves the GTK 3 stack or
compatible upstream packages incorporate the fixes. Do not claim that the rest
of the legacy GTK bindings are actively maintained by these targeted patches.

The compiler-error fixture includes an empty package named `gtk` solely so
`proc-macro-crate` can resolve the expected crate name before rejecting a
malformed template derive. It does not implement or test GTK widgets. The
Linux CI job separately checks the real Tauri/GTK dependency graph.
