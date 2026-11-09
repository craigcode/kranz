# ACP selected resource limits and host qualification

This review selects a bounded initial candidate from the completed R3
measurements and adds proof at those exact limits. It does not admit v2 profiles
or close the parent resource/terminal ticket. Base: `97d3c234`.

## Selected budget

| Resource | Selected value | Basis and limit of the claim |
|---|---:|---|
| Memory, no swap | 2,048 MiB | 3.13 times the larger measured cgroup charge peak; includes cache and kernel memory |
| CPU quota | 2,000 millicores | Same quota as both live measurements; the synthetic saturation case must observe throttling |
| Tasks, including threads | 256 | 3.32 times the larger measured task peak |
| Open descriptors per process | 1,024 | Successful live fixture and explicit EMFILE proof; no descriptor high-water measurement |
| Size per file | 128 MiB | Successful live fixture and explicit EFBIG proof; no aggregate disk quota |
| Size per writable tmpfs | 128 MiB | Exact `/dev/shm` size, all writable tmpfs mounts at or below the cap, and ENOSPC proof |
| Session wall clock | 600 seconds | Same ceiling as the live fixtures; the full deadline is exercised without shortening it |

Keep the initial admission proposal at these exact values: minimum, default and
maximum equal to the selected value. This intentionally supplies no wider
qualified override range. The current test-only profile's runtime-valid lower
bounds remain available to fixtures; they are not useful-work sizing claims.
A later production admission must enforce the selected range explicitly.
Broader ranges require another useful-work/enforcement review. The provisional
60-minute default and four-hour maximum in D-R5 are deferred, not qualified by
a ten-minute proof. They remain work for longer-session and terminal qualification.

The useful-work envelope is one approved native shell command executing the
unchanged read-only `qualification_workload.py`: a dependency-free Rust library
build and two tests, and Node syntax checking and two tests. Installed tools are
Rust/Cargo 1.94.1, Node 22.23.1 and npm 10.9.8. Downloads are zero. This proves
adapter plus toolchain overhead for that fixture. It makes no capacity promise
for a large repository, dependency installation, simultaneous builds, an
agent-authored feature, a complete governed mission or ACP terminal RPCs.

## Live measurements retained without another provider attempt

| Adapter | Observation | Memory charge peak | Task peak | CPU used | Session elapsed |
|---|---|---:|---:|---:|---:|
| Codex ACP 1.11.0 | 2026-10-06; reported `gpt-6-astra` | 578.90 MiB | 77 | 1.657 s | 14.271 s |
| Claude ACP 0.77.0 | 2026-10-08; reported `default` | 653.48 MiB | 39 | 1.988 s | 11.255 s |

These are single observations, not throughput benchmarks or variance estimates.
The conservative headroom decision retains the measured allowances rather than
reducing them. CPU, file, descriptor and tmpfs values are tested policy limits;
there is no fabricated peak-plus-headroom calculation for unmeasured quantities.
Claude's specific model is unknown. Neither adapter's default selection is a
promise about future model identity; qualify the adapter/resource envelope and
retain the reported identity on each run.

The original result bytes are preserved as public, credential-free
[Codex](evidence/2026-10-10-acp-resource-codex.json) and
[Claude](evidence/2026-10-10-acp-resource-claude.json) evidence. Their SHA-256 values
are respectively `34737668eb6685e3f90e50ad778d33bcb3bfde49b89a7a115482bbb91c4a83ad`
and `8be38783b876f800f87fb9ba655453976834746d4268da2460a9a0a81835deed`.
Each retains one session, one delivered permission, successful workload,
positive protected accounting, effective kernel limits, an unchanged primary
checkout and confirmed namespace removal. The historical `productionQualified`
field remains false. The original consumed allowances are not reused.

Before carrying these observations forward, their private preparation and
result digests were checked against the consumed attempt and review records.
The retained runner SHA-256 is
`29fc85465d7a6462660db91b8c5268797fad20ccf954ed1eb3a638c72b723c54`, built from
`23977f6b07cf54d9023b792da4e8c96f70351151`. No credential contents were read.
The new regression test binds the public result bytes, selected limits, current
supervisor and unchanged workload; changing one requires deliberate requalification.
This is provenance and drift detection, not a cryptographic attestation of a host.

## Host and image boundary

The selected proof tuple is macOS 26.6.2 ARM64 (build 25G83), Colima 0.10.3
using VZ, Docker client 29.7.2/server 29.2.1, Ubuntu 24.04.4 LTS guest with
kernel `6.8.0-100-generic`, cgroup v2/cgroupfs and runc 1.3.4. The VM has four
CPUs and approximately 6 GiB RAM. One candidate container is exercised at a
time. Both historical measurements report the same daemon/kernel/guest/architecture;
they did not record every client/VM detail, so this review does not backfill them.

