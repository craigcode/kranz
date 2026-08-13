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

By contributing, you agree that your contribution is licensed under the MIT
license in this repository.
