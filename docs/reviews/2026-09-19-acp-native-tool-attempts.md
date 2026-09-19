# First contained native-tool attempts and probe corrections

The approved one-Claude/one-Codex batch at `0d41f41` consumed exactly two ACP
prompts, with no retries. Both attempts failed. Their [projected receipts](../compatibility/acp/native-tool-attempts-v1.json)
retain the original failure decisions, command/permission evidence, source
receipt hashes and independent empty daemon inventory. Projections omit text,
account/quota metadata and unrelated session updates; they are not full raw
transcripts. Digests refer to the pre-scrub proposals, not projected preimages.

Claude ACP 0.77.0 / Agent SDK 0.3.270 requested the exact Bash command, received
one synced `allow_once` decision with a separate `sent` receipt, and reported
successful completion. It then failed the egress check on
`http-intake.logs.us5.datadoghq.com:443`. That check runs before host file/report
verification and checkpoint creation: this attempt does **not** prove the file
contents or a feature commit. Adapter-reported cost was USD 0.063802; underlying
API request count and final billing are not independently verified.

Codex ACP 1.11.0 / Codex 0.153.4 announced the bare command, but its permission
proposal carried `/usr/bin/bash -lc 'echo kranz-acp-tool-fixture-v1 > fixture-result.txt'`.
The fixture refused that encoding before granting consent and confirmed an
aborted session. No tool completion or cost report was captured; absent cost
telemetry does not mean zero cost. The [pinned presentation code](https://github.com/agentclientprotocol/codex-acp/blob/51d6247ac7448485bfcf534b813196fafc26df59/src/permissions/presentation.ts)
uses [a shell-prefix stripper](https://github.com/agentclientprotocol/codex-acp/blob/51d6247ac7448485bfcf534b813196fafc26df59/src/CommandUtils.ts)
that does not strip this `/usr/bin/bash` form.

The unchanged allowlists blocked Claude telemetry; no domain was added. Both
containers and relay resources were independently confirmed absent. Credentials
came only through the approved local OAuth/file channels; no Keychain was used.
The 120-second prompt, 180-second session and 240-second outer limits were not
exceeded. These failures do not close S6 or authorize another provider call.

## Corrections and five-axis author review

- Correctness: the fixture accepts exactly the observed Codex wrapper as well as
  the bare command. Original proposals and digests remain intact. Synthetic cases
  reproduce the announcement/request difference and reject commands added inside
  or outside the quotes, another executable and a changed working directory,
  before any decision is emitted. Existing refusal cases remain required.
- Readability: both fixes stay in the explicit compatibility example. Its
  preflight displays Claude's fixed startup environment without applying it;
  the start receipt distinguishes application from preview.
- Architecture: production worker policy, event/state schemas, admission and
  client filesystem/terminal capabilities are unchanged. Synthetic fixtures
  exercise the existing contained transport; they do not qualify vendor behavior.
- Security: Claude receives `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` before
  adapter initialization. No egress rule is relaxed or denial ignored. Synthetic
  peers verify this fixed value across key, OAuth and file-login channels on
  native/contained paths, private homes and unchanged source login fixtures.
  Command matching remains literal, never shell parsing, prefix matching or
  arbitrary wrapper removal. Selected options still cannot carry extensions.
- Performance: one environment entry and one exact comparison, with no new
  dependency. The 29-case container suite remains serial and bounded; the
  provider batch retains its one-prompt/no-retry limits.

This is an author self-review, not an independent validator verdict. The
[documented Claude traffic policy](https://code.claude.com/docs/en/env-vars)
and static inspection of the native binary shipped with pinned SDK 0.3.270
support the startup change: its Datadog initializer checks the telemetry guard,
which consults this environment flag. The binary was copied from a stopped,
network-disabled inspection container, never executed during inspection; its
hash is in the [correction proof](../compatibility/acp/native-tool-fix-proof.json).
The fake peers prove environment delivery. Only a new approved live batch can
show whether that runtime now completes without denied egress.

## Validation and retained limits

The [correction proof](../compatibility/acp/native-tool-fix-proof.json) records
source/log hashes, raw gate exits, test counts and cleanup. Its synthetic checks
make no provider calls and use only invented fixture credentials. The positive
Codex case verifies the observed wire encoding; the fake peer executes the fixed
payload with its own `/bin/sh`. It is not a Bash-runtime qualification. An initial
test incorrectly tried to execute `/usr/bin/bash` in the minimal Python image,
which has no Bash. That failed log remains retained; the fixture was corrected
without changing an image or weakening a production boundary.

A subsequent synthetic run stopped on the existing mount preflight: Docker start
exceeded its deadline and removal was unconfirmed within the cleanup budget.
The `extra-authority` case correctly rejected this unrelated failure instead of
counting it as its intended permission refusal. No adapter event was captured.
The original receipt retains the recovery-ledger path; a separate later daemon
inventory confirmed absence. That infrastructure failure is retained separately
from the unchanged suite's follow-up results. No production timeout, cleanup
policy or retry behavior was altered.

The first expanded startup-test run also found a test assertion error: its
file-login case demanded that an unused dummy key literal disappear from the
logged fake-peer source argument. That value is not the file-login credential
and is not supplied to that channel's scrubber. The key-redaction assertion now
applies to explicit key/OAuth runs, matching the existing Codex checks; all
private-home, unchanged-source and traffic-policy checks still cover file login.
The failed test log is retained. No actual credential was read by these tests.

Linux synthetic CI passed the preceding `0d41f41` head, including all 24 earlier
tool cases: [external-evaluator job](https://github.com/craigcode/kranz/actions/runs/35471361270/job/105972797363).
That result does not cover these corrections or certify Linux vendor behavior.

Remaining work: a separately authorized bounded live batch, equivalent Linux
vendor receipts, admission restricted to qualified combinations, then S7's
independent-review/repair mission. Cooperative adapter telemetry cannot prove
that every native command sought consent or that no transient action occurred.
No release, merge or production enablement is claimed.