The installed Linux ARM64 image is
`sha256:8da749ff0a9b09d5d4baa41172992eb92ca1beb9e5ed464feb7e608c5984457c`,
built by the existing offline qualification recipe. No image pull or rebuild
is part of this review. Its trusted supervisor hash is
`07d91f102a51fde11717939f51aaa25b5d8977b23762d08feb8a3365be161abe`; the workload
hash is `67f2a9ab59ed6d3648040d3087cb81234f7d504023a101a538dd35205d4f0d7a`.

| Host target | Resource qualification status |
|---|---|
| Exact macOS/Colima tuple above | Selected for this proof; results recorded below |
| Native Linux ARM64 | Not established by running a Linux guest on macOS |
| Other Docker/VM/kernel tuples | Not covered by this record |
| x86_64, Windows, other architectures | Unqualified |

A future v2 admission must refuse tuples outside its reviewed support policy
before credential use or worker spawn. The existing broad v1 macOS/Linux ARM64
allowance cannot silently become the resource revision's support matrix.

## Proof and reproduction

`acp_resource_qualification_selected_limits` runs nine sequential cases on the
pinned candidate: useful work, usage accounting, memory exhaustion, task
exhaustion, CPU throttling, EMFILE, EFBIG, tmpfs ENOSPC and actual 600-second
session expiry. Every case reads effective kernel limits through the protected
sample and checks owned namespace removal. Only memory, task and deadline
exhaustion fail the session. Handled errors and CPU throttling preserve success.
File-size enforcement uses a sparse file to avoid unnecessary host disk writes;
four CPU-bound children are necessary to saturate the selected two-CPU quota.

With that exact image already installed, use a trusted scratch root outside the
checkout and run from the repository root:

```sh
KRANZ_ACP_CONTAINER_TESTS=1 \
KRANZ_ACP_PROOF_IMAGE=sha256:8da749ff0a9b09d5d4baa41172992eb92ca1beb9e5ed464feb7e608c5984457c \
cargo test --workspace --locked acp_container::tests::acp_resource_qualification_selected_limits -- --exact --ignored --nocapture
```

Require the bare command exit code, exactly one passed selected-limit test,
nine case receipts and no `SKIP-ACP-CONTAINMENT` marker. The test is ignored in
ordinary CI because it requires the separately installed candidate image and
ten minutes of real deadline time. Existing named Linux CI cases continue to
exercise the same failure/cleanup machinery with smaller synthetic limits;
they do not certify this candidate image or native Linux production support.

The [selected-limit evidence record](evidence/2026-10-10-acp-selected-limits.json)
contains all nine namespace-bound receipts, source and runner hashes, historical
measurement references, the 36 named containment test results and gate log hashes.
The full deadline expired at 600,046 ms with `wallClockExpired: true`, a
complete protected sample and confirmed removal. All nine cases passed.

All four required workspace gates passed: 3,170 tests passed, zero failed and
15 explicitly ignored; Clippy with warnings denied; `cargo fmt --all --check`
(after formatting); and workspace build. All 36 named containment tests passed
without a `SKIP-ACP-CONTAINMENT` marker. Domain lint passed;
the public-tree audit and a credential scan of the evidence directory passed.
The additional ignored test is the ten-minute proof, which was run explicitly.
The final daemon inventory contained zero containers and zero owned egress
networks; Colima was restored to its original stopped state.

The first compilation failed because the accumulated temporary build cache
filled the host disk. Only regenerable incremental artifacts were removed;
validation was rerun successfully with incremental compilation disabled and two
build jobs. Source, retained provider receipts and the original prepared runner
were preserved. No provider session or credential read was added by this review.

## Five-axis review and admission boundary

Self-review, not an independent agent review.

- Correctness: the selected values equal the measured values; exact kernel
  readback and real exhaustion close the gap left by scaled-down fixtures.
  Original measurement receipts remain unchanged, and absent values cannot pass.
- Readability: public measurements, selected enforcement proof and production
  admission are distinct. The scope and unsupported tuples are explicit.
- Architecture: all executable changes are test code using the existing backend,
  supervisor and resource receipt. No new persisted event, configuration field,
  scheduler or prompt-routing mechanism is introduced.
- Security: synthetic runs carry no credentials and no provider egress. Neither
  worker text nor process exit codes can manufacture a confirmed resource class.
  Production v2 remains unknown; v1 definitions and behavior stay unchanged.
- Performance: exhaustion cases are sequential, bounded by the selected limits
  and an outer cleanup deadline. Production runtime work is unchanged.

Next admission review must implement and prove the exact override range and
host refusal policy, bind these evidence records to new v2 definitions, and run
release gates. General ACP terminals, longer sessions, other hosts and larger
workload claims remain separate qualification work. No runtime profile or
release is enabled by this evidence change.
