# Gate evaluation contract v1 — design review

Five-axis self-review for S1, 2026-09-14. This is a design review, not an
independent security audit or a claim of runtime enforcement.

- **Correctness:** five stages have typed, mutually exclusive subjects. The
  response must correlate to the attempt, subject, policy, registration and
  exact evidence bytes. Judged pass/fail, escalation, engine disposition and
  authenticated consent are separate. A plugin cannot choose its stage,
  blocking policy, floor waiver or human identity. Test fixtures distinguish
  schema validity from host-only byte/correlation checks.
- **Readability:** the contract defines the ownership of each transition and
  explains its relation to existing gates and mission-wide grants. A compact
  authority/migration matrix avoids a universal untyped evidence bag. Schemas
  use local URN identifiers and share definitions without network retrieval.
- **Architecture:** no runtime wiring, second worker abstraction, new scheduler,
  backend default or persisted schema change is introduced. Explicit contract
  change proposals describe the later backend/event/state extensions. The
  existing pipeline and command/Flight Rules behavior remain unchanged.
- **Security:** future evaluators receive restricted inputs rather than full
  audit exports. The document requires containment, trusted checker bytes,
  bounded I/O, no-follow artifact import and original/retained hash distinctions.
  Findings require real input artifact anchors. It identifies the current gap
  between local capability authority and named-user authentication. These are
  specified requirements, not sandbox or live-adapter test results.
- **Performance:** this slice adds only documents, schemas and development
  validation. The Python checker uses pinned, hash-verified development packages;
  there is no new Rust runtime dependency. Fixture validation is offline after
  dependency installation and uses small synthetic inputs.

The development checks pass fifteen tests, including all five subject
families and fifteen positive request/result pairs, plus stage/version/binding,
forged-actor, malformed-response, path, deadline, byte-drift and finding-location
negative cases. Review also corrected regex end-of-string handling, restricted
manifest paths to the assigned input tree, and made line attribution depend on
LF in exact UTF-8 bytes, including CRLF and empty input cases.

Local repository checks passed: full workspace tests (2,940 passed, zero failed,
10 existing ignored), Clippy with warnings denied, formatting and build. Schema
checks pass in isolated Python 3.11 and 3.14 environments; the development
dependency lock was installed with required hashes. Actionlint, domain lint
(927 files), knowledge freshness and the engine source-package inventory passed.
All 36 schema, fixture and schema README files are packaged. No dashboard or Rust
runtime file changed. The integrating pull request carries CI results.

Process execution, permission races and old-log replay require S3/S4/S5 tests;
live adapter and containment proof remain S2/S6. The contract and fixture checker
are not production authorization, filesystem confinement or process supervision.
