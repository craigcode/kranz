//! Transport adapter: turns neutral [`MissionSpan`] records into OTel
//! [`SpanData`] and exports them over OTLP HTTP/protobuf.
//!
//! This module is the only place in `kranz otel` that touches the
//! `opentelemetry*` crates — the mapping module ([`super::map`]) stays
//! dependency-free and unit-testable without a network.

use anyhow::Result;
use chrono::{DateTime, Utc};
use opentelemetry::trace::{SpanContext, SpanId, SpanKind, Status, TraceFlags, TraceId};
use opentelemetry::{InstrumentationScope, KeyValue, Value};
use opentelemetry_otlp::{Protocol, SpanExporter as OtlpSpanExporter, WithExportConfig};
use opentelemetry_sdk::trace::{SpanData, SpanEvents, SpanLinks};
use opentelemetry_sdk::Resource;

use super::map::{AttrValue, MissionSpan, SpanStatus};

/// Convert a neutral [`MissionSpan`] into an OTel [`SpanData`] record, ready
/// to hand to a [`opentelemetry_sdk::trace::SpanExporter`]. Pure and
/// network-free.
pub fn to_span_data(span: &MissionSpan) -> SpanData {
    let trace_id = TraceId::from_bytes(span.trace_id);
    let span_id = SpanId::from_bytes(span.span_id);
    let parent_span_id = span
        .parent_span_id
        .map(SpanId::from_bytes)
        .unwrap_or(SpanId::INVALID);

    let span_context = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::SAMPLED,
        false,
        Default::default(),
    );

    let attributes: Vec<KeyValue> = span
        .attributes
        .iter()
        .map(|(key, value)| {
            let value = match value {
                AttrValue::String(s) => Value::String(s.clone().into()),
                AttrValue::I64(i) => Value::I64(*i),
                AttrValue::F64(f) => Value::F64(*f),
            };
            KeyValue::new(key.clone(), value)
        })
        .collect();

    let status = match &span.status {
        SpanStatus::Unset => Status::Unset,
        SpanStatus::Ok => Status::Ok,
        SpanStatus::Error(description) => Status::error(description.clone()),
    };

    SpanData {
        span_context,
        parent_span_id,
        parent_span_is_remote: false,
        span_kind: SpanKind::Internal,
        name: span.name.clone().into(),
        start_time: to_system_time(span.start),
        end_time: to_system_time(span.end),
        attributes,
        dropped_attributes_count: 0,
        events: SpanEvents::default(),
        links: SpanLinks::default(),
        status,
        instrumentation_scope: InstrumentationScope::default(),
    }
}

fn to_system_time(ts: DateTime<Utc>) -> std::time::SystemTime {
    std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_nanos(ts.timestamp_nanos_opt().unwrap_or(0).max(0) as u64)
}

/// Build an OTLP HTTP/protobuf span exporter pointed at `endpoint` (e.g.
/// `http://localhost:4318/v1/traces` or a bare collector base URL).
pub fn build_exporter(endpoint: &str) -> Result<OtlpSpanExporter> {
    let exporter = OtlpSpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_protocol(Protocol::HttpBinary)
        .build()?;
    Ok(exporter)
}

/// Export `spans` as a single batch through `exporter`. Logs and returns on
/// export error — a sidecar tailing the mission log must never panic.
pub async fn export_spans(exporter: &OtlpSpanExporter, spans: Vec<MissionSpan>) {
    if spans.is_empty() {
        return;
    }

    let batch: Vec<SpanData> = spans.iter().map(to_span_data).collect();

    use opentelemetry_sdk::trace::SpanExporter;
    if let Err(err) = exporter.export(batch).await {
        tracing::warn!(error = %err, "kranz otel: failed to export spans, continuing");
    }
}

/// Build the OTel `Resource` describing this sidecar's service identity.
pub fn kranz_resource() -> Resource {
    Resource::builder().with_service_name("kranz").build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::otel::map::{span_id, trace_id};
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn sample_span() -> MissionSpan {
        MissionSpan {
            trace_id: trace_id("m-01"),
            span_id: span_id("m-01", 1),
            parent_span_id: Some(span_id("m-01", 0)),
            name: "worker run-1".to_string(),
            start: ts(0),
            end: ts(10),
            attributes: vec![
                (
                    "kranz.role".to_string(),
                    AttrValue::String("worker".to_string()),
                ),
                ("kranz.tokens.input".to_string(), AttrValue::I64(100)),
                ("kranz.cost.usd".to_string(), AttrValue::F64(1.25)),
            ],
            status: SpanStatus::Ok,
        }
    }

    #[test]
    fn to_span_data_round_trips_ids() {
        let span = sample_span();
        let data = to_span_data(&span);

        assert_eq!(data.span_context.trace_id().to_bytes(), span.trace_id);
        assert_eq!(data.span_context.span_id().to_bytes(), span.span_id);
        assert_eq!(data.parent_span_id.to_bytes(), span_id("m-01", 0));
    }

    #[test]
    fn to_span_data_uses_invalid_parent_when_none() {
        let mut span = sample_span();
        span.parent_span_id = None;
        let data = to_span_data(&span);
        assert_eq!(data.parent_span_id, SpanId::INVALID);
    }

    #[test]
    fn to_span_data_preserves_timestamps() {
        let span = sample_span();
        let data = to_span_data(&span);
        assert_eq!(data.start_time, to_system_time(span.start));
        assert_eq!(data.end_time, to_system_time(span.end));
    }

    #[test]
    fn to_span_data_maps_ok_status() {
        let span = sample_span();
        let data = to_span_data(&span);
        assert_eq!(data.status, Status::Ok);
    }

    #[test]
    fn to_span_data_maps_error_status_with_description() {
        let mut span = sample_span();
        span.status = SpanStatus::Error("boom".to_string());
        let data = to_span_data(&span);
        assert_eq!(data.status, Status::error("boom"));
    }

    #[test]
    fn to_span_data_preserves_attributes() {
        let span = sample_span();
        let data = to_span_data(&span);
        assert_eq!(data.attributes.len(), 3);
        assert!(data.attributes.iter().any(
            |kv| kv.key.as_str() == "kranz.role" && kv.value == Value::String("worker".into())
        ));
        assert!(data
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "kranz.tokens.input" && kv.value == Value::I64(100)));
        assert!(data
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "kranz.cost.usd" && kv.value == Value::F64(1.25)));
    }
}
