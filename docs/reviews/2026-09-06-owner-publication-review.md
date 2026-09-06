# Owner publication review — prepared, unsigned

Prepared 2026-09-06 UTC for the owner. **No confidentiality, ownership,
licensing, or publication approval is recorded.** This packet makes the
remaining decisions concrete after [unattended acceptance passed](2026-09-06-unattended-acceptance.md).

## Pinned scope

The accepted source commit is `d1eafe037df0dc08dd67e94889937fa66f56f7ac`, tree
`dba49abc9433473bcb189dc7feea014283d73035`.

- [Tree inventory](evidence/2026-09-06-release/owner-tree-inventory.csv): 949
  paths, each with its Git object ID and an unsigned decision field.
- [Advertised refs](evidence/2026-09-06-release/advertised-refs.tsv): 11 branch
  heads and 50 pull-request refs, no tags. The saved private audit mirror
  resolves every ref to the recorded object.
- [Scope summary](evidence/2026-09-06-release/owner-scope-summary.json): 1,461
  reachable commits and 5,607 unique blobs; 4,661 blobs are absent from the
  accepted tree. Historical path hints include 95 paths absent from that tree.
  The private history inventory includes every blob, its size, and a path hint.

Public visibility exposes the advertised branches and reachable history too.
In particular, the open Even G2 branch and other unmerged work are part of
that exposure even though they are outside v0.2.0's supported CLI scope.
Removing a file from main would not remove its old versions or PR refs.

These inventories cover the accepted base. The new receipt, investigation,
ticket, and review packet in this preparation change need delta review as
well. Local untracked idea tickets and the untracked application directory
were not included in the 949-file snapshot. Before signing, refresh the scope
for the final candidate and every then-current advertised ref; record both
the final SHA and refs-manifest digest.

## Decisions to make

All rows remain **pending**. The suggested order puts material requiring the
owner's provenance knowledge first.

| Review set | Concrete material | Owner decision / completion evidence |
| --- | --- | --- |
| Mission records | 160 files across 52 missions under `.kranz/missions/`, including plans, reports, and research | Confirm that requirements, examples, prompts, and incident narratives may be public. Classify any external-party or proprietary material in every reachable version. |
| Backlog and discussions | 233 ticket/sidecar files under `.kranz/tickets/` | Check copied briefs, private conversations, links, identifiers, and ownership of requested work. A completed ticket is not automatically publication-cleared. |
| Research and review prose | 25 review files, 31 scoping files, 11 knowledge notes | Verify provenance and permitted publication of quotations, paraphrases, and operational details. Start with the concrete items below. |
| Prompts and captured fixtures | 4 role prompts and 25 paths in fixture directories; additional captures under scoping | Confirm synthetic versus captured content, redaction, ownership, and redistribution suitability. Check session/account identifiers and local machine details, beyond credential patterns. |
| Artwork | 19 SVG/PNG/ICO paths, including desktop icons and embedded duplicates | Identify creator/source and allowed use of each distinct asset. Repository ownership alone does not establish artwork provenance. |
| Packages and embedded dependencies | Root MIT declaration, 4 crate archives, embedded dashboard, lockfiles, release payload construction | Resolve the measured notice gaps below and verify the actual packaged/distributed files. Record any required attribution or source provision decision. |
| Remaining source, docs, and history | 426 other source/doc paths plus history-only versions and other branch tips | Confirm contributor/employer/client rights and that internal paths or domain material have not escaped the private-pack boundary. Review unmerged branch content separately from main. |

The groups in the machine inventory are disjoint and total 949; the table
groups some related categories together for review. A `pending` row must be
resolved as `approved`, `needs-change`, or `excluded-with-history-action`, with
reviewer and evidence. A group approval is acceptable only if it explicitly
covers the pinned path/object inventory and the corresponding historical
versions. No automated field has been filled with a human verdict.

## Specific items found during preparation

