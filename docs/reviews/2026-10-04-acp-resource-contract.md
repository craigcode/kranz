# ACP resource contract: R1 review

Base: public `a07467c2`. This reviews the combined scope in PR #93 and the R1
implementation in PR #94. The parent
[resource-budget ticket](../../.kranz/tickets/acp-resource-budget-qualification.md)
remains open: classification, production qualification and terminal budgets
are later slices.

## Findings resolved

The initial override validator admitted memory values below Docker's 6 MiB
minimum and CPU quotas below Linux's 1 ms minimum at Docker's default 100 ms
period. Validation now rejects memory below 6 MiB and CPU below 10 millicores
before creating a container. Every field has lower/upper boundary tests;
negative, fractional, overflowing and unknown fields are refused.

The scope previously admitted provisional production profiles before measuring
and proving them. R1 and R2 now remain fixture-only; production v2 admission
requires R3 qualification. The scope also distinguishes configured limits from
observed exhaustion, preserves uncertain failure attribution, includes the
host's 40-second terminal capture deadline, and treats proposed session budgets
as unqualified choices. The [scope](../scoping/acp-resource-budgets.md) records
the remaining decisions and evidence requirements.

The original Docker proof only created and inspected a container. It remains
as a flag-plumbing check. A second required proof runs a complete synthetic
mission through a v2 fixture profile with nondefault overrides. The guest reads
memory, swap, CPU and process limits from its cgroup v2 namespace and checks
descriptor/file-size limits through `getrlimit`. The host requires delivery,
permission evidence, unchanged primary source, disposed private home, and
matching profile/container receipts. A required guest witness is emitted only
after all readbacks pass, so losing the fixture trigger cannot silently skip
the assertions. Linux CI requires both named tests and
rejects capability skips. This checks propagation and useful synthetic work;
it does not qualify real adapters or resource-exhaustion behavior.

## Five-axis review

Self-review, not an independent review:

- Correctness: range checks precede startup; flags precede the image; the
  existing process limit is replaced once. Production v1 definitions carry no
  resources and reject overrides. Old profile JSON round-trips unchanged.
- Readability: profile defaults, overrides and effective values are distinct
  types; documentation distinguishes configuration receipts from enforcement.
- Architecture: only the ACP create path applies limits. Shared container and
  gate policy stay unchanged. Boxing the role's profile preserves serialized
  configuration and avoids growing already-sensitive hosted mission stacks;
  [stack headroom](../../.kranz/tickets/hosted-mission-stack-headroom.md) remains
  separate work.
- Security: project config and both runtime patch channels refuse resource
  overrides. Production v2 IDs remain unrecognized. No credential discovery,
  ambient environment inheritance or terminal capability is added.
- Performance: production v1 startup adds a small definition digest and receipt
  fields. The new end-to-end test uses an existing fixture mission; no provider
  call or external service is required.

## Validation boundary

The focused resource tests cover range admission, config authority and legacy
serialization. Merge requires full workspace tests, Clippy, formatting, build,
strict docs, dependency/domain/secret checks and the required platform CI jobs.
Docker is unavailable on the local review host; live namespace evidence must
come from Linux CI. PR check results and retained run logs record the actual
validation outcome for the final commit. No live provider session is part of
this slice.
