# S3 external evaluator review

Author self-review against the five repository axes, 2026-09-15 UTC. This is not
an independent audit. The branch follows S1 and reuses the strict JSON/owned
process primitives prepared in S2; no live provider call is part of this review.

## Scope and outcome

The engine library can pin a schema-5 checker from an approved Git ref, validate
its complete evidence pack and run one bounded JSON-RPC evaluation in Docker.
It accepts a result only after correlated bytes, successful exit and confirmed
container cleanup. It does not enable a mission-stage consumer: `load_for_config`
rejects configured external declarations until S5 is implemented. Existing
command gates, standards enforcement, waiver rules and persisted event/state
contracts retain their existing behavior.

| Axis | Review |
|---|---|
| Correctness | All five stage wire fixtures decode; host joins use exact bytes. Mandatory roles/provenance, stage selection, stale deadlines and artifact/line attribution are checked in code. Judged failure, escalation and execution failure remain distinct. Negative integration cases assert their expected error class, preventing a broken checker launch from masquerading as rejection coverage. |
| Readability | Separate modules own the wire types, trusted registration, frozen input bytes, control-plane I/O, container lifecycle and output import. Public API documentation states caller trust and unsupported surfaces. |
| Architecture | Extends the existing pack and command execution seams. The async driver does not wait for a human or change synchronous `Gate::evaluate`. No worker scheduler, prompt intelligence or new dependency was added. The duplicate-key parser is shared with the ACP backend without changing its policy. |
| Security | Checker bytes come from regular Git blobs at the approved ref; OCI runtime bytes are digest-pinned. Environment clearing, read-only inputs/checker, no network, private scratch, owned process groups and container namespace teardown enforce the boundary. No-follow opens verify file type/length/hash; aliases, traversal, hard links and FIFOs are refused. Default retention scrubs output and drops raw inputs; raw and retained hashes are separate. |
| Performance | Bounded argv/file inventory, 32 MiB checker dependencies, 128 MiB input total, 1 MiB maximum stream frames, 128 maximum output artifacts, wall/write limits and Docker resource limits prevent unbounded protocol buffering. Git pinning and one container per attempt add explicit startup cost; no daemon or pool is introduced. |

## Findings fixed during review

- The bootstrap shell added an environment variable after the initial clear.
  It now clears the environment again immediately before the checker executes.
- Files under differently cased parent spellings, and file/directory prefix
  collisions, are rejected before filesystem materialization.
- A pass response cannot bypass nonzero exit, trailing stdout, expiry during
  cleanup or a missing cleanup confirmation.
- Interrupted container creation is not treated as proved absent merely because
  a later listing is empty; uncertainty preserves its private recovery ledger.
- A raw stdout digest explicitly records whether raw stdout was retained. Output
  scrubbing identifies the actual scrub source bytes, not only a product version.
- A missing/invalid artifact cannot be read through a symlink/FIFO or credited as
  a valid finding. UTF-8/CRLF line counting is based on the supplied bytes.

## Validation

Final local full-workspace test, Clippy, formatting and build gates passed
(2,965 tests, zero failures, 10 ignored), including the Docker opt-in and the
path-alias regression. Remote CI is recorded in the PR.

The opted-in macOS/Colima suite executes an external Python checker, including
six valid-result scenarios, 19 invalid terminal/artifact cases, timed and signalled
cancellation, a child that creates a new session, caller-future drop and scrubbed
default retention. The dedicated Linux CI job pulls the same immutable OCI index
and requires the Docker capability. Portable schema fixtures pass (15 checks);
Actionlint and local domain lint pass. The workspace test also exercises the
existing gate order, engine floors, waivers, sandbox behavior and old logs.

## Limits and remaining integration

- S5 must select engine-observed mission inputs, apply dispositions and persist/
  replay lifecycle events. S3 does not manufacture consent or enable configured
  mission checks before that work exists.
- The new runtime is Docker-backed on macOS/Linux. Windows remains unavailable;
  its existing CLI and gates must continue to pass CI.
- Binary output redaction and symlink/submodule snapshot representation are not
  implemented; these cases fail readiness/import explicitly.
- Scrubbing is pattern-based, and per-file/import limits are not a hard aggregate
  disk quota. The host owns its attempt parent and any required disk quota.
- A killed host process cannot execute Drop. Cleanup uncertainty keeps a private
  ledger and never yields a result; automated crash/startup recovery is S5 work.
- Linux certification and the full remote regression suite must be green before
  S3 is marked done. S2's live Claude/Codex checks still need separate approved
  call budget and locally supplied API credentials.
