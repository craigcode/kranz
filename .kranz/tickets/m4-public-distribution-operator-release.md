---
state: wontfix
state-note: "Closed 2026-08-13: the owner explicitly retained private repository visibility. The clean private-origin migration and audits are complete, but no public visibility, crates.io, Homebrew, or public release is authorized. Open a new ticket only after an explicit policy change."
title: Cut the first version-aligned public Kranz distribution
priority: 1
schedule: once
---

## Goal

After the repository owner authorizes public visibility, cut one coherent
0.2.0 release across GitHub source/binaries, the four reserved crates.io
packages, Tauri metadata, and a real Homebrew tap, then prove installation on
clean hosts without cloning the repository.

## Context

The clean-origin migration is complete: `craigcode/kranz` is the active private
repository and the legacy repository is retained as a private archived origin.
The public-tree/public-history audits passed at the recorded migration
boundary. Subsequent private-only GitHub rebase activity may carry operator
identity metadata, so that receipt is not a continuing publication claim. The
owner has chosen to keep the active repository private, and this release is
deliberately not scheduled. The historical v0.1.0 release remains only in the
private archive, while crates.io contains the 0.0.1 namespace placeholders. If
the visibility decision changes, create a new ticket, rerun the fail-closed
audits against then-current refs, remediate every result, and use the
version-aligned release procedure in
[the operator-readiness packet](../../docs/reviews/m4-m6-operator-readiness.md).

## Acceptance hints

- The historical v0.1.0 release stays only in the private archive, or is
  withdrawn/explicitly marked unsupported on an approved in-place route; its
  old binaries never silently become the public stable distribution.
- The human-only origin-migration/public-visibility gate is explicitly
  approved and both public audit scripts pass from an anonymous fresh clone
  before package publication.
- Root workspace/dependency versions, Tauri versions, tag, crates, release
  assets, and formula all agree on 0.2.0 (or a later version explicitly chosen
  before implementation).
- Full workspace, dashboard, secret, advisory, dependency-policy, rustdoc, and
  packaging gates pass at the exact release commit. Engine dry-run passes
  before publication; dependent dry-runs run bottom-up after each required
  sibling version appears in the registry (Cargo cannot resolve an unpublished
  sibling from crates.io merely because a workspace path exists).
- Crates publish bottom-up and each dependency is visible in the index before
  its dependant is published.
- The Homebrew tap uses the real tagged-tarball digest and current multi-agent
  product wording.
- Clean Linux/macOS/Windows installs reach `kranz --help`, `kranz init`, and
  `kranz ready` without a source checkout.
