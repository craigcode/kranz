# Rustls TLS record-boundary security update

The 2026-09-14 pre-ACP housekeeping checks identified
[RUSTSEC-2026-0285 / GHSA-2mjx-qc3c-rqvc](https://github.com/rustls/rustls/security/advisories/GHSA-2mjx-qc3c-rqvc),
published that day. Both committed dependency graphs used rustls 0.23.44,
which is in the affected range. The maintainer identifies 0.23.45 as fixed.

The advisory concerns accepting TLS 1.3 handshake messages across encryption
level boundaries within a record. The maintainer states that the handshake
transcript remains authenticated; this is not evidence that a network attacker
can forge or complete an authenticated handshake.

Update only rustls to 0.23.45 in the root and standalone Tauri lockfiles, raise
the workspace's direct rustls requirement to that minimum, and regenerate the
CLI's third-party notices. Preserve the provider features and all existing
audit policy. No advisory exception, certificate-validation change or Kranz
event/API change is needed.

The integrating PR records the workspace, desktop, dashboard and dependency
audit results. These verify the dependency update and existing behavior; the
upstream advisory is the source for the protocol defect and its fix. Review
covers correctness of both locked graphs, the minimum-version requirement,
unchanged dependency/provider architecture, license notices and unchanged
security gates. No new runtime abstraction or performance work is introduced.

Release consequence: already-published v0.2.2 binaries retain 0.23.44. Merging
this fix does not replace those archives. A subsequent patch release must build
and verify new binaries before installed users receive the correction; the
successful v0.2.2 startup/provenance receipts do not negate this later advisory.
