# Handoff → Claude on macOS

Scratch note for picking this up on a Mac. **Delete before merging** — it is not
part of the repo's permanent docs.

Written from a Windows 11 ARM64 box that had never built this project. Most of
what follows was found by building it there for the first time.

---

## The one job for the Mac

**PR #22 (`feat/capability-gates-fail-loudly`) is red on `rust-macos`.** The
`Provision a container runtime (Colima)` step fails. Everything else on that PR
is green or was green before the last push.

The Mac matters because that failure cannot be diagnosed from Windows, and
because the macOS container path has *never actually run* — on CI or anywhere.

### What the CI log actually says

Do not trust my earlier guess. I hypothesised "Apple Silicon runners lack nested
virtualization" and **the log disproved it** — VZ starts fine:

```
[hostagent] Starting VZ (hint: ... see /Users/runner/.colima/_lima/colima/serial*.log)
[hostagent] Mounting disk `colima` on `/mnt/lima-colima`
[hostagent] Converting `.../_disks/colima/datadisk` (raw) to a raw disk `.../datadisk`
level=fatal msg="exiting, status={Running:false Degraded:false Exiting:true Errors:[] ...}"
##[error]Process completed with exit code 1.
```

So: `brew install colima docker` succeeded, the aarch64 image downloaded, the
disk resized 3.5→20GiB, the hostagent socket appeared, VZ launched — then Lima
exited during disk mount with an **empty `Errors:[]`**. The real detail is in
`~/.colima/_lima/colima/ha.stderr.log`, which CI never captures.

Fetch the full log (note `--allow-escape-sequences`, without it gh writes 0 bytes,
and `gh run view --log` refuses while any job in the run is still going):

```bash
gh api repos/craigcode/kranz/actions/jobs/97586122026/logs --allow-escape-sequences > colima.log
```

### First thing to try on the Mac

```bash
brew install colima docker && colima start && docker version
```

- **Works locally** → runner-specific. Try `colima start --vm-type qemu` (slower,
  sidesteps VZ) or pin the job to `macos-13` (Intel).
- **Fails locally too** → read `~/.colima/_lima/colima/ha.stderr.log`. That is the
  error CI is hiding.

### Then the actual prize

```bash
KRANZ_REQUIRED_CAPABILITIES=git,sandbox-exec,container cargo test --workspace
```

First time the macOS container path executes. **Expect it to surface real bugs.**
The equivalent Windows change did, immediately, and both were genuine (a verbatim
`\\?\` path breaking docker's colon-delimited `-v` parser, and a daemon that could
not pull Linux images).

### The legitimate escape hatch

`sandbox_container.rs` claims "Host support is deliberately macOS/Linux only". If
hosted macOS runners genuinely cannot run Linux containers, **narrowing that claim
to Linux is the correct fix, not a retreat** — an honest support matrix beats an
unevidenced one. The ticket
`.kranz/tickets/macos-container-path-unexercised.md` explicitly allows it.

---

## Repo state

- `main` = `43a8ee7`. Release candidate `d9b1c19` merged and audited.
- **M7 is closed on evidence** — both operator receipts captured on real Windows 11
  hardware, recorded verbatim in
  `.kranz/tickets/m7-windows-containment-parity.md`.
- v0.2.0 release plan: **9 of 18 items done**; every remaining one is owner-gated
  publication process, not engineering.
- Open PRs: **#22** (this work), plus pre-existing #19 and four dependabot PRs
  that predate this session.

### Do not re-litigate

- `git push` to `main` is **rejected by ruleset** — PRs are mandatory, 11 checks
  required. I tried; it refused. That is correct behaviour.
- Windows suite is **2424 passed / 0 failed** from `cmd.exe` with no POSIX
  coreutils. Do not "fix" Windows without reproducing first.
- The ARM64 release leg is **proven** (run 32764000460): PE machine word `0xAA64`,
  build 10m17s, binary runs. The `choco install llvm` fallback inside it is
  **unproven** — the runner already ships clang, so that branch never executed.

---

## Windows-specific traps (context, not tasks)

Recorded because they are non-obvious and cost real time:

1. **`=ExitCode`** — cmd.exe sets this pseudo-variable; the AppContainer env-block
   validator rejected any key containing `=`. Every enforced Windows session
   launched from a cmd prompt failed closed. CI never saw it: GitHub `run:` steps
   default to **pwsh**.
2. **`C:\Users` traversal** — the DACL lease walked into SYSTEM-owned `C:\Users`,
   and Node's `realpathSync` must `lstat` it. Bypass-traverse lets you pass
   *through* a directory, not stat it — I got that wrong first and the `EPERM`
   corrected me. Host prep now grants the derived profile parent.
3. **Vacuous skips** — a runtime-gated test that returns early prints `ok`. This is
   what PR #22 exists to fix. Three instances found in one session.
4. **`knowledge-refresh` under-reports locally** — `path_changed_since` reads
   `git log`, so **uncommitted edits are invisible**. Always re-run it *after*
   committing. This caught me twice.
5. **Frontmatter dates are UTC.** `--since=<date>T23:59:59Z`. A commit authored
   20:25 PDT is `2026-08-24T03:25Z`, so the local date reads as drifted.

---

## If you only do one thing

Run `colima start` on the Mac and read `ha.stderr.log`. Everything else about
PR #22 is verified; that single unknown is the whole blocker.
