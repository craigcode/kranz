# Kranz server protocol (REST + WebSocket)

Served by `kranz serve` (crate `kranz-server`, axum). The Tauri shell embeds the
same server and points its webview at it. The server NEVER writes
`events.jsonl` (single-writer rule §4.3) — writes go through the control inbox.

Base: `http://127.0.0.1:<port>` (default 4560). All JSON camelCase, matching
the engine's serde shapes.

## REST

| Method/Path | Response |
|---|---|
| `GET /api/missions` | `[{ "id", "status", "goal", "createdAt" }]` (folds each log; tolerate corrupt ones with `"status":"failed"` + `"error"`) |
| `GET /api/missions/:id/state` | full `MissionState` JSON (fold of events.jsonl; NOT the state.json cache) |
| `GET /api/missions/:id/events?since=<seq>` | `[Event]` with `seq > since` (omit `since` → all) |
| `GET /api/missions/:id/plan` | contents of plan.json (404 if not approved yet) |
| `GET /api/missions/:id/runs/:runId/transcript` | JSONL parsed into a JSON array of raw stream values (404 if missing) |
| `POST /api/missions/:id/control` | body = `ControlCommand` JSON (`{"kind":"msg","text":"...","interrupt":false}`, `{"kind":"pause"}`, `{"kind":"resume"}`, `{"kind":"config-change","patch":{...}}`) → `202 {"queued":true}` |
| `GET /api/health` | `{"ok":true,"version":"<crate version>"}` |

Static dashboard files served from a configurable dir at `/` (SPA fallback to
index.html).

## WebSocket `GET /api/missions/:id/ws?since=<seq>`

Server → client messages (JSON text frames):

```json
{ "type": "snapshot", "seq": 412, "state": { ...MissionState } }
{ "type": "event",    "seq": 413, "event": { ...Event } }
{ "type": "state",    "seq": 420, "state": { ...MissionState } }
```

- On connect: one `snapshot` (current fold + its seq).
- If `?since=` was given and the gap from `since` to head is ≤ 5000 events, the
  server instead replays `event` frames from `since+1` (no snapshot) — client
  keeps its state. Larger gap (or `since` ahead of head, or unparsable): fresh
  `snapshot`. Either way the client just handles both frame kinds.
- Live tail: server polls events.jsonl (`read_events_after`) every 250 ms and
  pushes `event` frames in seq order.
- After every NON-`worker.message` event (lifecycle), the server also pushes a
  `state` frame (full re-fold) — clients may use it instead of client-side
  reduction (the Rust reducer stays the single source of truth; the UI is a
  pure view).
- Client → server frames are ignored except `{"type":"ping"}` → `{"type":"pong"}`.

Reconnect contract (plan Phase 3 acceptance): client tracks the last `seq`
seen; on reconnect passes it as `?since=`; kill/restart of the dashboard or
server loses nothing because the log is the source of truth.
