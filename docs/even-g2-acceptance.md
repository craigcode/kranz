# Even G2 / R1 acceptance session

Status: **prepared; physical-device acceptance has not passed**. This is the
remaining gate for the experimental companion in Kranz's first public release.
Merging the client or packing an `.ehpk` does not close it. Track the result in
[the hardware ticket](../.kranz/tickets/even-realities-g2-demo.md).

## Bring to the session

- Charged, paired G2 glasses, R1 ring and your phone; record all firmware and
  phone OS versions. The pinned SDK 0.0.14 declares Even App 2.2.9 as its floor.
- Even Hub Developer Mode enabled for the same account as the phone app.
  Follow the vendor's [setup guide](https://hub.evenrealities.com/docs/get-started/quickstart/index).
- The Mac and phone on the same private, trusted Wi-Fi with device-to-device
  traffic allowed; keep the phone unlocked and the Even App in the foreground.
- Node 22.22.2 or newer, the reviewed Kranz build, and a **disposable repository**
  with no private source, credentials or production destinations.
- About 30–45 minutes, with the dashboard visible alongside the glasses so we
  can compare the decision and its eventual result.

The local sideload uses HTTP. A token in a header is still visible to an
observer of that connection. Use a disposable serve instance and trusted
network for this test. Its token has full serve mutation authority. Production
pairing and encrypted transport are separate [distribution work](even-g2-distribution.md).

## First prove the device interaction with fixtures

From the reviewed checkout:

```sh
cd apps/even-g2
npm ci
npm run dev:demo
```

In another terminal in that directory, substitute the Mac's actual LAN IP:

```sh
./node_modules/.bin/evenhub qr --url 'http://<mac-lan-ip>:5174/?demo=1'
```

Scan with the Even App's developer QR scanner. This path contains only fixture
missions and does not contact Kranz. Observe, on both temple and ring controls:

1. Swipe/rotate through missions; tap the blocked mission to open its question.
2. Read the whole question and chosen answer. Change the selection, tap to
   confirm, and check the exact answer, repository and mission identity.
3. Swipe on confirmation: return to choices and send nothing.
4. Reopen confirmation and tap deliberately: see `QUEUED`. Tap to refresh.
5. The fixture question is gone; open the remaining grant, cancel once, then
   approve it. Reset the sideload to repeat with denial.
6. Double-tap invokes the system exit flow. Record what the real device does;
   the final installed-app exit behavior also needs beta-package testing.

No clipped decision or missing confirmation footer is acceptable. In this
slice, questions/commands over 56 characters, options over 28, non-ASCII or
control characters, and oversized identities defer to the dashboard. Check
representative maximum-length text as well as the short fixture examples.

The [vendor's local testing guide](https://hub.evenrealities.com/docs/test/local-testing)
explains why locking or backgrounding the phone can suspend a sideload. Record
this limitation; a QR demonstration is not a locked-phone acceptance result.

## Then prove the live Kranz round trip

Stop the fixture server. Before this session, prepare a real disposable mission
whose approved plan deliberately reaches a bounded question or grant; obtain
its mission id and keep its worker/orchestrator running. For a question, use
`Which output format?` with `JSON` and `Text`; ask the worker to create the
chosen output only after the ordinary structured question is answered. A
separate disposable mission should exercise grant denial. Do not fabricate
`events.jsonl` entries to manufacture a passing receipt.

In the **disposable repository**, start the reviewed binary:

```sh
kranz serve --read-auth --port 4561
```

In `apps/even-g2` in the client checkout:

```sh
KRANZ_API_TARGET=http://127.0.0.1:4561 npm run dev
```

Generate a new QR with the live URL (remove `?demo=1`). Paste this disposable
serve instance's mutation token into the phone companion's password field.
Never put it in the QR, a screenshot, a command line or this receipt. If the
host exposes several repositories, use `?repo=<disposable-repo-id>` and verify
that exact identity on the decision and confirmation screens.

| Check | Required observation |
| --- | --- |
| No/wrong token | Missions or mutation unavailable; no successful decision |
| Read parity | Same pending decision and repository as the dashboard |
| Cancel | No queued decision and no matching decision event |
| Confirm question | Exact displayed option and index recorded as `question.answered` |
| Approve grant | Exact target recorded as `grant.approved` |
| Deny grant | `grant.denied`; denied capability remains unavailable |
| Repeat tap | One deliberate selection produces one decision; sending does not advance on extra taps |
| Stale decision | Resolve/change it on the dashboard first; glasses show an error and require fresh review |
| Connection loss | Clear error; no success claim and no automatic resend |
| Resume | After an affirmative decision, real mission execution continues and produces the expected output |

`QUEUED` is only an inbox acknowledgement. Close the acceptance gate only after
checking the ordinary event log and subsequent worker progress. If a gate was
resolved elsewhere during the test, repeat with a fresh pending decision.

## Receipt to fill in

Store raw logs and pictures privately outside the checkout. Commit only a
reviewed, token-free summary and links/digests suitable for publication.

```text
Date / operator:
Client commit / Kranz commit:
Phone OS / Even App / G2 firmware / R1 firmware:
Mode: QR sideload (not installed beta)
Disposable repository / mission ids:
Glasses and ring navigation, cancel, confirm, exit: PASS / FAIL
Full decision and footer legible, including length limits: PASS / FAIL
Grant approve / grant deny / structured answer: PASS / FAIL
Stale decision / bad token / disconnect: PASS / FAIL
Decision event types, ids or sequence numbers:
Subsequent worker progress / produced output:
Screenshots and event-log artifact paths / SHA-256:
Limitations / unresolved failures:
Operator acceptance:
```

Stop the disposable Kranz server and Vite after recording evidence; close the
phone WebView to clear its session token. Keep the hardware ticket open until
the required observations have receipts. The broader repository stays private
and release publishing stays disabled until separately cleared.
