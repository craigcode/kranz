//! `kranz otel` sidecar — read-side event-to-span mapping.
//!
//! Everything here consumes `kranz_engine::events`/`kranz_engine::types`
//! read-side APIs only; no engine changes, no OTLP/network dependency in
//! this module. Transport (real OTLP export) is a later feature.

pub mod emit;
pub mod map;
pub mod run;

pub use emit::{build_exporter, export_spans, kranz_resource, to_span_data};
pub use map::{map_mission, span_id, trace_id, AttrValue, MissionSpan, SpanStatus};
pub use run::run_otel;
