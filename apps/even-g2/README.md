# Kranz Mission Control for Even G2

This is a thin Even Hub client for Kranz. It renders mission state from the
existing REST API and lets an operator resolve two bounded human gates from an
Even Realities G2 or R1 ring:

- approve or deny a pending capability grant; and
- choose one of a structured question's supplied answers.

It does **not** approve plans, answer free-text questions, merge, publish, or
hold mission state. Those actions need more evidence or input than the glasses
surface can safely provide.

## Run the simulator with fixture data

```sh
cd apps/even-g2
npm ci
npm run dev:demo
```

In another terminal:

```sh
cd apps/even-g2
npm run simulate
```

The simulator opens `http://localhost:5174/?demo=1`. No Kranz server or token
is used in demo mode. It also exposes its official automation API on
`http://127.0.0.1:9898` so CI and local QA can capture the 576×288 framebuffer
and send up/down/click events without driving the native window.

## Run against a local Kranz server

Start Kranz with authenticated reads, then start the G2 development server:

```sh
kranz serve --read-auth
cd apps/even-g2
npm run dev
```

The Vite server proxies its same-origin `/api` to
`http://127.0.0.1:4560`. Override that development target with
`KRANZ_API_TARGET`. Paste the per-serve mutation token into the companion view
before making a decision. The token is stored only in `sessionStorage` and is
sent in `x-kranz-token`; it is never placed in the URL.

Live development refuses to start unless an unauthenticated `/api/repos` probe
gets `401`. This matters because Vite is LAN-facing for phone sideloading: a
loopback Kranz serve without `--read-auth` would otherwise become a tokenless
LAN read proxy.

On a multi-repository host, the client discovers `/api/repos` and selects a
healthy explicit default, the only healthy repository, or the only healthy
pinned repository. If selection is ambiguous, add `?repo=<id>` to the sideload
URL. Every subsequent read and decision uses that repository's
`/api/repos/<id>/...` scope, and the repository name is repeated on the review
and confirmation frames.

For physical glasses, enable Developer Mode and point the Even Hub QR loader
at `http://<development-mac-ip>:5174`. The phone and development Mac must be on
the same network, and the Even App must be 2.2.9 or newer (the floor declared
by SDK 0.0.14). The WebView still calls same-origin `/api`; Vite performs the
trusted loopback proxy to Kranz.

## Interaction

- swipe or rotate the R1 ring: move between missions or choices;
- tap or click the ring: open, review, and then confirm a decision;
- swipe on the confirmation screen: cancel; and
- double-tap: exit.

Every mutation requires a review screen followed by a separate confirmation
tap. A server rejection is shown without retrying or silently treating the
decision as accepted.

## Packaging boundary

`npm run pack` verifies that the app can be assembled as an `.ehpk`, but the
first slice is a development sideload, not a production Even Hub publication.
A hosted package needs a separately designed HTTPS relay/origin policy so it
can reach a Kranz server without weakening Kranz's Host, CORS, and token
boundaries. Do not publish the package with a baked-in serve token.

The app follows the official MIT-licensed Even Hub minimal/text-heavy starter
patterns. See [NOTICE.md](NOTICE.md).
