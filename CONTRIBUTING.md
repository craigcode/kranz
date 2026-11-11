# Contributing to Kranz

Thank you for helping improve Kranz. The project accepts focused bug fixes,
documentation improvements, tests, and changes that strengthen its governance
and evidence layer.

## Before opening a change

- Search existing issues and tickets for overlapping work.
- Discuss large features before implementation. Kranz intentionally does not
  grow new agent-writing, prompt-routing, or context-management primitives.
- Report security problems through the private process in `SECURITY.md`.
- Keep commits focused and preserve persisted event/config compatibility.

## Development setup

Install Rust 1.88 or newer, Git, Node.js 22 for dashboard work, and the system
dependencies required by Tauri when changing the desktop shell.

Run the complete Rust gate before submitting:

```sh
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

When `apps/dashboard` changes, also run:

```sh
cd apps/dashboard
npm ci
npm audit --audit-level=high
npx tsc -b
npm run test
npm run build
npm run sync-embedded
npm run check-embedded
npm run lint
```

Run `cargo deny check` and the repository's secret/domain scans when changing
dependencies, release automation, authentication, sandboxing, or fixtures.

## Pull requests

Explain the user-visible outcome, security implications, validation performed,
and any compatibility or rollout concern. Add regression tests for behavior
changes. Do not commit credentials, raw agent transcripts, operator home paths,
or captured workstation/plugin inventories.

## Licensing of contributions

Kranz is licensed under the Apache License, Version 2.0 (see `LICENSE`).
By submitting a contribution you agree to the following terms.

- **Inbound license.** Your contribution is licensed to the project and its
  users under the Apache License 2.0, including its patent grant, as set out
  in section 5 of that license.
- **Sign-off.** Every commit you submit carries a `Signed-off-by:` line with
  your real name and email (`git commit -s`). The line certifies the
  [Developer Certificate of Origin 1.1](https://developercertificate.org/):
  you wrote the change, or otherwise have the right to submit it under the
  project license. Pull requests with unsigned commits are not merged.
- **Relicensing.** You keep the copyright in your contribution. You also
  grant Craig Martin, the project's copyright holder, and any successors and
  assigns, a perpetual, worldwide, non-exclusive, royalty-free, irrevocable
  license to use, modify, sublicense and distribute your contribution as part
  of Kranz under other license terms, including terms that are not open
  source. Releases already published under Apache 2.0 remain available under
  it.
- **Employer rights.** If your employer has rights in work you create, confirm
  you have permission to contribute before opening a pull request.
