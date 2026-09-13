# Kranz landing page

A standalone, responsive product landing page. All authored site files live in
`dist/` and are intentionally tracked. There is no build step, package install,
frontend framework, remote font, analytics, or browser library dependency.

## Preview

From the repository root:

```sh
python3 -m http.server 4173 --bind 127.0.0.1 --directory apps/landing/dist
```

Open http://127.0.0.1:4173. Reload after editing the HTML, CSS, or JavaScript.
The feature sections work without JavaScript; JavaScript adds expand/collapse all.

## Railway deployment

The live URL is `https://kranz.craigmartin.com/`. The Dockerfile serves only
`dist/` with Caddy, listening on Railway's `PORT` (8080 by default). Railway
terminates public HTTPS. No database, volume, application secrets, build command,
or custom start command is needed. Set the service health check to `/`.

This folder is deployed as the `kranz-landing` service in the existing **Craig Martin
Website** project, production environment. The existing `www.craigmartin.com`
service uses `craigcode/cm-nextjs`; it does not need to be redeployed to add Kranz.

- Project: `17c12516-1a0b-4be5-9291-6b5a61c9b0fc`
- Production environment: `dab9bc7a-b381-4951-b3a4-1828b20616cc`
- Existing website service: `156dd55b-4c21-4e84-8385-0d18487d3cf7`
- Last successful GitHub-recorded website deployment: `9286f41d-2222-4cbc-b5be-0ba8ca78fb02`,
  commit `4564d0a131c9c4041d76ffec0cde281e3d72e5f2`, December 7, 2025.
  Verified against the active deployment before updating the site.
- Website update: deployment `cd519796-78dd-43d5-8795-805515a7d8fd` succeeded
  September 13, 2026. It adds the Kranz Projects card and patches Next.js to
  14.2.35. Source is local branch `codex/add-kranz` at `c7edb46` in `cm-nextjs`;
  it was uploaded with the Railway CLI and has not been pushed to GitHub.

The landing page service ID is `27dbd8d5-9b78-49a6-bcbc-0f83e21a90f8`.
Upload only this directory from the repository root:

```sh
railway up apps/landing --path-as-root \
  --project 17c12516-1a0b-4be5-9291-6b5a61c9b0fc \
  --environment dab9bc7a-b381-4951-b3a4-1828b20616cc \
  --service 27dbd8d5-9b78-49a6-bcbc-0f83e21a90f8
```

The upload and Docker context have explicit allowlists. Do not deploy the
repository root or use the existing website's service ID for this upload.

Add `kranz.craigmartin.com` under the new service's public networking settings,
targeting its HTTP port. Copy the exact CNAME and ownership-verification TXT
records Railway supplies into the `craigmartin.com` Cloudflare DNS zone, then
verify Railway domain status, HTTPS, page content, and all local assets.

Deployment `08bdda10-d958-4e0f-badd-e717e6c0ca58` built successfully on Railway
on September 13, 2026. Deployment `e7f56285-9bf6-45b1-9681-6a826f3f8b73`
then enabled the `/` health check and succeeded. The custom domain is live with
valid HTTPS, verified ownership, and these Cloudflare DNS records (CNAME proxied):

| Type | Name | Value |
| --- | --- | --- |
| CNAME | `kranz` | `6zwzlo7u.up.railway.app` |
| TXT | `_railway-verify.kranz` | `railway-verify=55c6e9ca37e8aecbcdd36f2e97c0fccfe1c001c425d1fcaa0223150097a4bfbb` |

`craigcode/kranz` is currently private; its links are intentionally
retained because the owner plans to make the repository public before launch.
The optional `.openai/hosting.json` describes
the static directory for Sites, but this deployment uses Railway.

For other static hosts, publish `dist/` directly with no build command.

## Content sources

Positioning follows `docs/knowledge/decisions/positioning-governance-evidence-layer.md`.
Feature copy follows `README.md`, `docs/agent-backends.md`, `docs/roadmap.md`,
`docs/tickets.md`, `docs/merge-gates.md`, `docs/metrics.md`,
`docs/scoping/worker-sandboxing.md`, and the Flight Rules design.
When they differ, prefer current implementation and the positioning ADR over
older quickstart prose. The mission card is explicitly illustrative.
Preview features and backend/containment limits must remain qualified.

## Product captures

`dist/images/web-mission-review.jpg` is a real capture of the existing Kranz
dashboard displaying recorded mission `m-6a20dc` (the July 12, 2026 Slack
workflow validation). Its source data was copied into a temporary local checkout
with automatic work disabled. No mission was run, approved, or modified to
produce the capture, and no transcript text or results were fabricated.

The image shows the scrutiny session near its final review. The caption identifies
it as a recorded mission; the dashboard's `live` indicator refers to its event-feed
connection. Click the image to inspect the original at full size.

A real Slack thread capture is pending access to the relevant conversation;
the page does not substitute a simulated Slack screenshot.

## Verification

```sh
node --check apps/landing/dist/script.js
```

Check desktop and mobile layouts, keyboard navigation, individual disclosures,
and expand/collapse all in the browser. Follow the repository's `AGENTS.md`
workspace gates before declaring changes complete.

The Railway Caddy configuration was validated with Caddy 2.11.4 and exercised
locally on port 4180: all four assets returned exact file bytes, gzip worked,
and missing files / configuration paths returned 404. A local container build
was unavailable because the Docker daemon was not running. Railway's container
build succeeded. Live HTTPS returned exact bytes for all four assets; desktop
layout, canonical URL, and expand/collapse controls were verified in the browser.
