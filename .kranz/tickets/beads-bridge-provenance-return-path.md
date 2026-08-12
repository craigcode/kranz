---
state: done
state-note: Done: dispatch stamps external_ref=kranz-<slug(title)> (never clobbers); kranz-run-bead posts one deduped mission-id+outcome comment. EXTERNAL-REF/RETURN-PATH round-trip cases green on live bd 1.0.5 with all 5 selftests.
title: "Beads bridge: provenance + return path (D-BW-2 brief 2)"
priority: 2
schedule: once
---

# Beads bridge: provenance + return path

Decision source: `docs/scoping/beads-workstore.md` D-BW-2 (accepted
2026-07-29). Sister ticket: `beads-bridge-translator-correctness` (land
that first — this builds on the hardened translator).

## Problem

A bead created from a kranz ticket is currently a dead end in both
directions: the bead does not know which kranz ticket/mission it came
from, and the kranz side does not know which bead it became. For Gas City
interop the records must be navigable both ways, or every cross-system
hand-off becomes archaeology.

## Design (from the decision doc, locked)

1. **Outbound provenance:** on dispatch, record `external_ref =
   kranz-<slug>` on the bead (whatever field/label mechanism the hardened
   translator establishes in brief 1 — reuse it, don't invent a second
   channel).
2. **Return path:** on close of the kranz mission, write the kranz mission
   id back as a `bd comment` on the bead, including the terminal outcome
   (COMPLETED/FAILED/abandoned) so the bead reader sees how it ended
   without opening kranz.
3. **Refuse-and-comment preserved:** the spike's refuse path (a dispatch
   the kranz side rejects) keeps working and also leaves its reason as a
   `bd comment`.
4. **Idempotency:** re-running the return-path write for an
   already-commented mission is a no-op (comment-dedup on the mission id
   string), so retries never double-post.

## Test gate

- Round-trip fixture (extending brief 1's): dispatch carries
  `external_ref = kranz-<slug>`; closing the fixture mission writes
  exactly one `bd comment` containing the mission id; a second close
  writes nothing.
- Workspace gates green (bare exit codes, never piped): `cargo test
  --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check`, `cargo build --workspace`.
