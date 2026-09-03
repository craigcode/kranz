---
state: open
title: GitHub macOS runner image 20260831 refuses security create-keychain under a relocated HOME
priority: 2
schedule: once
---

## Goal

On 2026-09-03 the `rust-macos` CI job started failing every
`backend_cursor::tests::cursor_keychain_*` test with
`ensure_session_login_keychain` returning false. The failing jobs all ran on
runner image `macos-26-arm64` version `20260831.0337.3`; the same commit's
predecessor and a workflow-dispatched baseline on `main` passed on version
`20260728.0273.1` the same afternoon. The branch under test changed no
keychain code. The tests now probe `security create-keychain` in a throwaway
HOME and skip with the `keychain` capability marker when it fails, so the
regression is visible in the skip ledger rather than hidden as a red job.

## Acceptance hints

- Reproduce on the new image: what does `security create-keychain -p x
  <home>/Library/Keychains/login.keychain-db` return on
  `20260831.0337.3`, and does the cursor backend's seed still work for a real
  session there (the CLI dies with security exit 154 when no login keychain
  resolves through HOME).
- Either adapt `ensure_session_login_keychain` to the image's behaviour or
  document the image as unsupported for the cursor backend, then add
  `keychain` to `KRANZ_REQUIRED_CAPABILITIES` for the macOS job so the tests
  cannot silently skip again.