1. **Crate notices are absent from the current package lists.** Fresh
   `cargo package --list --locked` calls returned zero for all four crates
   (162/18/35/42 entries respectively), but none listed a license or notice
   file. [Measured results](evidence/2026-09-06-release/package-notice-observations.json)
   retain that observation. Existing [PR #31](https://github.com/craigcode/kranz/pull/31)
   proposes per-crate MIT files and a package check; review it against current
   main and verify the resulting archives before treating the gap as closed.
   Its older narrative is not evidence of today's package contents.
2. **Binary and embedded-dashboard notice delivery is unproven.**
   `.github/workflows/release.yml` currently stages bare binaries. The tracked
   embedded JS has no copyright, MIT, or `@license` marker, and there is no
   tracked third-party notice file. Installed React, React DOM, Scheduler,
   and Zustand packages contain their own license notices. Determine and
   implement the appropriate notice delivery in the actual payload, then
   review its contents; a source SBOM is not a measured notice-delivery test.
3. **Dependency-policy coverage is narrower than all distribution rights.**
   The recorded `cargo deny check` passes the root Rust dependency policy.
   The dashboard lockfile has 184 dependency package entries with license
   metadata, including 12 Lightning CSS platform/toolchain entries marked
   `MPL-2.0`. Distinguish build tools from shipped code and verify the final
   payload rather than treating a lockfile label as a distribution verdict.
   The unsupported Tauri shell remains publicly exposed source even though
   signed desktop installers are outside this release.
4. **Captured third-party output needs a provenance decision.**
   [Cursor preflight](../scoping/cursor-probe-evidence/preflight.md) explicitly
   retains verbatim CLI help/output with identifier substitutions. Its
   accompanying JSON/JSONL/text fixtures retain temporary-directory paths.
   Several engine fixtures retain thread/session-shaped identifiers while
   [the later Codex probe](../../crates/engine/tests/fixtures/codex_exec_gpt_5_6_sol_probe.jsonl)
   uses redacted values. Confirm which are synthetic, and whether retained
   captures and operational detail are suitable for publication.
5. **External-source prose needs attribution and rights review.**
   [Local-inference scoping](../scoping/local-inference-executor-tier.md)
   explicitly adapts language from an external token-burn analysis.
   [Amp](ampcode.md), [Atomic](atomic.md), and
   [the skills-flow review](mattpocock-skills-flow.md) discuss third-party
   material. Verify original authorship versus borrowed text, source links,
   and the basis for retaining any quotations. This preparation does not
   certify external claims or grant permission on an author's behalf.

These are review leads and observed packaging gaps, not findings that every
listed artifact is confidential or unlawfully included. No source was deleted
or history rewritten while preparing this packet.

## Automated evidence and its limits

The earlier fresh-clone public-tree and public-history audits passed, with
Gitleaks reporting no leaks across 1,277 scanned commits. That scanner count
is distinct from the 1,461 reachable commits in the complete ref inventory.
The recorded Rust dependency policy and nine offline acceptance-harness tests
also passed. Full logs and original mission evidence are retained privately;
[the evidence manifest](evidence/2026-09-06-release/evidence-manifest.json)
records their hashes.

The tree/history scripts check configured operator markers and credentials.
They do not certify authorship, confidential business meaning, third-party
permission, or absence of all identifying metadata. The public domain-lint
salt and hashes do not replace review against the owner's private vocabulary.

## Sign-off record to complete

Keep detailed sensitive findings in the private review worksheet. Put only
neutral disposition IDs and cleared summaries into the source repository.

- Owner/reviewer: **pending**
- Review date: **pending**
- Final candidate SHA and advertised-refs manifest digest: **pending**
- Approved tree groups and historical scope: **pending**
- Exceptions, required edits/history actions, and proof of resolution: **pending**
- Contributor/client/employer rights and source-provenance confirmation: **pending**
- Package/binary/dashboard notice and license evidence: **pending**
- Content-cleared decision: **pending**
- Separate public-visibility authorization: **not requested or granted by this packet**

After content and licensing issues are resolved, rerun the audits on the
final candidate and refs. Obtain the separate visibility approval described
in [public readiness](../public-readiness.md), verify the anonymous clone and
publication controls, then follow [the release procedure](../releasing.md).
The release lock remains closed during this preparation.

## Preparation verification

This change adds documentation, evidence indexes, an unsigned review packet,
and a diagnostic reproducer. Runtime source, dependencies, workflows, and
persisted contracts are unchanged. Review covered correctness of the pinned
claims, readability of the owner decisions, the governance boundary, private
evidence handling, and the absence of runtime performance impact.

The workspace build, clippy with warnings denied, and formatting check passed.
The new ticket parses through the CLI. Evidence hashes, all 949 path/object
mappings, JSON, local document links, and unsigned review fields were checked.
The public-tree audit and staged Gitleaks scan passed; domain lint passed on
787 files with 17 skipped by its file-selection policy. Knowledge refresh
returned zero; its declared command check was not executed by the default
refresh mode.

The local workspace test run failed two container tests because the configured
Docker endpoint `unix:///var/run/docker.sock` was unavailable:
`container_authority_directory_hides_tokens_created_after_start` and
`live_bind_mount_round_trip_closes_under_the_checkout`. A direct Docker info
probe confirmed the missing socket. This is not a local all-green test claim;
the prior exact-source GitHub workflow remains the platform receipt. No test
was removed, weakened, or made to skip to obtain a pass.
The subsequent `cargo test --workspace --no-fail-fast` completed every target;
the same two tests failed and all other test targets passed (overall exit 101).
