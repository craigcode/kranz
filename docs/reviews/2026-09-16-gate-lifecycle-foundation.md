# Gate lifecycle foundation review

This is a checkpoint within S5, not completion of the lifecycle integration
ticket. Mission configuration still refuses external evaluators. No stage
adapter can consume a subprocess result through this change alone.

## Correctness

Four additive events record request, terminal attempt result, engine resolution
and consumption. Old logs omit the new default-empty state fields. Folding the
same events reconstructs the same records without running checks or recreating
response capabilities. Duplicate IDs/transitions, changed bindings, expired
consumption, wrong stage actions and failed process cleanup are rejected.

A judged result remains distinct from stage consent. Blocking failure/error
cannot become proceed; escalation remains require-human; advisory diagnostics
remain advisory. An operator cannot override the recorded nonwaivable mechanical
prerequisite. Invocation evaluations join the actual S4 permission's action,
options, workspace, run, peer and plan/policy. Consent attribution must match the
existing permission decision, and consuming its audit record sends no ACP reply.

## Security and evidence

Source snapshots include indexed, dirty and untracked nonignored regular files,
plus base-tree deletions. HEAD, inventory bytes and selection bytes are separately
bound. Rechecking captures again; the same HEAD does not hide a changed source
file or executable mode. Reads pin directory handles, refuse link traversal and
hard links, bound file sizes and do not block on special files. Conventional
provider, authority and mission-runtime paths are explicitly excluded. A snapshot
contains no shared Git mount. It is not an atomic filesystem transaction or a
claim that every ignored file is irrelevant; stage drivers still need stable
worker shutdown and appropriate environment/check-receipt binding.

The existing auditor export includes retained gate references, with distinct raw
and retained digests in the lifecycle record. Missing files, conflicting
references and bytes that no longer match the recorded retained digest become
unresolved entries. Hashes in the exported manifest describe the exported bytes.
The evaluator input and auditor bundle remain different products.

## Architecture, readability and performance

The existing event writer, reducer, provenance chain, CLI tail renderer and
bundle exporter are extended. No new scheduler, exporter, credential store or
agent backend is introduced. Runtime snapshots are capped at 10,000 labels,
128 MiB of source and 8 MiB per source file. Lifecycle history is capped at 2,048
attempts. The future FrozenEvidence builder must also respect its existing input
count and aggregate-byte bounds; these snapshot caps do not override those.

## Contract change record

The event/state additions implement the S1 `contractChangeRequest` proposals.
The path alphabet is additively widened to permit portable ASCII dotfiles and
interior spaces, required for `.gitignore`, `.github` and ordinary source names.
Both host validation and the v1 schema retain traversal, empty-component,
absolute/drive/stream path and trailing-dot/space refusals; the host additionally
rejects reserved device names and case/prefix collisions. No old accepted path
or existing log changes meaning. Non-UTF-8 source names fail readiness explicitly;
non-ASCII labels remain unsupported by this v1 path contract.

## Remaining S5 work

- Assemble minimal frozen inputs from the approved plan, selected scope,
  criteria, source snapshots, current check receipts and independent findings.
- Connect real approval, milestone, final and scratch-integration merge drivers,
  with pinned registration/policy and live-base drift checks at consumption.
- Join legacy gate receipts without inventing a pass for errors/escalation.
- Expose pending stage obligations through the operator decision surfaces and
  close interrupted attempts without replaying effects.
- Prove those real drivers end to end before removing the configuration refusal.

Claude live compatibility and ACP containment remain separate unproved work.
No provider or Keychain operation is needed for this checkpoint's tests.

## Validation

The full workspace suite passed 2,992 tests, with zero failures and ten existing
ignored tests, including the opt-in real container evaluator proofs. Workspace
Clippy with warnings denied, formatting and build passed. All 16 protocol schema
checks passed. The six source snapshot tests passed again after retaining the
exact source-selection bytes. Knowledge refresh passed. These local results do
not complete S5's stage-driver acceptance or the stack's missing provider proof.
