# `backend_cursor` implementation brief

Superseded pointer — the authoritative route decision and implementation
brief now live in `docs/scoping/cursor-cli-backend.md`, under
`## Decision (2026-07-09, revised)`. Do not add a second brief or route
recommendation here; that document is the single source of truth.

See `probe-result.json` for the underlying evidence
(`.model_matrix`, `.permission_posture`, `.fixture_capture`,
`.recommendation_note`, `.acceptance_bar`, `.route_decision`).

Auth/permission requirement (ticket 3d8f93e, verified): a headless
`backend_cursor` worker/validator needs an inherited `~/.cursor` directory
or a `CURSOR_API_KEY` (equivalent bearer token), because Cursor's CLI auth
state under `$HOME/.cursor` does not survive a relocated `$HOME`
(`HOME=/tmp/x agent status` reports `Not logged in`). See
`## Permission and auth requirements (headless deployment)` in
`docs/scoping/cursor-cli-backend.md` for the full detail.
