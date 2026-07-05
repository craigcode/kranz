//! `kranz otel` sidecar — read-side event-to-span mapping.
//!
//! Everything here consumes `kranz_engine::events`/`kranz_engine::types`
//! read-side APIs only; no engine changes, no OTLP/network dependency in
//! this module. Transport (real OTLP export) is a later feature.

pub mod map;

pub use map::{span_id, trace_id, AttrValue, MissionSpan, SpanStatus};
