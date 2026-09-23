# One-call ACP permissions

ACP workers can pause one tool invocation for an operator decision while their
output continues to stream. This is separate from the existing mission-wide
command grants. Allowing one invocation does not add a command grant, lift a
deny rule or authorize future calls.

This feature is for the opt-in ACP backend. It does not enable ACP validators,
client filesystem/terminal services, session resume or enforced containment.
An adapter can act without requesting permission; these callbacks are not an
OS sandbox. The [compatibility guide](acp-compatibility.md) records the tested
adapter paths and remaining live-proof limits. Ordinary mission credential
provisioning remains separate from the explicit compatibility probe.

## Answer from the dashboard or CLI

A running `kranz serve` dashboard shows **One-call permission** with the exact
invocation, workspace, run and expiry time. Inspect the full action, then choose
**Allow once** or **Deny once**. Approval binding details identify the request,
approved plan and policy. “Answer queued” means the control inbox accepted the
answer; it does not claim that the adapter received it or that a tool succeeded.

From another terminal in the same repository:

```sh
kranz permission list <mission-id>
kranz permission allow <mission-id> <request-id> --binding <binding-digest>
kranz permission deny <mission-id> <request-id> --binding <binding-digest>
```

The list prints pending records as JSON. Use the `request.proposal.id` and
`request.bindingDigest` from the record you inspected. The digest is required:
a stale response cannot silently approve a changed invocation. These commands
queue a signed control record for the running engine; they do not launch an
agent, change its login or answer a terminated peer.

An engine prohibition cannot be overridden by these controls. Unknown effects,
mode changes, incomplete actions and missing or ambiguous one-time allow options
are refused. When normal evidence scrubbing would change an action, the run
stops instead of presenting a redacted preview as the full authorized action.
Scrubbing uses the existing detector and retains its documented limitations.

## Optional Slack

The existing [Slack bridge setup](../apps/landing/dist/guides/slack/index.html) supplies the channel, Socket Mode
connection and authorized `slack.allowUsers` list. Live permission notifications
use the existing blocked-notification preference. A verified, authorized Slack
interaction can allow or deny the exact invocation; a missing user identity is
refused even when the bridge deliberately allows all users.

The Slack card shows the action, mission, run, workspace and expiry. If the
complete action or workspace is too large
for a card, it offers a dashboard link when configured and only a denial button.
It never offers approval from truncated evidence. The dashboard or CLI can show
the complete request. No Slack setup is required to use either local surface.

## API and authority

`POST /api/missions/:id/permission/answer` accepts exactly:

```json
{"requestId":"permission-example","bindingDigest":"<digest-from-state>","allow":true}
```

Use the existing mutation credential in `X-Kranz-Token`. Read-only credentials
cannot answer. Successful enqueue returns HTTP 202 with `{"queued":true}`;
unknown, expired, closed or incorrectly bound requests return 409. The generic
`/control` endpoint rejects permission commands, and this dedicated body rejects
extra fields, including a caller-supplied actor.

Local CLI authority is recorded as `local-repository-authority`; local API
mutation authority is `local-mutation-capability`. These are capability
attributions, not verified human names. Slack records its verified user ID.
Policy refusals have a distinct `policy` actor. No evaluator or peer can mint
an operator identity through this interface.

## Lifetime and receipts

The engine is the sole writer of four additive events:

| Event | Meaning |
| --- | --- |
| `permission.requested` | Exact action/options, engine and peer session IDs, RPC/tool IDs, run/workspace, approved plan/policy digests and original deadline |
| `permission.resolved` | One operator decision or policy refusal, persisted before queuing any wire response |
| `permission.response-recorded` | The bounded pipe write was sent or its delivery is uncertain; not proof of a tool effect |
| `permission.closed` | No further response can be sent for this request |

Their default-empty `permissions` state projection and `kranz tail` display
retain the original request, decision and response times. Subtracting observed,
resolved and responded times separates operator wait from response delivery.
A missing delivery receipt means delivery was not established; never infer it
from a recorded decision alone.

Requests expire after five minutes. A monotonic live deadline and an absolute
persisted deadline prevent clock changes or restart from extending that limit.
The live backend permits at most 16 pending requests, the mission permits at
most 16 unresolved requests and 1,024 retained requests, and each proposal is
limited to 64 KiB. An unavailable operator eventually causes a failed run,
not an automatic allow.

A changed invocation, ended peer, cancellation or expired deadline cannot use
a queued approval. An expired request fails its own ACP session; sibling
candidates retain their live requests. The backend owns the session deadline,
while the engine closes the expired request's response capability.

While ACP workers are active, only one-call permission answers and messages
with `interrupt: false` apply immediately. Pause, resume, interrupting messages,
configuration changes, revision controls, command-grant decisions and structured
question answers cancel and join the active batch before they are processed.
This conservative boundary also covers controls that later prove stale or
have no effect; it preserves inbox order while authority can change or a new
controller turn can start. Later inbox files stay unacknowledged until the
workers stop. On restart, the writer closes outstanding requests; replay and
state reads never recreate an in-memory response handle. A lost or uncertain
response is not retried automatically.

Concurrent worker runs relay the events preceding a permission to the same
engine writer. The runner waits only for a bounded persistence acknowledgement,
then continues pumping while the operator decides. Rejected requests fail their
own session; log-integrity failures cancel and join the whole batch. Existing
non-permission buffered runs retain their deferred event replay.
