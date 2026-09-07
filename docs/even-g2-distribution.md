# Even G2 distribution decision

Status: **prepared for operator scope selection; no package publication**.
The intended first public Kranz release includes an experimental G2/R1
companion, subject to [physical acceptance](even-g2-acceptance.md).

## Two different release scopes

| Scope | User experience | Remaining work |
| --- | --- | --- |
| Experimental source / QR sideload | Developer setup, Mac running locally, phone unlocked, trusted LAN | Merge reviewed client, complete hardware receipt, document limitations and include it in final release audits |
| Installable Even Hub app | Install through Even Hub; durable connection and phone backgrounding | Production connection design, pairing/credentials, lifecycle handling, manifest permissions, beta acceptance and portal submission |

The first is the proposed small go-live scope. It must be described as an
experimental sideload companion. It does not establish the second scope.

## Why the current package is not ready for the store

The app calls same-origin `/api`; the development server proxies those requests
to a local Kranz server. That proxy is absent from `dist/` and `.ehpk`.
The current manifest has no network permissions because it is not configured
for a production origin. A packaged build alone has no working live API route.

The vendor documents two separate network checks: manifest origin permissions
and browser CORS. Production destinations use HTTPS, with explicit allowed
origins. A manifest permission does not override CORS.
[Networking reference](https://hub.evenrealities.com/docs/build/networking).

A production design must decide:

- **Connection ownership:** an operator-hosted HTTPS companion gateway, or a
  managed relay reached through an outbound connector. The former leaves
  hosting with the operator; the latter creates hosting, privacy and support
  obligations for Kranz. Neither exists in this PR.
- **Authority:** explicit phone pairing, revocation, expiry and credentials
  scoped to the intended repository and bounded decisions. The current full
  serve token must not be embedded in a package, URL or QR. Design how the
  gateway keeps broader serve authority away from the wearable.
- **Exposure:** exact origins, TLS, preflights, denied permissions, CSRF and
  replay protection. Keep Kranz's loopback Host, CORS and token protections.
- **Recovery:** detect stale mission state after reconnect/phone unlock, require
  a new review before sending, and represent offline or uncertain sends honestly.
  Local session storage does not establish a production pairing lifecycle.
- **Operations:** installation/onboarding, updates, credential rotation,
  disconnected-host behavior, logs without secrets and support ownership.

A design review should pin those choices before implementing the connector.
Track that work in [the distribution ticket](../.kranz/tickets/even-g2-production-distribution.md).

## Package and submission preparation

`npm run pack` builds and locally validates `out.ehpk` with the pinned SDK floor.
It includes the Kranz license and third-party notices. CI verifies packaging;
it does not upload the artifact or claim that the packaged live API works.
The final production manifest needs its chosen origin permission and the final
payload needs a fresh notice/secret review.
[Packaging reference](https://hub.evenrealities.com/docs/ship/packaging).

After the connection design is implemented, use a private build to verify the
actual archive and permissions, then install the exact candidate through an
Even Hub beta group. Exercise the five-minute locked-phone test, system exit
dialog, permission denial and clean relaunch of other glasses apps; repeat
Kranz's stale-decision, duplicate-tap and event-log checks on that installed
build. QR testing cannot substitute for this installed beta receipt.
[Beta testing requirements](https://hub.evenrealities.com/docs/test/beta-testing).

Record the exact archive digest, source commit, device versions and beta
receipt before submission. Owner approval to publish Kranz or an Even Hub app
remains separate from engineering preparation. Repository visibility remains
private and `KRANZ_PUBLIC_RELEASE_ENABLED=false` during this work.
