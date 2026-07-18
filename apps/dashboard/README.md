# Kranz Mission Control dashboard

React frontend for the Kranz server (`docs/protocol.md`), runnable two ways:
in a browser against `kranz serve`, or as a self-contained Tauri desktop app
that embeds the server.

In both modes the frontend discovers the server via the
`window.__KRANZ_SERVER__` global (a base URL like `http://127.0.0.1:4560`).
The desktop shell injects it with an initialization script; in browser mode it
is unset and the UI falls back to same-origin requests.

## Browser mode

Build the frontend, then let the CLI serve it:

```sh
cd apps/dashboard
npm install
npm run build          # emits apps/dashboard/dist
npm run sync-embedded  # updates the CLI's committed release bundle

kranz serve --open     # serves the API + the dist bundle, opens a browser
```

For frontend iteration you can also run `npm run dev` (Vite on
`http://localhost:5173`) alongside `kranz serve`; the server sends permissive
CORS headers, so the dev page can call the API directly.

## Desktop mode (Tauri)

The Tauri shell (`src-tauri/`) picks a free localhost port at startup, runs
`kranz_server::serve` on it inside the app process, and opens a 1280x800
"Kranz Mission Control" window pointed at the frontend.

Repo root resolution order: `KRANZ_REPO` env var, then the first CLI argument,
then the current working directory.

```sh
cd apps/dashboard
npm install

# development (starts Vite via beforeDevCommand)
KRANZ_REPO=/path/to/repo npx tauri dev

# release bundle — the frontend MUST be built first so dist/ exists for
# bundling (beforeBuildCommand runs `npm run build` automatically)
npx tauri build
```

Notes:

- `src-tauri/` is a standalone cargo workspace (empty `[workspace]` table in
  its `Cargo.toml`) so the heavy Tauri dependency tree stays out of root
  workspace builds. It depends on `kranz-server` / `kranz-engine` by path.
- The desktop app exposes two IPC commands as a fallback to the injected
  global: `get_server_url()` and `get_repo_root()`.
- The CSP in `src-tauri/tauri.conf.json` allows `connect-src` to
  `http://127.0.0.1:*` and `ws://127.0.0.1:*` because the embedded server's
  port is chosen at runtime.
