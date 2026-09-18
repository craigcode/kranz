# ACP containment preflight

S6 remains open. This checkpoint closes a silent downgrade in the public
backend API; it does not enable enforced ACP sessions or certify an adapter.
S5's stage integration is on PR #63, with the Linux Docker stage proof passing
at `bbd7d6d`. S6 and S7 remain separate release requirements.

## Confirmed boundary and fix

Configuration rejects ACP with enforced sandbox settings, but callers can use
`AcpBackend::start(SessionSpec)` directly. That method previously ignored a
supplied `ResolvedSandbox` and spawned the peer on the host. It now refuses
before environment preparation, process creation or handshake. The regression
uses an executable peer that would leave a marker, supplies each resolved
backend variant, and verifies the exact refusal and absence of the marker.
Sandbox-off behavior and capability reporting are unchanged.

## What must precede enablement

1. **Supervise the whole boundary.** Existing ACP cleanup owns a process group,
   not a namespace. A child can call `setsid`. The existing Seatbelt profile
   permits process operations; merely wrapping the adapter does not establish
   descendant cleanup. Linux bubblewrap provides a PID namespace and
   `--die-with-parent`; macOS needs an independently proved lifetime mechanism.
2. **Keep container ownership explicit.** The current container egress boundary
   records an immutable host owner and cleans stale resources on later startup.
   That is recovery, not immediate cleanup after host death. A Docker-based ACP
   implementation must prove both adapter death and engine death, including
   stdin blockage, escaped children and daemon cleanup uncertainty. It must
   retain recovery evidence rather than report a clean stop without proof.
3. **Separate credentials from containment.** The compatibility probe's private
   native-login inputs do not configure ordinary missions. Certified startup
   needs a minimal private auth home and actual runtime/version receipts, while
   keeping `env_clear`, authority-file denials and protected Git metadata.
   No ambient HOME restoration or automatic Keychain access is authorized by
   the existence of a compatibility receipt.
4. **Run hostile and useful fixtures together.** Direct I/O, shell and nested
   children must fail outside the permitted roots; forbidden network access
   and token/Git attacks must fail without asking ACP permission. A normal
   peer must still create a real feature commit in its own worktree. Only then
   should config admission advertise the proved backend/platform combination.
5. **Prepare the live acceptance batch last.** Pin the supported adapter,
   runtime and boundary, then prepare the S7 workload and call/time budget.
   Fixture success alone is not a live adapter or authentication certificate.

## Local lifetime probe

On macOS arm64 with Docker client 29.7.2 and daemon 29.2.1 under Colima,
an offline Python container remained running after its attached Docker client
process group received `SIGKILL`. The fixture ignored stdin and forked a child
that called `setsid`; it did not use credentials, the network or host mounts.
The run used `--rm -i --network=none --read-only --cap-drop=ALL`,
`--security-opt=no-new-privileges` and `--pids-limit=32`, with image
`python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d`.
`docker inspect` still reported `State.Running=true` after client death.
Explicit `docker rm --force` removed the fixture; inspection reported no such
container and a successful daemon inventory contained no matching container.

This is a negative lifetime probe, not a complete test of Kranz's container
wrapper or a certified ACP run. It establishes that client process-group kill
and `--rm` alone do not own daemon-side lifetime. The first attempt did not
confirm immediate removal; a subsequent successful daemon inventory found no
matching container. A repeat retained the individual cleanup results and
confirmed removal through both inspection and inventory. Both probes were
removed; the first attempt's incomplete cleanup check is not a clean receipt.

The S6/S7 ticket bullets now use the parser's `Scoping answers` section and
single physical lines, so folded mission goals preserve all scope and exclusions.

## Five-axis review

Correctness: refusal occurs before spawn and prevents a false enforcement claim.
Security: the change narrows an existing public entry point; it grants no new
credentials, network access, writable roots or sandbox exceptions. Architecture:
the backend enforces its own admission contract while future wrappers must reuse
the existing sandbox and process primitives. Readability: the guard is adjacent
to the existing unsupported-resume guard. Performance: one option check before
startup, with no effect on streaming or retained state.

Validation: the targeted no-spawn regression passed, followed by the full
workspace suite with real Docker evaluator proofs enabled: 3,011 passed, zero
failed and ten existing ignored. Workspace Clippy with warnings denied,
formatting and build passed. Domain lint, staged secret scanning and knowledge
refresh passed. The CLI's folded goals retain both tickets' full scope,
acceptance criteria and exclusions. The first workspace attempt lacked the
running Docker environment and failed two existing container tests; restoring
that environment produced the complete passing run without changing those tests.
No provider calls or Keychain access were used. This is a self-review, not
independent containment acceptance.
