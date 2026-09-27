//! Bounded, read-only response observation. Forward the original HTTP frames,
//! errors and trailers; diagnostics must never buffer or rewrite inference.
use crate::{model::ChannelConfig, source_driver::SourceDriverId};
use bytes::Bytes;
use http_body::Body;
use reqwest::{Response, ResponseBuilderExt};
use serde_json::{Map, Value};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

const MAX_DIAGNOSTIC_BYTES: usize = 256 * 1024;
const FIELD: &str = "upstream_diagnostics";

#[derive(Debug, Clone, Default)]
pub(crate) struct ResponseDiagnostics(Arc<Mutex<Map<String, Value>>>);

impl ResponseDiagnostics {
    pub(crate) fn attach(&self, payload: &mut Map<String, Value>) {
        let Ok(snapshot) = self.0.lock() else {
            return;
        };
        if snapshot.is_empty() {
            return;
        }
        // Keep provider details out of the control fields used by the existing
        // retry/billing classification. Only the unified logger consumes this.
        payload.insert(FIELD.into(), Value::Object(snapshot.clone()));
    }

    fn record(&self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        if !text.contains("\"openrouter_metadata\"") && !text.contains("\"error\"") {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return;
        };
        let metadata = value
            .get("openrouter_metadata")
            .or_else(|| value.pointer("/response/openrouter_metadata"))
            .and_then(Value::as_object);
        let error_pair = crate::upstream_failure::error_object(&value);
        let error = error_pair.map(|(_, error)| error);
        if metadata.is_none() && error.is_none() {
            return;
        }
        let Ok(mut snapshot) = self.0.lock() else {
            return;
        };
        if let Some(metadata) = metadata {
            for (wire, field) in [
                ("requested", "requested_model"),
                ("strategy", "routing_strategy"),
                ("region", "region"),
            ] {
                copy_identifier(metadata.get(wire), &mut snapshot, field);
            }
            if let Some(attempt) = metadata
                .get("attempt")
                .and_then(Value::as_u64)
                .filter(|v| *v <= 10_000)
            {
                snapshot.insert("attempt".into(), attempt.into());
            }
            if let Some(byok) = metadata.get("is_byok").and_then(Value::as_bool) {
                snapshot.insert("is_byok".into(), byok.into());
            }
            let endpoints = metadata.get("endpoints").and_then(Value::as_object);
            if let Some(total) = endpoints
                .and_then(|e| e.get("total"))
                .and_then(Value::as_u64)
            {
                snapshot.insert("endpoint_count".into(), total.into());
            }
            let selected = endpoints
                .and_then(|e| e.get("available"))
                .and_then(Value::as_array)
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|entry| entry.get("selected") == Some(&Value::Bool(true)))
                });
            if let Some(selected) = selected {
                copy_identifier(selected.get("provider"), &mut snapshot, "provider");
                copy_identifier(selected.get("model"), &mut snapshot, "served_model");
            }
            // Never retain summaries, pipeline/guardrail data, prompts or raw
            // provider errors. Only bounded structured attempt identifiers.
            if let Some(attempts) = metadata.get("attempts").and_then(Value::as_array) {
                let attempts = attempts
                    .iter()
                    .take(8)
                    .map(|attempt| {
                        let mut out = Map::new();
                        for name in ["provider", "model"] {
                            copy_identifier(attempt.get(name), &mut out, name);
                        }
                        if let Some(status) = attempt
                            .get("status")
                            .and_then(Value::as_u64)
                            .filter(|s| (100..=599).contains(s))
                        {
                            out.insert("status".into(), status.into());
                        }
                        Value::Object(out)
                    })
                    .collect();
                snapshot.insert("attempts".into(), Value::Array(attempts));
            }
        }
        copy_identifier(
            value.get("id").or_else(|| value.pointer("/response/id")),
            &mut snapshot,
            "provider_request_id",
        );
        copy_identifier(value.get("provider"), &mut snapshot, "provider");
        if let Some(error) = error {
            copy_identifier(
                error
                    .pointer("/metadata/error_type")
                    .or_else(|| error.get("error_type"))
                    .or_else(|| error.get("type")),
                &mut snapshot,
                "error_type",
            );
            if let Some((envelope, _)) = error_pair {
                let canonical = super::canonical_error_type(envelope, error);
                if !canonical.is_empty() {
                    snapshot.insert("error_type".into(), canonical.into());
                }
            }
            copy_identifier(
                error
                    .pointer("/metadata/provider_code")
                    .or_else(|| error.get("code")),
                &mut snapshot,
                "error_code",
            );
            copy_identifier(
                error.pointer("/metadata/provider_name"),
                &mut snapshot,
                "provider",
            );
            if let Some(code) = error
                .get("code")
                .and_then(Value::as_u64)
                .filter(|s| (100..=599).contains(s))
            {
                snapshot.insert("error_status".into(), code.into());
            }
        }
    }
}

