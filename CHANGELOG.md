# Changelog

Notable user-visible changes are documented here. This project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

- Added an experimental Even Realities G2 thin client (`apps/even-g2`) that
  renders mission status from the existing REST API and lets an operator
  approve or deny a pending grant or pick a structured question's answer,
  each behind a review screen and a separate confirmation tap. Development
  sideload only; the physical-device receipt is tracked in the
  `even-realities-g2-demo` ticket.

## 0.2.0 - 2026-08-22

- Prepared the repository's security, contribution, release, and supply-chain
  controls for public distribution.
- Expanded Kranz from its original three CLI integrations to additional
  worker and validation backends, including Kimi, Cursor, ACP, and local
  OpenAI-compatible inference.
- Hardened validator isolation, gate-command sandboxing, credential handling,
  mission-path filesystem operations, authorization, and audit evidence.
- Added first-class macOS, Linux, Docker, and Windows AppContainer containment
  evidence, including fail-closed validation and gate execution.
- Added multi-repository hosting, dashboard routing, Slack operation, knowledge
  refresh, release evidence, and clean public-distribution automation.

## 0.1.0 - 2026-07-04

Private preview. The historical binaries predate substantial security and
correctness hardening and are not a supported public distribution.
