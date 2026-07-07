---
title: kranz otel exports spans with service.name=unknown_service
priority: 3
schedule: once
---

## Goal
The otel sidecar calls exporter.export(batch) directly and never applies a resource (crates/cli/src/otel/emit.rs:99); kranz_resource() (sets service.name=kranz) is defined and re-exported but never called. Spans export but are mis-attributed at the collector. Fix: attach kranz_resource() to the exporter, or export through a TracerProvider configured with it.

## Context
Found by a security code review 2026-07-07 (HEAD 0740072). Cosmetic/attribution.

## Acceptance hints
- Exported spans carry service.name=kranz. Verify via the emit path/test.