fn copy_identifier(value: Option<&Value>, into: &mut Map<String, Value>, key: &str) {
    if let Some(value) = value.and_then(Value::as_str).filter(|s| {
        !s.is_empty()
            && s.len() <= 192
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:/ []".contains(&b))
    }) {
        into.insert(key.into(), value.into());
    }
}

pub(crate) fn response_diagnostics(response: &Response) -> Option<ResponseDiagnostics> {
    response.extensions().get::<ResponseDiagnostics>().cloned()
}

pub(crate) fn observe_response(channel: &ChannelConfig, response: Response) -> Response {
    if channel.source_driver() != SourceDriverId::Openrouter {
        return response;
    }
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    let is_json = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("json"));
    // Do not inspect image/audio/video bytes or untyped opaque downloads.
    if !is_sse && !is_json {
        return response;
    }
    let report = ResponseDiagnostics::default();
    if let Ok(mut snapshot) = report.0.lock() {
        snapshot.insert("http_status".into(), response.status().as_u16().into());
        if let Some(id) = response
            .headers()
            .get("x-generation-id")
            .and_then(|v| v.to_str().ok())
        {
            copy_identifier(
                Some(&Value::String(id.into())),
                &mut snapshot,
                "provider_request_id",
            );
        }
    }
    let mut builder = http::Response::builder().url(response.url().clone());
    let original: http::Response<reqwest::Body> = response.into();
    let (mut parts, inner) = original.into_parts();
    if let Some(extensions) = builder.extensions_mut() {
        parts.extensions.extend(std::mem::take(extensions));
    }
    parts.extensions.insert(report.clone());
    let body = ObservedBody {
        inner: Box::pin(inner),
        report,
        channel: channel.id.clone(),
        is_sse,
        buffer: Vec::new(),
        line: Vec::new(),
        line_has_content: false,
        overflow: false,
        finished: false,
    };
    http::Response::from_parts(parts, reqwest::Body::wrap(body)).into()
}

struct ObservedBody {
    inner: Pin<Box<reqwest::Body>>,
    report: ResponseDiagnostics,
    channel: String,
    is_sse: bool,
    buffer: Vec<u8>,
    line: Vec<u8>,
    line_has_content: bool,
    overflow: bool,
    finished: bool,
}

