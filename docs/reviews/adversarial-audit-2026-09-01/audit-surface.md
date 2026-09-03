# Kranz adversarial security audit — Tauri dashboard, CLI, supply chain, public-tree hygiene

Target: `craigcode/kranz` @ `33732c27` (main). Read-only audit; no files in the target tree were modified.

## Summary

1. The Tauri shell is the strongest surface here: real CSP with no `script-src 'unsafe-inline'`, `core:default` capabilities only (no shell/fs/http plugin), no `withGlobalTauri`, no `dangerousRemoteDomainIpcAccess`, and a hand-rolled markdown renderer that emits React nodes and no links — I found no webview-XSS or IPC-reachability path.
2. The CLI is the weak surface: agent-authored text reaches the operator's terminal with ANSI/OSC/DCS control sequences fully intact, on the default `kranz run` / `kranz exec` path and on the `kranz plan` approval screen — the exact screen the governance model depends on.
3. CI is unusually well hardened (every action SHA-pinned, `pull_request_target` used only in a trusted-base pattern that never executes proposed code, digest-pinned Docker bases, checksum-verified tool downloads, build provenance attestation) — I found no fork-PR secret-exfil or release-tampering path.
4. The gap in the release story is crates.io: `cargo install kranz --locked` is the headline install, and those crates are published by hand from a workstation with a long-lived token, entirely outside the attested pipeline.
5. Token handling has two soft spots: the mutation token is passed as an argv to `open`/`xdg-open`, and `http://tauri.localhost` is an unconditional CORS/WS origin exemption. Public-tree hygiene found no real credentials, but the audit scripts check far less than their names suggest.

---

# Findings

---

## H1 — Agent-controlled text reaches the operator's terminal with escape sequences intact

**Severity:** HIGH  **Confidence:** CONFIRMED (full path traced from agent stdout to `eprintln!`)

**Files**
- `crates/engine/src/runner.rs:127-155` — agent event text becomes `WorkerMessage.content`
- `crates/engine/src/scrub.rs:483-515` — `scrub_impl`, the only transform applied
- `crates/cli/src/output.rs:1049-1057` — `one_line`, the only sanitizer on the render path
- `crates/cli/src/tail.rs:683-690`, `crates/cli/src/tail.rs:736` — render + `eprintln!`
- `crates/cli/src/commands.rs:1272-1279` (`kranz run`), `crates/cli/src/exec.rs:391-398` (`kranz exec`) — the tail is spawned unconditionally
- `crates/cli/src/output.rs:138-206` (`render_plan`), `crates/cli/src/output.rs:70-133` (`render_status`)
- `crates/cli/src/commands.rs:1089` then `crates/cli/src/commands.rs:1101` — plan printed, then `approve? [y/N]`

**Attacker + preconditions.** Anyone who can influence what a worker/orchestrator/validator agent emits: a prompt-injection payload in a dependency README, a fetched web page, a test fixture, an issue body the agent reads, a hostile MCP server, or a compromised agent CLI. No operator interaction beyond running a mission is required. This is precisely the threat model Kranz exists for — the harness assumes the agent may be adversarial.

**What I verified.** `AgentEvent::Text { text }` is copied verbatim into `content` (`runner.rs:128`), passed through `scrub::scrub_and_truncate` (`runner.rs:154`), and appended to the event log. `scrub_impl` (`scrub.rs:483-503`) applies only credential-shaped regexes plus an entropy pass; it performs no control-character filtering — I read every rule pass. `EventRenderer::render` formats it into `body` and calls:

```rust
// crates/cli/src/tail.rs:684-685
let budget = LINE_MAX.saturating_sub(tag.chars().count() + 3);
let body = one_line(&body, budget);
```

and `one_line` is:

```rust
// crates/cli/src/output.rs:1049-1057
pub fn one_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    ...
}
```

`split_whitespace` splits on `char::is_whitespace`, i.e. the Unicode `White_Space` property. In the C0 range that is only `0x09`–`0x0D` and `0x20`. `ESC` (`0x1B`), `BEL` (`0x07`), `CSI` (`0x9B`), `DCS`, `ST` and `SOS` are **not** whitespace and pass through untouched. A payload containing no spaces or newlines therefore survives `one_line` byte-for-byte and is written straight to the operator's terminal at `tail.rs:736` (`eprintln!`). I grepped the whole `crates/` tree for any escape-stripping or control-character filter and the only hit is `crates/engine/src/review_artifact.rs:41` (`path.chars().any(char::is_control)`), which guards artifact *paths*, not rendered text.

