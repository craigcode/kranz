# ACP resource qualification: R3 preparation review

Base: R2 integration head `d10fc745`. This change prepares measurement; it does
not complete R3 or admit production v2. The [scope and additive contract change](../scoping/acp-resource-budgets.md#r3-preparation-and-additive-contractchangerequest)
record the approved implementation boundary. The
[parent ticket](../../.kranz/tickets/acp-resource-budget-qualification.md)
remains open through live measurements, production admission and terminal work.

## Findings resolved

The installed adapter image has Node but no Rust compiler. Copying just Rust's
compiler did not supply a complete build environment: the offline test exposed
the missing system linker. The separate candidate now starts from the complete
pinned Rust image and copies the pinned Python, Node and adapter installations.
Both inputs are Debian bookworm ARM64. Its offline recipe downloads nothing,
uses no credentials and leaves released image/profile definitions unchanged.
The resulting candidate is pinned by immutable image ID, not its local tag.

R2 recorded failure counters but lacked useful-work high-water measurements.
The protected supervisor sample now includes kernel memory charge/task peaks,
CPU time and effective limits. Unsupported readings stay null. Memory charge
includes cache and kernel memory; it must not be described as process RSS.
Authorization/measurement checks reject missing counters, missing peaks, changed
limits, exhaustion, incomplete intervals and unconfirmed namespace removal.

Descriptor and file-size limits now have actual exhaustion proofs, in addition
to guest readback. They verify EMFILE and EFBIG respectively, recover, and finish
successfully. A handled limit error is not a failed attempt. The Linux lane
requires both this proof and positive kernel-accounting proof by name.

The proposed live experiment is a fixed offline workload, not an agent-authored
feature. Its shell command points to a read-only harness script. A manifest
binds the exact prompt/command, provider, explicit credential source, image,
profile, executable and workload/supervisor bytes before authorization. The
harness consumes each manifest once before reading credentials. It refuses a
second permission, added command/authority, uncertain delivery, different tool
identity, premature completion or tool activity after completion. A timeout or
failure consumes the allowance; it does not trigger a harness retry.

## Five-axis review

Self-review; this is not an independent agent review.

- Correctness: additive usage data preserves old records, missing data cannot
  become a clean interval, and effective limits must match the proposed bounds.
  Test counts exclude vacuous passes. Production defaults still need measured
  headroom and another qualification pass at those exact values.
- Readability: exploration ceilings, synthetic proof and live measurement are
  named separately. Receipts label measurement-only success and explicitly leave
  production qualification false.
- Architecture: measurement reuses the existing profile, backend, permission,
  sandbox, supervisor and cleanup paths. The workload and live harness are test
  code. No runtime scheduler, code-generation feature or new release config flag
  is introduced.
- Security: kernel usage retains the protected inode boundary. Preparation reads
  no credential; execution uses an explicit private file through the existing
  profile path. No Keychain, credential fallback, image pull or extra grant is
  added. Authorization is one prompt and at most one exact command, with time
  bounds but no hard dollar cap. Provider-internal retries are not bounded by the
  harness's no-retry rule.
- Performance: a few additional bounded cgroup files are read at R2's existing
  100 ms sampling interval. This affects only resource test revisions. The
  small workload measures adapter/toolchain overhead; it cannot establish
  capacity for a large repository build.

## Validation and remaining gates

The offline candidate workload builds and tests Rust (two tests) and checks and
tests Node (two tests), without dependency downloads. Its receipt includes
positive kernel accounting and confirmed namespace cleanup. The authorization
unit tests cover replay, changed bounds, extra commands, missing/duplicate
consent, second tools, premature reports and missing kernel evidence. Old sample
serialization is covered independently of the new optional usage record.

The full workspace suite passed 3,169 tests (zero failures, 14 ignored). Clippy
with warnings denied, formatting, build, strict docs, domain lint, knowledge
freshness, dependency audit, secret scan and all 17 gate-schema checks passed.
All 36 named containment tests passed without a skip marker: container proofs
used the new candidate image; ordinary-profile fixtures retained their existing
synthetic image. The offline preflight passed both workload/toolchain checks and
four authorization/accounting checks. No provider call was made. Live Claude/Codex runs remain separately authorized
and have not been executed by this change. A candidate-image pass on a macOS
Docker VM does not establish a native Linux or different-architecture tuple.
R3 remains incomplete until those measurements and headroom decisions are
reviewed; production profile admission and a release are subsequent gates.
