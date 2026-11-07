# Security audit remediation — v0.4.4 candidate

Reviewed against `47cc3a8f5fef7a79311f31ba16d7a8ab9556fb75` on 2026-11-07.
The implementation was prepared in an isolated worktree; the operator's primary
checkout and ongoing dashboard changes were not used as the release base.

## Findings and changes

1. Dashboard selection now uses the embedded application unless the operator
   explicitly sets `--dashboard` or `KRANZ_DASHBOARD_DIST`. Directory serving pins
   the selected root and opens each component without following symlinks. Assets
   and SPA fallbacks share that boundary; files must be regular and at most
   64 MiB. Repository-local and installation-neighbor discovery were removed.
2. Local bootstrap, readiness, clone, migrate, skew-check and reset commands carry
   the resolved worker gate policy through the workspace provider. They use the
   existing cleared, bounded sandbox runner and refuse unsupported containment.
   `off` remains an explicit environment-only posture. Container `disk.prune`
   host commands are refused before provisioning; owned Compose teardown remains.
3. The 0.4.4 candidate includes main's rustls-webpki 0.103.16 and Tokio 1.53.3
   fixes, and updates both root-store packages to 1.0.11. Version declarations,
   lockfiles, install references and dependency notices agree.
4. CLI serving requires read authority by default on loopback too. Development
   origins require `--dev-origins`; Tauri enables them only in debug builds.
   Read authority cannot mutate. The health endpoint stays public. Legacy
   library constructors retain their explicit compatibility policies.
5. The Linux desktop stack uses the exact upstream GLib iterator pointer fix
   backported to its compatible 0.18.5 line. Both unmaintained macro-error
   packages are absent; small local adapters use existing syn diagnostics.
   Original versions and licenses are retained. Hash and lockfile checks bind
   the advisory disposition to the patched source. See
   [backport provenance](../../apps/dashboard/src-tauri/vendor/README.md).

## Review and evidence

- Correctness: the workspace suite passed 3,227 tests, with 14 existing ignored
  tests and zero failures. Dashboard tests passed 266 tests across 37 files.
- Security: native macOS probes cover all six workspace command lanes, including
  allowed writes, denied outside writes, denied authority-file reads, denied
  network operations and fail-closed resolution. Every denied operation has an
  enforcement-off negative control. These prove the selected existing boundary;
  filesystem enforcement does not promise confidentiality for arbitrary host
  files, and its network policy is distinct from filesystem-plus-network mode.
- A built-binary HTTP probe confirmed embedded selection despite a planted repo
  dashboard, explicit override behavior, leaf/parent/authority/SPA symlink
  refusal, GET/HEAD authentication, read-token mutation refusal and explicit
  development-origin approval. It used only synthetic files and credentials.
- Optimized GLib tests passed iterator and derive checks. Two malformed derives
  produced their intended compiler diagnostics without macro panics. CI adds a
  full Linux desktop check; local macOS evidence does not substitute for it.
- Readability and architecture: the changes reuse the existing gate resolver,
  child runner, environment policy and capability filesystem utilities. No new
  execution primitive, event contract or mission push path was introduced.
- Performance: file opens/reads run off the async executor and asset reads are
  bounded. Directory handles pin the root and avoid canonicalize/open races.
  Workspace commands retain existing timeout and process-tree supervision.
- Local release gates passed: workspace build, Clippy with warnings denied,
  formatting, rustdoc with warnings denied, dependency policy, desktop advisory
  check, all dashboard gates, locked macOS Tauri check, source-hash verification,
  compiler diagnostics, public tree/history audits and knowledge freshness.

The GLib version still matches RUSTSEC-2024-0429: the audit configuration records
its verified source patch, not acceptance of the unfixed defect. No maintenance
advisory is ignored. The third-party GTK stack is not otherwise claimed to be
actively maintained by these narrow patches.

## Publication status

This is release-candidate evidence, not evidence of a published 0.4.4 archive.
Exact-commit CI, crate packaging, release rehearsal, platform archive smoke
checks and the protected release-environment approval remain required under
[the release procedure](../releasing.md).