Concretely reachable with a ≤160-char, space-free payload:
- `ESC ] 52 ; c ; <base64> BEL` — OSC 52 clipboard write. The operator's next paste is attacker-chosen text (classic `curl … | sh`).
- `ESC ] 8 ; ; <url> BEL` — OSC 8 hyperlink whose visible label differs from its target.
- `ESC ] 0 ; <text> BEL` — window/tab title rewrite.
- `ESC [ 2 J`, `ESC [ nA`, `ESC [ K` — clear/scroll-up/erase-line, i.e. repaint or erase lines already printed.

**The worse variant: the approval screen.** `render_plan` (`output.rs:138-206`) prints the orchestrator-authored plan — goal, milestone titles, feature titles, `feature.spec` **line by line** (`output.rs:198-205`), validation criteria, and the literal `command` string of each `AssertionCheck::Command`. `one_line` is not applied here at all and newlines are preserved, so the full cursor-addressing repertoire is available. `commands.rs:1089` prints it and `commands.rs:1101` immediately asks `approve? [y/N]`. An orchestrator whose planning context was poisoned can therefore print one plan and have the terminal display a different one: emit the real `Bash(...)` grant request, then `ESC [ nA` + `ESC [ K` to overwrite those lines with benign text before the prompt is drawn. The same applies to `render_status` (`output.rs:70-133`, printed at `commands.rs:79`) and to `backlog.rs:483`.

**Impact.** Operator clipboard write (→ arbitrary command execution when pasted); falsified consent screen for command grants and plan approval, defeating the review step the whole product is built around; terminal title/hyperlink spoofing. In a governance harness this is a bypass of the primary control, not merely cosmetic.

**Existing test coverage.** None. `crates/cli/src/output.rs` has an extensive `mod tests`, and `tail.rs` has renderer tests, but no test anywhere in the workspace asserts control-character behaviour — a repo-wide grep for `\x1b` outside `output.rs::ansi` returns only doc prose (`docs/scoping/cursor-probe-evidence/preflight.md:15`).

**Suggested fix.** Add one sanitizer in `crates/cli/src/output.rs` — e.g. `fn sanitize_untrusted(s: &str) -> String` that drops or picture-izes every `char::is_control` except `\n` and `\t`, and strips `\u{0080}`–`\u{009F}` (C1, including 8-bit CSI/OSC) — and apply it at the trust boundary: inside `one_line`, and to every field of `render_plan`/`render_status` that originates from a model (`goal`, `title`, `spec`, `statement`, `command`, `criterion`, `finding.evidence`, `reason`, `answer`). Do not rely on the caller remembering. Add a test asserting `\x1b]52;c;…\x07` and `\x1b[2A` do not survive. Note that `planning_tui.rs` is already largely immune because ratatui buffers by grapheme and drops zero-width cells — the plain `println!`/`eprintln!` paths are the exposed ones.

---

## M1 — `http://tauri.localhost` is an unconditional CORS/WebSocket origin exemption

**Severity:** MEDIUM  **Confidence:** PLAUSIBLE (code path fully traced; the "unprivileged port 80" premise is inferred, not demonstrated)

**Files**
- `crates/server/src/lib.rs:588-591` — `origin_allowed`
- `crates/server/src/lib.rs:652-672` — `ws_origin_allowed`
- `crates/server/src/lib.rs:552-561` — the `CorsLayer` predicate
- `crates/server/src/lib.rs:2286-2296` (doc comment) and `crates/server/src/lib.rs:848-870` — the loopback tokenless-read rule

**Attacker + preconditions.** A non-privileged local process (or a second local user) on a machine where the operator runs `kranz serve` on a loopback bind without `--read-auth`. The attacker binds `127.0.0.1:80` and serves a page; the operator (or anything that fetches for them) loads `http://tauri.localhost/`.

**What I verified.** The origin allowlist is otherwise carefully constructed — it pins the *IP*, not just the port, explicitly to defeat a co-resident process binding `127.0.0.2` on kranz's port (`lib.rs:576-584`). But the very first line short-circuits all of it:

```rust
// crates/server/src/lib.rs:588-591
pub(crate) fn origin_allowed(origin: &str, bind_addr: Option<SocketAddr>) -> bool {
    if origin == "tauri://localhost" || origin == "http://tauri.localhost" {
        return true;
    }
```

