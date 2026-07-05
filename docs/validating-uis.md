# Validating UIs: live QA mode for the functional validator

By default the functional validator (`crates/engine/prompts/validator-functional.md`)
proves a milestone by running contract commands and reading their output. It
never opens a browser or looks at a screen. **Live QA mode** is the same
validator driving the actual running app instead — clicking through it,
reading rendered text, taking screenshots — and it turns on by convention,
not by a flag.

## The convention

If the functional validator's session has, beyond the standard
`Bash`/`Read`/`Glob`/`Grep` tools, access to any browser or computer-use tool
(anything that can open a URL, click, type, or view a running UI), the
prompt self-activates live QA mode. There is nothing else to enable: the
validator checks its own available tools and behaves accordingly. So turning
this mode on for a mission is purely a **config** change — add the tool to
`validatorFunctional.tools` and the prompt does the rest.

## Config block

Kranz config is layered: compiled defaults, then `~/.kranz/config.json`,
then `<repo>/.kranz/config.json` — later layers win, and any file may be
partial (only the keys you want to override). Keys are camelCase.

To enable live QA mode, set `validatorFunctional.tools` in whichever layer
you want (typically the repo's `.kranz/config.json`):

```json
{
  "validatorFunctional": {
    "tools": ["Bash", "Read", "Glob", "Grep", "<your-installed-browser-tool>"]
  }
}
```

**This `tools` list is the EXCLUSIVE `--tools` set for the functional
validator — it *replaces* the default tool set, it does not add to it.**
If you set it to just the browser tool, the validator loses `Bash`/`Read`/
`Glob`/`Grep` and can no longer run contract commands or inspect the repo at
all. You must always list `Bash`, `Read`, `Glob`, and `Grep` alongside
whatever browser/computer-use tool you're adding.

`<your-installed-browser-tool>` is a placeholder on purpose: the exact tool
name depends on whatever browser/computer-use capability your installed
`claude` exposes (an MCP browser server, a built-in computer-use tool,
etc.) — look at what tools your session actually offers and use that literal
name. Do not hardcode a specific tool name here; it isn't portable across
installs.

## Permission model

Setting `tools` alone isn't enough to make the browser tool *usable* — in
`-p` (headless, non-interactive) mode there's no prompt to approve a tool
call, so a tool that's in `--tools` but not in `--allowedTools` simply fails
every time it's invoked. The engine handles this for you
(`crates/engine/src/permissions.rs`): any extra tool configured in
`validatorFunctional.tools` beyond the standard inspect set is automatically
folded into that validator's `--allowedTools`, so the browser tool actually
runs. `Write`, `Edit`, `WebFetch`, `WebSearch`, and `git push` stay denied
regardless — the validator can drive and observe the app, but it can never
modify it, install into it, or commit.

## Worked example: driving `apps/dashboard`

`apps/dashboard` is a Vite + React frontend. Per its README, frontend
iteration runs via:

```sh
cd apps/dashboard
npm install
npm run dev          # Vite dev server on http://localhost:5173
```

With live QA mode enabled, a functional validator asked to verify a
dashboard milestone would:

1. Start the dev server the same way (`npm run dev` in `apps/dashboard`,
   per the README — it does not invent its own command).
2. Navigate its browser tool to `http://localhost:5173`.
3. Exercise the milestone's acceptance criteria live instead of just
   grepping source. For example, for a milestone that adds a mission list
   view:
   - Navigate to `http://localhost:5173`.
   - Observe that the page renders a list of missions (or an empty-state
     message if none exist) rather than a blank screen or a thrown error.
   - Click into a mission row and observe the detail view actually renders
     that mission's data.

For each criterion it exercises live, it records the **evidence**: the exact
URL visited (`http://localhost:5173`), and the exact on-screen text/state
observed (e.g. `"Missions" heading followed by a table row reading
"m-45624a — kranz otel"`), plus a screenshot if the tool supports one. If a
criterion can't be driven live (pure library logic, backend-only, tooling
can't reach it), the validator falls back to command/read evidence and says
explicitly that it wasn't exercised live and why.

## Evidence in the report

Live observations aren't a separate report — they fold into the same
findings/summary JSON the functional validator always emits. A live-driven
criterion becomes ordinary evidence text on its finding (or folds into the
summary when it passes cleanly); there's no second output shape to produce.
