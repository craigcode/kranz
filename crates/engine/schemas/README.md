# Gate contract schemas

These v1 schemas specify the external gate wire contract. They do not register
or execute a gate, change existing pack behavior, or prove runtime containment.
See the [contract](../../../docs/gate-evaluation-contract.md) for authority, byte identity, lifecycle,
resource limits, migration and the host checks that JSON Schema cannot express.

- `gate-common.v1.schema.json`: opaque IDs, hashes, Git object identities,
  portable artifact labels and the immutable binding tuple.
- `gate-evaluate-request.v1.schema.json`: one `gate/evaluate` JSON-RPC request
  with one of the five stage-specific subjects.
- `gate-evaluate-response.v1.schema.json`: judged/escalated results or typed
  JSON-RPC errors. Evaluators cannot declare human actors or policy authority.
- `gate-evidence-manifest.v1.schema.json`: the restricted input inventory,
  separate from the operator's full audit export.

Schemas and fixtures live inside the engine crate so registry source packages
carry the same contract bytes; the CLI does not execute these files in S1.

The `urn:kranz:gate:*:1` identifiers are schema identifiers, not network URLs.
The checker preloads local references and refuses remote schema retrieval.
Unknown versions and fields fail validation; references do not confer authority.

The synthetic fixture families in `fixtures/gate-v1` include exact subject and
manifest bytes, their actual SHA-256 digests, and pass/fail/escalation responses.
They exercise structure and joins only. Their deliberately minimal inventories
are not complete mission evidence packs or live adapter receipts. `cases.json`
selects all five families, and the checker requires exactly fifteen positive
request/result pairs before exercising invalid variants.

Run the development checks in an isolated Python 3.11+ environment:

```sh
python3 -m venv /tmp/kranz-gate-schema-venv
/tmp/kranz-gate-schema-venv/bin/python -m pip install --only-binary=:all: \
  --require-hashes -r scripts/requirements-gate-schemas.txt
/tmp/kranz-gate-schema-venv/bin/python scripts/check-gate-contract.py
```

On Windows use the environment's `Scripts/python.exe` executable. The pinned
validator is development tooling and is not included in the Rust application.
The fixture checker rejects duplicate JSON keys, verifies exact retained bytes
and response correlations, and checks finding anchors against input bytes.
It is not a production filesystem, authorization or process supervisor; follow-on
implementation needs its own hostile-process and legacy-log regression tests.
