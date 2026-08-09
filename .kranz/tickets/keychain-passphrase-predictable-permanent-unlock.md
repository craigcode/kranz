---
title: Keychain seed: predictable session passphrase, argv-visible unlock, no auto-lock
priority: 1
schedule: once
---

## Goal
Harden the session keychain seed (acdc77b): the passphrase is kranz-scratch-{session_id} — predictable and visible in paths/logs — it is passed via security -p (argv-visible to any same-user process), and auto-lock is cleared, all to protect a store that is NOT empty forever (the motivating failure was Cursor writing credentials into the seeded keychain). Move to a random per-session secret stored 0600 under the scratch HOME, avoid argv-visible unlock where the security CLI allows, and keep a lock timeout. 14th-pass review finding.

## Context

Commits 71f7f60 + acdc77b seeded an empty login keychain into Cursor session
HOMEs to stop Cursor macOS sessions from writing credentials into the
operator's real login keychain. The v2 shape (session-derived passphrase,
argv-passed unlock, no auto-lock) solved the immediate failure but
predictably: anyone who can read mission paths/logs can reconstruct the
passphrase, `ps` can catch the unlock argv, and the keychain never re-locks —
while the whole point is that Cursor DOES write credentials into this store.

## Scoping answers

## Acceptance hints

- Passphrase is a random per-session secret, written 0600 under the session
  scratch HOME, never derived from the session id or other logged material.
- Unlock does not expose the secret in argv where the security CLI offers a
  stdin/file alternative; if argv is unavoidable on macOS, the plan must say
  so and justify the residual exposure.
- A lock timeout (or lock-on-session-end) replaces the cleared auto-lock.
- The original failure stays fixed: a Cursor session still reads/writes its
  own seeded keychain and never touches the operator's real one (regression
  test or a documented live-verification step).
- Anti-vacuity: test-name filters must match only the new tests.