`http://tauri.localhost` with no port is origin-equal to a page served from `tauri.localhost:80`. `tauri.localhost` resolves to loopback under RFC 6761, so a local listener on port 80 can serve exactly that origin. `ws_origin_allowed` (`lib.rs:663-672`) delegates to the same function, so the WebSocket upgrade is approved too. On a loopback bind, `require_read_token` is false (`effective_require_read_token`, `commands.rs:2285`), so GETs and the WS upgrade need no token — the module doc at `lib.rs:568-571` states plainly that "on loopback binds GETs and the WS upgrade are tokenless, so this allowlist is what stands between an unrelated local page and mission state."

**What I inferred.** That an unprivileged process can bind port 80. True on Windows by default; on macOS/Linux it needs root or `CAP_NET_BIND_SERVICE` / a lowered `net.ipv4.ip_unprivileged_port_start`, so this is materially a Windows finding (or any box where something already serves on loopback:80).

**Impact.** Cross-origin, tokenless read of every mission: `/api/missions`, `/api/missions/:id/state`, `/api/missions/:id/runs/:run/transcript`, plus a live WebSocket feed. Mission transcripts contain repository source, tool output, and whatever the agent read — `.dockerignore:24` calls them secrets in as many words. Mutations still require the header token, so this is disclosure, not control.

**Existing test coverage.** `crates/server/src/lib.rs` has thorough `origin_allowed` tests (the 127.0.0.2 case is covered around `lib.rs:1471-1519`), but none exercises the `tauri.localhost` branch adversarially.

**Suggested fix.** Gate the Tauri exemption on actually being the Tauri shell — thread a flag from `apps/dashboard/src-tauri/src/lib.rs:82-91` (which already passes `read_auth: true`) into `TokenGate`/`cors_layer`, and accept `tauri://localhost` / `http://tauri.localhost` only when set. The Tauri build already requires the token on reads, so the exemption buys it nothing that the token doesn't.

---

## M2 — The mutation token is passed to `open` / `xdg-open` as a command-line argument

**Severity:** MEDIUM  **Confidence:** CONFIRMED (code path traced; argv visibility is standard OS behaviour)

**Files**
- `crates/cli/src/commands.rs:2426` — `let url = format!("{url}#token={token}");`
- `crates/cli/src/commands.rs:2858-2888` — `open_browser`
- `crates/cli/src/commands.rs:2288-2296` — the doc comment stating the token's purpose

**Attacker + preconditions.** Any other local user, or any unprivileged local process running as the operator, on a machine where the operator runs `kranz serve --open`. On Linux `/proc/<pid>/cmdline` is world-readable by default; `ps aux` shows full argv on macOS and Linux alike.