impl ObservedBody {
    fn push(&mut self, bytes: &[u8]) {
        if !self.is_sse {
            if self.buffer.len().saturating_add(bytes.len()) <= MAX_DIAGNOSTIC_BYTES
                && !self.overflow
            {
                self.buffer.extend_from_slice(bytes);
            } else {
                self.buffer.clear();
                self.overflow = true;
            }
            return;
        }
        for segment in bytes.split_inclusive(|b| *b == b'\n') {
            self.line_has_content |= segment.iter().any(|b| *b != b'\n' && *b != b'\r');
            if self
                .line
                .len()
                .saturating_add(self.buffer.len())
                .saturating_add(segment.len())
                <= MAX_DIAGNOSTIC_BYTES
                && !self.overflow
            {
                self.line.extend_from_slice(segment);
            } else {
                self.line.clear();
                self.buffer.clear();
                self.overflow = true;
            }
            if segment.last() != Some(&b'\n') {
                continue;
            }
            let line = self.line.strip_suffix(b"\n").unwrap_or(&self.line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() && !self.overflow {
                self.report.record(&self.buffer);
                self.buffer.clear();
            } else if let Some(data) = line.strip_prefix(b"data:") {
                if !self.buffer.is_empty() {
                    self.buffer.push(b'\n');
                }
                self.buffer
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
            }
            // A blank physical line safely resumes observation after oversized
            // content; the oversized event is still forwarded without changes.
            if self.overflow && !self.line_has_content {
                self.overflow = false;
            }
            self.line_has_content = false;
            self.line.clear();
        }
    }

    fn finish(&mut self, complete: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.overflow {
            if self.is_sse {
                // Observe a terminal data line even if the upstream omitted its
                // final newline. Wire bytes and downstream SSE behavior stay intact.
                if let Some(data) = self.line.strip_prefix(b"data:") {
                    if !self.buffer.is_empty() {
                        self.buffer.push(b'\n');
                    }
                    self.buffer
                        .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                }
            }
            self.report.record(&self.buffer);
        }
        if let Ok(mut snapshot) = self.report.0.lock() {
            snapshot.insert("body_complete".into(), complete.into());
            let failed = snapshot.contains_key("error_type")
                || snapshot.contains_key("error_status")
                || snapshot
                    .get("http_status")
                    .and_then(Value::as_u64)
                    .is_some_and(|s| s >= 400)
                || !complete;
            if failed {
                log::warn!(
                    "[const-api][openrouter] channel_id={} diagnostics={}",
                    self.channel,
                    Value::Object(snapshot.clone())
                );
            } else {
                log::debug!(
                    "[const-api][openrouter] channel_id={} diagnostics={}",
                    self.channel,
                    Value::Object(snapshot.clone())
                );
            }
        }
    }
}

impl Body for ObservedBody {
    type Data = Bytes;
    type Error = <reqwest::Body as Body>::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let result = this.inner.as_mut().poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.push(data);
                }
            }
            Poll::Ready(Some(Err(_))) => this.finish(false),
            Poll::Ready(None) => this.finish(true),
            _ => {}
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for ObservedBody {
    fn drop(&mut self) {
        self.finish(self.inner.is_end_stream());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel(driver: SourceDriverId) -> ChannelConfig {
        let mut channel =
            crate::channel_from_supplier("router-test".into(), &crate::default_supplier_config());
        channel.set_source_driver(driver);
        channel
    }

    fn response(content_type: &str, bytes: &[u8], chunk_size: usize, status: u16) -> Response {
        let chunks = bytes
            .chunks(chunk_size)
            .map(Bytes::copy_from_slice)
            .map(Ok::<_, std::io::Error>)
            .collect::<Vec<_>>();
        http::Response::builder()
            .url(reqwest::Url::parse("https://openrouter.ai/api/v1/responses").unwrap())
            .status(status)
            .header("content-type", content_type)
            .header("x-generation-id", "gen-header")
            .body(reqwest::Body::wrap_stream(futures_util::stream::iter(
                chunks,
            )))
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn fragmented_sse_diagnostics_preserve_every_byte_and_exclude_content() {
        let metadata = json!({"id":"gen-end", "openrouter_metadata":{
            "requested":"vendor/model:free","strategy":"direct","region":"iad","attempt":2,
            "summary":"SECRET","pipeline":[{"raw":"SECRET"}],
            "endpoints":{"total":3,"available":[{"provider":"Provider A","model":"vendor/model:free","selected":true}]},
            "attempts":[{"provider":"Provider B","status":429,"message":"SECRET"}]
        }});
        let sse = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"你好 SECRET\"}}}}]}}\r\n\r\ndata: {metadata}\r\n\r\ndata: [DONE]\r\n\r\n"
        );
        for size in [1, 2, 7, 31, 1024] {
            let response = observe_response(
                &channel(SourceDriverId::Openrouter),
                response("text/event-stream", sse.as_bytes(), size, 200),
            );
            assert_eq!(
                response.url().as_str(),
                "https://openrouter.ai/api/v1/responses"
            );
            assert_eq!(response.headers()["x-generation-id"], "gen-header");
            let diagnostics = response_diagnostics(&response).unwrap();
            assert_eq!(response.bytes().await.unwrap().as_ref(), sse.as_bytes());
            let mut payload = Map::new();
            diagnostics.attach(&mut payload);
            assert_eq!(payload[FIELD]["attempt"], 2);
            assert_eq!(payload[FIELD]["provider"], "Provider A");
            assert_eq!(payload[FIELD]["body_complete"], true);
            assert_eq!(payload[FIELD]["provider_request_id"], "gen-end");
            assert!(!Value::Object(payload).to_string().contains("SECRET"));
        }
    }

    #[tokio::test]
    async fn errors_and_json_success_are_observed_without_changing_http_semantics() {
        for status in [200, 400, 402, 403, 429, 503] {
            let body = br#"{"error":{"code":429,"message":"SECRET","metadata":{"error_type":"rate_limit_exceeded","provider_code":"overloaded","raw":"SECRET"}}}"#;
            let response = observe_response(
                &channel(SourceDriverId::Openrouter),
                response("application/json", body, 3, status),
            );
            let report = response_diagnostics(&response).unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(response.bytes().await.unwrap().as_ref(), body);
            let mut payload = Map::new();
            report.attach(&mut payload);
            assert_eq!(payload[FIELD]["error_type"], "rate_limit_exceeded");
            assert_eq!(payload[FIELD]["error_code"], "overloaded");
            assert!(!payload.contains_key("error_code") && !payload.contains_key("error_type"));
            assert!(!Value::Object(payload).to_string().contains("SECRET"));
        }
    }

    #[tokio::test]
    async fn nested_responses_errors_use_the_same_optional_wire_diagnostic_envelope() {
        let raw = br#"{"type":"response.failed","response":{"id":"gen-nested","error":{"type":"server_error","code":"overloaded","message":"SECRET"}}}"#;
        let response = observe_response(
            &channel(SourceDriverId::Openrouter),
            response("application/json", raw, 3, 200),
        );
        let report = response_diagnostics(&response).unwrap();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.unwrap();
        let mut payload =
            crate::surface_wire::http_response_payload(200, &headers, &bytes).unwrap();
        report.attach(&mut payload);
        let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(wire.body.decode().unwrap(), raw);
        assert_eq!(wire.status, Some(200));
        assert_eq!(payload[FIELD]["provider_request_id"], "gen-nested");
        assert_eq!(payload[FIELD]["error_type"], "server_error");
        assert_eq!(payload[FIELD]["error_code"], "overloaded");
        assert!(!payload.contains_key("error_code"));

        let report = ResponseDiagnostics::default();
        report.record(br#"{"id":"call-not-a-generation","content":{"error":"model-generated text is not a gateway failure"}}"#);
        let mut payload = Map::new();
        report.attach(&mut payload);
        assert!(payload.is_empty());
    }

    #[tokio::test]
    async fn oversized_events_do_not_break_passthrough_or_hide_later_metadata() {
        let sse = format!(
            "data: {{\"text\":\"{}\"}}\n\ndata: {{\"openrouter_metadata\":{{\"attempt\":3}}}}\n\n",
            "x".repeat(MAX_DIAGNOSTIC_BYTES + 1)
        );
        let response = observe_response(
            &channel(SourceDriverId::Openrouter),
            response("text/event-stream", sse.as_bytes(), 101, 200),
        );
        let report = response_diagnostics(&response).unwrap();
        assert_eq!(response.bytes().await.unwrap().as_ref(), sse.as_bytes());
        let mut payload = Map::new();
        report.attach(&mut payload);
        assert_eq!(payload[FIELD]["attempt"], 3);
    }

    #[tokio::test]
    async fn unrelated_drivers_and_binary_responses_are_not_observed() {
        for driver in crate::source_driver::ALL_SOURCE_DRIVER_IDS {
            for content_type in ["application/json", "audio/mpeg"] {
                let response = observe_response(
                    &channel(driver),
                    response(content_type, b"unchanged", 2, 200),
                );
                assert_eq!(
                    response_diagnostics(&response).is_some(),
                    driver == SourceDriverId::Openrouter && content_type == "application/json"
                );
                assert_eq!(response.bytes().await.unwrap().as_ref(), b"unchanged");
            }
        }
    }

    #[tokio::test]
    async fn multiline_and_unterminated_events_are_observed_without_rewriting_retry_classification()
    {
        let sse = b"event: message_stop\r\ndata: {\"openrouter_metadata\":\r\ndata: {\"attempt\":2},\"error\":{\"metadata\":{\"error_type\":\"upstream_error\",\"provider_code\":\"rate_limit\"}}}";
        let response = observe_response(
            &channel(SourceDriverId::Openrouter),
            response("text/event-stream", sse, 1, 200),
        );
        let report = response_diagnostics(&response).unwrap();
        assert_eq!(response.bytes().await.unwrap().as_ref(), sse);
        let mut payload = json!({"error_type":"existing_type","error_code":"existing_code","safe_to_retry_same_channel":false}).as_object().unwrap().clone();
        report.attach(&mut payload);
        assert_eq!(payload[FIELD]["attempt"], 2);
        assert_eq!(payload[FIELD]["error_type"], "upstream_error");
        assert_eq!(payload["error_type"], "existing_type");
        assert_eq!(payload["error_code"], "existing_code");
        assert_eq!(payload["safe_to_retry_same_channel"], false);
    }

    #[tokio::test]
    async fn trailers_transport_errors_and_cancellation_are_preserved() {
        use http_body_util::BodyExt;
        let mut trailers = reqwest::header::HeaderMap::new();
        trailers.insert("fixture-trailer", "unchanged".parse().unwrap());
        let frames = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(http_body::Frame::data(Bytes::from_static(b"{}"))),
            Ok(http_body::Frame::trailers(trailers.clone())),
        ]);
        let inner: Response = http::Response::builder()
            .header("content-type", "application/json")
            .body(reqwest::Body::wrap(http_body_util::StreamBody::new(frames)))
            .unwrap()
            .into();
        let response = observe_response(&channel(SourceDriverId::Openrouter), inner);
        let report = response_diagnostics(&response).unwrap();
        let wire: http::Response<reqwest::Body> = response.into();
        let collected = wire.into_body().collect().await.unwrap();
        assert_eq!(collected.trailers(), Some(&trailers));
        assert_eq!(collected.to_bytes(), b"{}".as_slice());
        let mut payload = Map::new();
        report.attach(&mut payload);
        assert_eq!(payload[FIELD]["body_complete"], true);

        let chunks = futures_util::stream::iter(vec![
            Ok(Bytes::from_static(b"data: {}\n\n")),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "fixture reset",
            )),
        ]);
        let inner: Response = http::Response::builder()
            .header("content-type", "text/event-stream")
            .body(reqwest::Body::wrap_stream(chunks))
            .unwrap()
            .into();
        let mut response = observe_response(&channel(SourceDriverId::Openrouter), inner);
        let report = response_diagnostics(&response).unwrap();
        assert_eq!(
            response.chunk().await.unwrap().unwrap(),
            b"data: {}\n\n".as_slice()
        );
        assert!(response.chunk().await.is_err());
        let mut payload = Map::new();
        report.attach(&mut payload);
        assert_eq!(payload[FIELD]["body_complete"], false);

        let inner: Response = http::Response::builder()
            .header("content-type", "text/event-stream")
            .body(reqwest::Body::wrap_stream(futures_util::stream::pending::<
                Result<Bytes, std::io::Error>,
            >()))
            .unwrap()
            .into();
        let response = observe_response(&channel(SourceDriverId::Openrouter), inner);
        let report = response_diagnostics(&response).unwrap();
        drop(response);
        let mut payload = Map::new();
        report.attach(&mut payload);
        assert_eq!(payload[FIELD]["body_complete"], false);
    }
}
