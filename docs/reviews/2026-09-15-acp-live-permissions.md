# ACP one-call consent review

S4 implements the permission lifecycle approved by the S1 contract. It adds a
bounded backend response handle, an engine-owned broker and CLI/API/dashboard/
Slack controls. This review is a local code review and deterministic test record;
it does not establish live Claude compatibility or ACP containment.

## Correctness

The request binds the exact action/options and RPC/tool/session identity to the
engine run, workspace and approved plan/policy. Reducer transitions reject stale,
duplicate, expired and foreign answers. A policy prohibition cannot be overridden
by an operator. Only unique one-time options can be selected, including refusals;
otherwise the backend cancels that invocation.

The engine persists the decision before the bounded response channel can supply
it to the output pump. Delivery is recorded separately as sent or uncertain.
The fake effect-boundary test reads the real log before either of two concurrent
workers performs its one effect, proving both ordering and duplicate-click
suppression. Restart closes records without recreating peer handles; the original
deadline remains unchanged.

Pause/policy-changing inbox entries cancel the live workers before they are
applied. The entry and later files stay unacknowledged, preserving control order.
The runner gives cancellation priority. Wall-clock and monotonic deadlines bound
the live wait; missing operator input never supplies consent.

## Readability and architecture

`live_permission.rs` contains the data and pure transition checks;
`orchestrator/live_permissions.rs` owns durable authority and response handles.
The existing runner, engine writer and signed control inbox remain in use.
Other backends have a default-absent responder. ACP buffers relay their preceding
events for durable consent; ordinary buffered runs keep their existing replay.

No scheduler, permission daemon, question-event workaround or mission-wide grant
conversion was added. Candidate linkage stays attached when buffered spawn events
reach the writer. Rejected peer requests fail their own session; durability
failures cancel and join all active workers before propagating the error.

## Security

The API requires mutation authority, rejects caller-supplied actor fields and
blocks permission commands through generic `/control`. Slack checks its existing
allowlist and records the interaction's actual user ID. Local capability authority
is named honestly instead of being presented as a verified human identity.

Complete action evidence is required for consent. Requests requiring normal
scrubbing are refused before retention; large Slack cards omit their allow
button. No new credential provisioning, Keychain access, inherited environment,
client filesystem/terminal capability or containment claim is introduced.

ACP remains cooperative and opt-in. Validator and enforced-sandbox combinations
remain refused. A peer acting without a callback is outside this control; S6
must prove containment before that claim can change.

## Performance

Pending requests are bounded at 16 per live session and 16 per mission, with
1,024 retained requests per mission and 64 KiB per proposal. Response channels,
broker channels and ACP event buffers are bounded. The writer services live
controls at 100 ms intervals; a persistence acknowledgement has a five-second
limit. Human wait does not borrow or block the session output pump.

## Validation

The targeted permission tests cover parallel effect ordering, continued output,
changed action, cancellation, restart, policy drift, response-channel loss,
redaction refusal, foreign sessions, expiry, duplicate/wrong bindings, control
inbox ordering, API token/actor rules, CLI parsing and Slack identity/allowlist
rules. Dashboard tests cover exact bindings, queued wording, duplicate clicks,
policy prohibitions, stale/expired states and API failure.

The reviewed workspace run passed 2,978 tests with zero failures and 10 existing
ignored tests. Workspace Clippy with warnings denied, formatting and build
passed. The native-login example's two tests and the synthetic compatibility
probe passed using disposable fixture data. Staged secret scanning and knowledge
refresh also passed. Two secret-scanner false positives in test variable names
were corrected by renaming the variables; scanner policy was unchanged.

Dashboard validation passed after `npm ci`: TypeScript, 244 tests, production
build, embedded sync/check and lint (18 existing warnings in untouched components,
zero errors). No live provider, Slack message or Keychain operation was used by
these checks. The separately retained Codex compatibility receipt is S2 evidence;
Claude's live proof remains outstanding.