**What I verified.** `cmd_serve` builds the browser URL with the mutation token in the fragment and hands it to `open_browser`, which spawns `open <url>` (macOS), `cmd /C start "" <url>` (Windows), or `xdg-open <url>` (Linux). The comment at `commands.rs:2423-2425` reasons that "the fragment hands the token to the dashboard without it ever appearing in a request line or server log" — correct about HTTP, but the fragment is nevertheless in the argv of a separate process. The stated purpose of this token (`lib.rs:844-846`) is that "the token stops other local processes… from creating or steering missions that spend money", which is exactly the boundary argv crossing defeats. The token is also written to browser history via `location.hash` before `apps/dashboard/src/lib/token.ts:30-40` strips it with `history.replaceState` (replaceState does not remove the already-recorded entry's URL from all history back-ends).

**Impact.** Full mutation authority over the local serve: create and start missions, approve grants, approve plans, merge. Missions execute agent commands, so this is local code execution as the operator, and it spends money.

**Suggested fix.** Do not put the token in the opened URL. Options that preserve the UX: open the bare URL and let the dashboard fetch the token from the 0600 file the CLI already writes (`write_token_file`, `commands.rs:2665-2707`) via a loopback-only endpoint, or hand the *read* token (already generated alongside, `commands.rs:2401`, and explicitly described as "safe for dashboards and agents") to `--open` instead of the mutation token.

---

## M3 — crates.io releases are published by hand, outside the attested pipeline

**Severity:** MEDIUM  **Confidence:** CONFIRMED (verified by absence — no publish job exists in any workflow)

**Files**
- `README.md:42`, `README.md:86` — `cargo install kranz --locked` is the documented install
- `docs/releasing.md:125-147` — the manual publish runbook
- `.github/workflows/release.yml` — GitHub binaries only; no `cargo publish`, no `CARGO_REGISTRY_TOKEN`, no trusted-publishing/OIDC step

**Attacker + preconditions.** Anyone with code execution on the maintainer's workstation, or with the crates.io API token stored there. Notably, that workstation is by design running headless AI coding agents against this repo (`.kranz/missions/` is committed dogfooding evidence), and the CLI's own escape-injection exposure (H1) is a step toward that.

**What I verified.** `release.yml` builds five platform binaries, attests each with `actions/attest-build-provenance` (`release.yml:202-205`), attests `SHA256SUMS` and the SBOM (`release.yml:259-267`), and gates on a protected `release` environment (`release.yml:244`). None of that covers crates.io. `docs/releasing.md:125-147` prescribes running `cargo publish -p kranz-engine` … `-p kranz` locally, one at a time. I grepped `.github/` for `cargo publish`, `crates.io`, and `CARGO_REGISTRY_TOKEN` and found nothing.

**Impact.** The primary documented install path has no build provenance, no reproducibility claim, and no second pair of eyes. `cargo publish` uploads the working tree at the moment it runs, so the `audit-public-tree.sh` / `check-release-version.sh` gates enforced on the tag do not constrain what actually lands on crates.io. A long-lived registry token on a developer machine is a single-factor path to a poisoned `kranz` crate.

**Suggested fix.** Move publishing into `release.yml` behind the existing `release` environment, using crates.io Trusted Publishing (OIDC, `id-token: write`) so no long-lived token exists. Publish from the same checked-out tag the binaries are built from, and add `cargo publish --dry-run` to the `verify` job so drift is caught before the tag.

---

## L1 — `audit-public-tree.sh` checks far less than its name and its CI job title suggest

**Severity:** LOW  **Confidence:** CONFIRMED

**Files**
- `scripts/audit-public-tree.sh:7-20`
- `.github/workflows/ci.yml:32-37` — the `supply-chain` job runs it, and runs the history audit with `KRANZ_SKIP_GITLEAKS: "1"`

**What it actually does.** Four `git grep -I -n -F` passes for four hard-coded operator markers (a home-directory prefix, two email addresses, one project name). That is the entire tree-level check. It is not a secret scanner.

**What it would miss, concretely.**
- `-I` tells `git grep` to skip binary files outright, so a marker inside an icon, a `.ico`, or any file git classifies as binary is invisible to both this script and (for the same reason, `-G` skipping binary diffs) `audit-public-history.sh`.
- `-F` is case-sensitive: `/Users/CraigMartin` or an uppercase email passes.
- Content only — file *names* and *paths* are never inspected. A path like `docs/from-Users-craigmartin/…` passes.
- Four literals only. Any other home-directory shape, any second machine's username, any internal hostname, any API key, any other project name is out of scope by construction.
- In ordinary PR CI, gitleaks is explicitly disabled (`ci.yml:36`). Tree-level secret detection on a PR rests entirely on `secret-scan.yml`'s `kranz scan --range`; gitleaks runs only at release time (`release.yml:60-76`).

**Impact.** Hygiene only. I ran the searches this script does not: a repo-wide sweep for `xox[baprs]-`, `ghp_`, `github_pat_`, `sk-`, `AKIA`, `AIza`, `/Users/<name>`, `/home/<name>`, `.internal`/`.corp`/`.lan`/ngrok hostnames, real Slack team/webhook IDs, and 50+ char base64/hex blobs. **Every hit was a synthetic test fixture** (`crates/engine/tests/scrub_test.rs`, `crates/slack/tests/fixtures/*` using the public `T024BE7LD` sample workspace id) **or a placeholder** (`alice@example.com`, `/home/op/.kranz`, `ci@kranz.local`). The only real identity in the tree is `6720093+craigcode@users.noreply.github.com`, which is a GitHub noreply address and intentional. No live credential, internal hostname, or personal path is exposed.

**Suggested fix.** Drop `-I` (or add a companion binary-blob pass), add `-i`, extend the marker list to cover path components as well as content, and run `gitleaks detect --no-git` over the working tree in the PR `supply-chain` job rather than deferring all gitleaks coverage to release.

---

## L2 — `audit-public-history.sh` never inspects annotated tag messages

**Severity:** LOW  **Confidence:** CONFIRMED

**File:** `scripts/audit-public-history.sh:22-49`

The script covers three things: `git log --all -G"$marker"` over patches, `git log --all --format='%H%x09%B' | grep -F` over commit messages, and `git log --all --format='%ae%n%ce'` over author/committer identity. `git log` walks *commits*; the message body of an **annotated tag object** is never in `%B` and is never fetched by any of these. `check-release-version.sh:16-19` requires release tags to be annotated, so annotated tags are the norm here — a marker typed into `git tag -a -m "…"` ships publicly and passes the gate. The same `-G` binary blind spot from L1 applies.

**Suggested fix.** Add a fourth pass: `git for-each-ref --format='%(refname) %(contents)' refs/tags | grep -F "$marker"`.

---

## L3 — PR URL from `gh` stdout is rendered as an `<a href>` with no scheme validation

**Severity:** LOW  **Confidence:** CONFIRMED (path traced; exploitation preconditions are weak)

**Files**
- `crates/engine/src/pr_handoff.rs:199-214` — returns raw `gh pr create` stdout, trimmed, unvalidated
- `crates/server/src/rest.rs:750-765` — hands it back as `{"url": …}`
- `apps/dashboard/src/components/DeliveredPanel.tsx:143` — `<a href={prUrl} target="_blank" rel="noreferrer">`

`create_pull_request` correctly avoids shell interpolation (title/body go through argv), but its return value is `String::from_utf8_lossy(&output.stdout).trim().to_string()` with no check that it parses as a URL or that its scheme is `https:`. The dashboard renders it directly as an anchor href.

**Preconditions are genuinely weak:** controlling `gh`'s stdout means controlling `PATH` or the `gh` binary, at which point you already have code execution as the operator. I am also fairly confident React 19 (`package.json:21`, `react ^19.2.8`) throws on `javascript:` URLs in `href` rather than rendering them, which closes the interesting variant — though I could not execute the app to confirm, so treat that mitigation as inferred.

**Suggested fix.** Validate in `pr_handoff.rs`: parse the trimmed stdout with `reqwest::Url` (already a dependency), require `https` scheme and a `github.` host, and error otherwise. That keeps the check server-side where the value originates.

---

## L4 — The Tauri init script injects the mutation token into every document the webview loads

**Severity:** LOW  **Confidence:** PLAUSIBLE (init-script content verified; per-document injection semantics inferred from wry/Tauri behaviour, not executed)

**Files:** `apps/dashboard/src-tauri/src/lib.rs:142-152`

```rust
let init_script = format!(
    "window.__KRANZ_SERVER__ = {}; window.__KRANZ_TOKEN__ = {};",
    ...
);
WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
    ...
    .initialization_script(&init_script)
```

Tauri/wry initialization scripts are document-start user scripts registered on the webview, not on an origin — they run for every document the webview navigates to. There is no `on_navigation` guard and no origin check around the injection, so if the webview ever navigates to a remote page (the only agent-adjacent trigger I found is `DeliveredPanel.tsx:143`'s `target="_blank"` link, which is a github.com URL in practice), that page's JS can read the mutation token and the server URL.

**Why this stays LOW.** The impact is contained by the server, not by the client: `origin_allowed` (`lib.rs:588`) would reject a remote origin for CORS, `ws_origin_allowed` would reject the WebSocket upgrade, POSTs carrying `x-kranz-token` force a preflight that fails, and `?token=` reads are readable only with a matching `access-control-allow-origin`. So a leaked token is currently inert from a remote origin. The finding is that the containment lives entirely on the far side of the boundary.

**Suggested fix.** Add `.on_navigation(|url| url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost"))` to the builder so the webview cannot leave the bundled app, and open external links through an explicit opener rather than in-webview navigation.

---

## L5 — The browser-served dashboard ships no security headers

**Severity:** LOW  **Confidence:** CONFIRMED (verified by absence)

**Files:** `crates/server/src/lib.rs:305-332` (router + layers), `crates/server/src/lib.rs:509-530` (`embedded_static_response`)

The CSP at `apps/dashboard/src-tauri/tauri.conf.json:15` is a Tauri config value; it applies only to documents Tauri's custom protocol serves. When the same SPA is served by `kranz serve` over HTTP (the `--dashboard` / embedded path, and the documented LAN/tailnet deployment), I found no `Content-Security-Policy`, `X-Content-Type-Options: nosniff`, or `X-Frame-Options` / `frame-ancestors` on any response — a grep for those header names across `crates/server/src` returns nothing.

**Impact.** Today, nil in practice: the frontend never touches `innerHTML`, `dangerouslySetInnerHTML`, `eval`, or `new Function` (verified by grep across all of `apps/dashboard/src`), and `renderMarkdown` builds React nodes only. But the browser path has no defence in depth, so a future regression that introduces raw HTML would be directly exploitable there while the Tauri build stayed safe — a nasty asymmetry for a reviewer to reason about.

**Suggested fix.** Add a small `SetResponseHeaderLayer` stack to the router with the same CSP the Tauri config uses (minus the `ipc:` sources), plus `nosniff` and `frame-ancestors 'none'`.

---

## L6 — Internal mission artifacts are committed to the public tree

**Severity:** LOW (hygiene)  **Confidence:** CONFIRMED

**Files:** `.kranz/missions/*/plan.json`, `.kranz/missions/*/plan.md`, `.kranz/missions/*/report.md`, `.kranz/missions/*/research.md`, `.kranz/lessons/*.md` (42 files), `.gitignore:11-30`

`.gitignore` deliberately excludes the sensitive runtime artifacts — `events.jsonl`, `state.json`, `runs/`, `workspace/`, `config.json`, `serve.token` — and I confirmed via `git ls-files .kranz` that none of those are tracked. What *is* tracked is every mission's plan and report, which are long, detailed internal engineering directives (e.g. `.kranz/missions/m-d341a7/plan.json:80` contains operational instructions about a third-party CLI, including which of its commands are unsafe to run). This is presumably intentional dogfooding evidence, but it is a large body of internal working material that neither audit script inspects for anything beyond the four operator markers, and it grows with every mission.

I found no credentials or personal paths in this content.

**Suggested fix.** If it is intentional, say so in `docs/public-readiness.md` and add a checklist item for reviewing new mission artifacts before they land. If not, gitignore `.kranz/missions/` and `.kranz/lessons/` and keep the dogfooding record in a curated doc instead.

---

## INFO — Notes without a finding

- **`cargo audit` runs in one job only.** `ci.yml:426-429` installs a pinned `cargo-audit 0.22.1` and runs `cargo audit --deny unsound` for both the desktop and root lockfiles, but only in the macOS `tauri` job; `release.yml` has no `cargo audit`. Advisory coverage at release time comes from `cargo-deny check` (`release.yml:91-93`), which does include the advisories database, so the gap is narrower than it looks. Worth making explicit rather than implicit.
- **The domain-lint salt is public by design.** `.kranz/domain-denylist.json:3` ships a salt alongside the hashes it salts, which means the "banned terms" are recoverable by dictionary attack. This is not a defect — `crates/engine/src/domain_lint.rs:12-24` states plainly that the salt "is committed and therefore NOT [a secret]" and that its only job is to defeat precomputed rainbow tables. Flagging it only so a future reader does not mistake it for confidentiality.
- **CSP `connect-src` is broader than the embedded port.** `tauri.conf.json:15` allows `http://127.0.0.1:*` and `http://localhost:*`. The port is only known at runtime so it cannot be pinned statically; the practical exposure is nil given `script-src 'self'` with no `unsafe-inline` and no injection sink in the frontend.

---

# Areas checked with no finding

**Tauri configuration (`apps/dashboard/src-tauri/`).** CSP is set with `default-src 'self'` and `script-src 'self'` — no `unsafe-inline`/`unsafe-eval` on scripts (`tauri.conf.json:15`); `style-src` carries `unsafe-inline`, which is inert without a script sink. Capabilities are `["core:default"]` only (`capabilities/default.json:8-10`) — no shell, fs, http, dialog, or opener plugin. `Cargo.toml:25-34` confirms the only plugin dependency is `tauri-plugin-log`, and `lib.rs:99-105` registers it under `cfg!(debug_assertions)` only. `withGlobalTauri` is absent (defaults false); `dangerousRemoteDomainIpcAccess` is absent; `assetProtocol` is absent; no `devtools` feature is enabled in `Cargo.toml`, so release builds have no inspector. Only two IPC commands exist (`get_server_url`, `get_repo_root`, `lib.rs:22-31`) and both are read-only accessors over managed state. Port binding avoids the pick-then-release TOCTOU by keeping the listener (`lib.rs:54-60`).

**Frontend rendering of untrusted content (~12k lines of TS).** Zero occurrences of `dangerouslySetInnerHTML`, `innerHTML`, `insertAdjacentHTML`, `document.write`, `eval`, `new Function`, `srcdoc`, `iframe`, `window.open`, `shell.open`, `openUrl`, `URL.createObjectURL`, or `javascript:` across `apps/dashboard/src`. `renderMarkdown` (`lib/markdown.tsx`) is hand-written, emits React nodes exclusively, and — importantly — implements no link syntax at all, so `[label](javascript:…)` in agent output cannot become an anchor. Its 14 call sites (transcripts, plan markdown, reports, ticket goal/context, orchestrator text) are all safe by that construction. All hrefs except `DeliveredPanel.tsx:143` come from `missionHash`/`ticketHash`, which `encodeURIComponent` their input (`lib/routes.ts:66-68`). Route parsing tolerates malformed percent-encoding without throwing (`routes.ts:11-19`).

**Frontend token handling.** `sessionStorage`, never `localStorage` (`lib/token.ts:24,44,54`) — the token dies with the tab. Read from `window.__KRANZ_TOKEN__` first, then a `#token=` hash which is stripped via `history.replaceState` (`token.ts:30-40`). Sent as the `x-kranz-token` header on both GET and POST (`lib/api.ts:134,162`), never as a query parameter except on the WebSocket upgrade, where browsers cannot set headers (`lib/ws.ts:137-138`) — a documented and correct exception. WS URL is derived by rewriting the scheme of the trusted origin (`ws.ts:133`) with `URLSearchParams` for the query and `encodeURIComponent` on the mission id (`ws.ts:143`); no string concatenation of untrusted values into the URL. Background polls opt out of the token gate so they cannot pile waiters onto the prompt (`api.ts:117-123`).

**GitHub Actions supply chain.** Every third-party action is pinned to a full commit SHA with a version comment — `actions/checkout@3d3c42e5`, `dtolnay/rust-toolchain@4be7066a`, `Swatinem/rust-cache@6323deb1`, `EmbarkStudios/cargo-deny-action@3c634983`, `anchore/sbom-action@e22c3899`, `softprops/action-gh-release@3d0d9888`, `github/codeql-action@5595ccaf`, `actions/attest-build-provenance@4d101475`, `actions/upload-artifact@043fb46d`, `actions/download-artifact@3e5f45b2`, `actions/setup-node@82076278`. Default `permissions: contents: read` at workflow level in all five workflows, with per-job escalation only where needed. **No expression-injection sinks**: I grepped every workflow for `github.head_ref`, `pull_request.title/body/head.ref/head.repo`, `issue`, `comment`, `review`, `head_commit`, and `inputs.*` inside `run:` blocks — zero hits; the only interpolated values are SHAs, matrix entries, and `runner.temp`. The `ANTHROPIC_API_KEY`-bearing `smoke` job is `workflow_dispatch`-gated (`ci.yml:517`) and pins `@anthropic-ai/claude-code@2.1.206` with an explicit comment about why (`ci.yml:529-534`). No `dependabot` auto-merge workflow exists.

**`pull_request_target` usage.** Both `secret-scan.yml` and `domain-lint.yml` use it, and both implement the trusted-base pattern correctly: check out the **base** SHA (`secret-scan.yml:43`), fetch the PR head as inert git data with a SHA equality assertion against the event payload (`secret-scan.yml:60-65`), build the scanner from the base worktree, then run the trusted binary over the proposed range. `domain-lint.yml:97-112` goes further and overlays the *trusted* policy files onto the proposed tree so a PR cannot un-ban a term or waive its own leak, with a documented bootstrap path that never substitutes the proposed policy. No proposed code is ever executed. `workflow_dispatch` is deliberately omitted from `secret-scan.yml` with the reason written down (`secret-scan.yml:6-8`). I also considered cache poisoning via `Swatinem/rust-cache` in these `pull_request_target` jobs (where `github.ref` is the base branch, so saves land in the main-branch scope): every input to the cached build comes from the trusted base, and rust-cache keys include the job name so `release.yml`'s build cache is a different scope regardless.

**Downloaded-tool integrity.** Both external binary downloads verify a pinned SHA-256 **before** extraction: Gitleaks 8.29.1 (`release.yml:60-73` — `curl`, then `sha256sum --check --strict`, then `tar -xzf`) and Gas City CLI 1.3.2 (`ci.yml:54-67`, same ordering). No `curl | sh` or `irm | iex` pattern anywhere in the repo (grepped `.sh`, `.ps1`, `.mjs`, and all workflows).

**Dockerfile.** Both stages pinned by digest (`rust:1-slim@sha256:8e8cf8f7…`, `debian:stable-slim@sha256:1710bde3…`). Non-root `USER kranz` (uid 10001) in the runtime stage. Only the stripped binary is copied from the builder, so no build cache or source leaks into the final image. No secrets baked in — `ANTHROPIC_API_KEY` and deploy keys are documented as run-time inputs (`Dockerfile:66-69`), no `ARG`-passed secret, no `--mount=type=secret` needed because none is used. `.dockerignore` excludes `.git/` and `.kranz/`, with a correct explanatory comment about the one re-inclusion the build needs.

**`deny.toml` and lockfiles.** `[advisories] ignore = []` — no unjustified advisory suppressions. Explicit license allowlist with `exceptions = []`. `unknown-registry = "deny"`, `unknown-git = "deny"`, `allow-git = []`. Both `Cargo.lock` files contain zero `source = "git+…"` entries. Spot-checked versions are current and past the relevant advisories: `ring 0.17.14`, `idna 1.1.0`, `url 2.5.8`, `tokio 1.53.1`, `hyper 1.10.1`, `axum 0.8.9`, `rustls 0.23.43`, `time 0.3.54`, `chrono 0.4.45`. The npm lockfile's 183 packages are all registry-sourced with integrity hashes; `npm audit --audit-level=high` gates both `ci.yml:464` and `release.yml:98`.

**Release integrity.** `check-release-version.sh` enforces exact semver, refuses a moved or reused tag, requires an annotated tag resolving to the checked-out commit, cross-checks the root workspace / Tauri config / Tauri crate versions, requires pinned intra-workspace dependency versions, requires a dated CHANGELOG section, and requires the tag commit to equal current `origin/main`. The `publish` job sits behind a protected `release` environment and attests binaries, `SHA256SUMS`, and the SBOM. A separate `vars.KRANZ_PUBLIC_RELEASE_ENABLED` switch gates the whole thing.

**CLI file writes and temp handling.** `write_token_file` (`commands.rs:2665-2707`) is textbook: `create_new(true)` with `mode(0o600)` on Unix, a UUID-suffixed sibling temp in the destination directory, `set_permissions(0o600)` before the rename, atomic `rename` over the target so an existing inode or symlink is replaced rather than written through, and temp cleanup on every error path. `config_cmd.rs:350` and `init.rs:507-511` follow the same write-temp-then-rename discipline and preserve or set restrictive modes. Tests assert the 0600 mode, including the case where the destination pre-exists as 0644 (`commands.rs:3815-3868`). No `std::env::temp_dir()` use in production paths — the only hits are inside `#[cfg(test)]` blocks.

**Argument and process handling.** `open_browser` (`commands.rs:2858-2878`) spawns via argv with no shell; the Windows branch correctly passes an empty title argument to `start`. `create_pull_request` (`pr_handoff.rs:199-204`) passes title and body as argv with an explicit comment about never shell-interpolating. `which_binary` (`pr_handoff.rs:226-243`) walks `PATH` with `split_paths` rather than invoking a shell. `openspec.rs` slugs route through `Ticket::scaffold` validation and refuse to overwrite an existing ticket. Token comparison is constant-time via `subtle::ConstantTimeEq` (`server/src/lib.rs:918-922`), tokens are UUIDv4 (`server/src/lib.rs:53-55`), and the `?token=` query form is accepted only on gated *reads*, never on a mutation (`server/src/lib.rs:886-902`).

**Unbounded reads.** Agent message content is capped at `MESSAGE_CONTENT_MAX = 2000` before it reaches the event log (`runner.rs:56,154`), and rendered lines are capped at `LINE_MAX = 160` (`tail.rs:23`). The engine carries a dedicated `stream_bounds.rs`. I found no unbounded read of agent-controlled data on the CLI display path.

**Credential sweep of the tracked tree.** Detailed under L1 — no live credential, internal hostname, or personal path found; every hit was a synthetic fixture or a documented placeholder.
