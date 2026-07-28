---
title: Slack privileged actions fail closed without an explicit allowAllUsers (P2)
priority: 3
schedule: once
---

## Goal
An empty slack allowUsers list currently authorizes EVERYONE for
spend-adjacent actions (work, approve, merge). Require explicit
`allowAllUsers: true` acknowledgement for that posture, or fail closed for
privileged actions when the list is empty.

## Context
From the review (P2): slack/src/config.rs:84. Read-only/status actions may
stay open; spend-adjacent ones must not.
