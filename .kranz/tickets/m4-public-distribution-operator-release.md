---
state: open
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

The history scrub is complete, but the repository is still private. GitHub
already has an older v0.1.0 release while crates.io has only 0.0.1 namespace
placeholders. Current main is substantially newer than the v0.1.0 tag, so
publishing it as crate version 0.1.0 would create conflicting provenance. The
checked-in Homebrew formula is also only a v0.1.0/all-zero-digest skeleton and
no tap repository exists. See
[the operator-readiness packet](../../docs/reviews/m4-m6-operator-readiness.md).

## Acceptance hints

- The human-only public-visibility gate is explicitly approved and anonymous
  clone plus rewritten history are rechecked before any package publication.
- Root workspace/dependency versions, Tauri versions, tag, crates, release
  assets, and formula all agree on 0.2.0 (or a later version explicitly chosen
  before implementation).
- Full workspace, dashboard, secret, advisory, and per-crate publish dry-run
  gates pass at the exact release commit.
- Crates publish bottom-up and each dependency is visible in the index before
  its dependant is published.
- The Homebrew tap uses the real tagged-tarball digest and current multi-agent
  product wording.
- Clean Linux/macOS/Windows installs reach `kranz --help`, `kranz init`, and
  `kranz ready` without a source checkout.
