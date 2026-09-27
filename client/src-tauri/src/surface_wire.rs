use crate::surface::{ApiOperation, ApiSurface};
use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

const WIRE_VERSION: u8 = 2;
pub(crate) const COUNTS_CONCURRENCY_FIELD: &str = "const_counts_concurrency";
pub(crate) const STARTS_LOGICAL_REQUEST_FIELD: &str = "const_starts_logical_request";
pub(crate) const LOGICAL_REQUEST_STARTED_FIELD: &str = "const_logical_request_started";
pub(crate) const LEGACY_COUNTS_RPM_FIELD: &str = "const_counts_rpm";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WireEncoding {
    Utf8,
    Base64,
}

impl Default for WireEncoding {
    fn default() -> Self {
        Self::Utf8
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WireBody {
    #[serde(default)]
    pub(crate) encoding: WireEncoding,
    #[serde(default)]
    pub(crate) data: String,
}

impl WireBody {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(text) => Self {
                encoding: WireEncoding::Utf8,
                data: text.to_string(),
            },
            Err(_) => Self {
                encoding: WireEncoding::Base64,
                data: BASE64.encode(bytes),
            },
        }
    }

    pub(crate) fn decode(&self) -> Result<Vec<u8>> {
        match self.encoding {
            WireEncoding::Utf8 => Ok(self.data.as_bytes().to_vec()),
            WireEncoding::Base64 => BASE64.decode(&self.data).map_err(Into::into),
        }
    }

    pub(crate) fn utf8(&self) -> Result<&str> {
        match self.encoding {
            WireEncoding::Utf8 => Ok(&self.data),
            WireEncoding::Base64 => Err(anyhow!("surface wire body is not UTF-8")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WireHeader {
    pub(crate) name: String,
    pub(crate) value: WireBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SurfaceEnvelope {
    pub(crate) version: u8,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "wire_surface"
    )]
    pub(crate) surface: Option<ApiSurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) operation: Option<ApiOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) protocol: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) method: String,
    #[serde(
        default,
        rename = "escaped_relative_path",
        skip_serializing_if = "String::is_empty"
    )]
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) raw_query: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) has_query: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) headers: Vec<WireHeader>,
    #[serde(default)]
    pub(crate) body: WireBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_upstream_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_target_protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cache_identity: Option<String>,
}

mod wire_surface {
    use super::ApiSurface;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S>(value: &Option<ApiSurface>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(surface) => serializer.serialize_some(surface.as_str()),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Option<ApiSurface>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        value
            .map(|surface| match surface.as_str() {
                "openai" | "open_ai" => Ok(ApiSurface::OpenAi),
                "anthropic" => Ok(ApiSurface::Anthropic),
                "gemini" => Ok(ApiSurface::Gemini),
                _ => Err(serde::de::Error::unknown_variant(
                    &surface,
                    &["openai", "anthropic", "gemini"],
                )),
            })
            .transpose()
    }
}

impl Default for SurfaceEnvelope {
    fn default() -> Self {
        Self {
            version: WIRE_VERSION,
            surface: None,
            operation: None,
            protocol: None,
            method: String::new(),
            path: String::new(),
            raw_query: String::new(),
            has_query: false,
            headers: Vec::new(),
            body: WireBody::default(),
            status: None,
            stream: None,
            requested_model: None,
            selected_upstream_model: None,
            selected_target_protocol: None,
            cache_identity: None,
        }
    }
}

impl SurfaceEnvelope {
    pub(crate) fn from_payload(
        payload: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self> {
        let wire = payload
            .get("wire")
            .ok_or_else(|| anyhow!("surface wire payload is missing v2 envelope"))?;
        let envelope: Self = serde_json::from_value(wire.clone())?;
        if envelope.version != WIRE_VERSION {
            return Err(anyhow!(
                "unsupported surface wire version {}",
                envelope.version
            ));
        }
        Ok(envelope)
    }

    pub(crate) fn header_map(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        for header in &self.headers {
            let name = HeaderName::from_bytes(header.name.as_bytes())?;
            let value = HeaderValue::from_bytes(&header.value.decode()?)?;
            headers.append(name, value);
        }
        Ok(headers)
    }

    pub(crate) fn header_utf8(&self, name: &str) -> Result<Option<&str>> {
        for header in &self.headers {
            if header.name.eq_ignore_ascii_case(name) {
                return header.value.utf8().map(Some);
            }
        }
        Ok(None)
    }

    pub(crate) fn canonical_path(&self) -> String {
        match self.surface {
            Some(ApiSurface::Anthropic) => format!("/anthropic{}", self.path),
            Some(ApiSurface::Gemini) => format!("/gemini{}", self.path),
            _ => self.path.clone(),
        }
    }
}

pub(crate) fn header_pairs(headers: &HeaderMap) -> Vec<WireHeader> {
    let mut out = Vec::new();
    for name in headers.keys() {
        for value in headers.get_all(name) {
            out.push(WireHeader {
                name: name.as_str().to_string(),
                value: WireBody::from_bytes(value.as_bytes()),
            });
        }
    }
    out
}

pub(crate) fn wire_payload(
    envelope: SurfaceEnvelope,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut payload = serde_json::Map::new();
    payload.insert("wire".to_string(), serde_json::to_value(envelope)?);
    Ok(payload)
}

pub(crate) fn merge_raw_query(target: &str, raw_query: &str, has_query: bool) -> String {
    if !has_query && raw_query.is_empty() {
        return target.to_string();
    }

    let (base, fragment) = target
        .split_once('#')
        .map_or((target, None), |(base, fragment)| (base, Some(fragment)));
    let mut merged = base.to_string();
    match (base.contains('?'), raw_query.is_empty()) {
        (false, _) => {
            merged.push('?');
            merged.push_str(raw_query);
        }
        (true, false) => {
            if !base.ends_with('?') && !base.ends_with('&') {
                merged.push('&');
            }
            merged.push_str(raw_query);
        }
        (true, true) => {}
    }
    if let Some(fragment) = fragment {
        merged.push('#');
        merged.push_str(fragment);
    }
    merged
}

pub(crate) fn http_response_payload(
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<serde_json::Map<String, serde_json::Value>> {
    wire_payload(SurfaceEnvelope {
        status: Some(status),
        headers: header_pairs(headers),
        body: WireBody::from_bytes(body),
        ..SurfaceEnvelope::default()
    })
}

pub(crate) fn stream_start_payload(
    status: u16,
    headers: &HeaderMap,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    wire_payload(SurfaceEnvelope {
        status: Some(status),
        headers: header_pairs(headers),
        stream: Some(true),
        ..SurfaceEnvelope::default()
    })
}

pub(crate) fn stream_chunk_payload(
    body: &[u8],
) -> Result<serde_json::Map<String, serde_json::Value>> {
    wire_payload(SurfaceEnvelope {
        body: WireBody::from_bytes(body),
        stream: Some(true),
        ..SurfaceEnvelope::default()
    })
}

const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn wire_bytes_round_trip_binary_data() {
        let raw = [0, 159, 146, 150, 255];
        let encoded = WireBody::from_bytes(&raw);
        assert_eq!(encoded.encoding, WireEncoding::Base64);
        assert_eq!(encoded.decode().unwrap(), raw);
    }

    #[test]
    fn repeated_headers_keep_their_order() {
        let mut headers = HeaderMap::new();
        headers.append("set-cookie", HeaderValue::from_static("a=1"));
        headers.append("set-cookie", HeaderValue::from_static("b=2"));
        let pairs = header_pairs(&headers);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].value.utf8().unwrap(), "a=1");
        assert_eq!(pairs[1].value.utf8().unwrap(), "b=2");
    }

    #[test]
    fn raw_query_merge_preserves_fixed_and_inbound_segments_byte_for_byte() {
        for (target, raw_query, has_query, expected) in [
            (
                "https://example.test/run?alt=sse&fixed=%2f",
                "tag=one&tag=two%2Fthree&empty=&bare",
                true,
                "https://example.test/run?alt=sse&fixed=%2f&tag=one&tag=two%2Fthree&empty=&bare",
            ),
            (
                "https://example.test/run?",
                "tag=one",
                true,
                "https://example.test/run?tag=one",
            ),
            (
                "https://example.test/run?fixed=1&",
                "tag=one",
                true,
                "https://example.test/run?fixed=1&tag=one",
            ),
            (
                "https://example.test/run",
                "",
                true,
                "https://example.test/run?",
            ),
            (
                "https://example.test/run?alt=sse",
                "",
                true,
                "https://example.test/run?alt=sse",
            ),
        ] {
            assert_eq!(merge_raw_query(target, raw_query, has_query), expected);
        }
    }

    #[test]
    fn legacy_flat_payloads_are_rejected() {
        let payload = serde_json::json!({
            "method": "POST",
            "path": "/v1/responses?x=1",
            "headers": {"content-type": "application/json"},
            "body": "{\"model\":\"gpt\"}",
            "target_upstream_model": "gpt-upstream"
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(SurfaceEnvelope::from_payload(&payload).is_err());
    }

    #[test]
    fn v2_payload_keeps_escaped_relative_path_raw_query_and_binary_body() {
        let payload = serde_json::json!({
            "wire": {
                "version": 2,
                "surface": "anthropic",
                "method": "POST",
                "escaped_relative_path": "/v1/files/a%2fb%3Fc%2Ftail",
                "raw_query": "tag=one&tag=two&escape=%2f&escape=%2F",
                "body": {"encoding": "base64", "data": "AP8B/gI="}
            }
        })
        .as_object()
        .unwrap()
        .clone();

        let envelope = SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(
            envelope.canonical_path(),
            "/anthropic/v1/files/a%2fb%3Fc%2Ftail"
        );
        assert_eq!(envelope.raw_query, "tag=one&tag=two&escape=%2f&escape=%2F");
        assert_eq!(envelope.body.decode().unwrap(), [0, 255, 1, 254, 2]);
    }

    #[test]
    fn decodes_openai_request_envelope_emitted_by_the_go_server() {
        let payload = serde_json::json!({
            "wire": {
                "version": 2,
                "surface": "openai",
                "operation": "responses",
                "protocol": "openai_responses",
                "method": "POST",
                "escaped_relative_path": "/v1/responses",
                "body": {
                    "encoding": "utf8",
                    "data": "{\"model\":\"gpt-5.6-luna\",\"stream\":true}"
                },
                "stream": true
            }
        })
        .as_object()
        .unwrap()
        .clone();

        let envelope = SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(envelope.surface, Some(ApiSurface::OpenAi));
        assert_eq!(envelope.operation, Some(ApiOperation::Responses));
        assert_eq!(envelope.canonical_path(), "/v1/responses");
        assert_eq!(serde_json::to_value(envelope).unwrap()["surface"], "openai");
    }

    #[test]
    fn canonical_path_always_mounts_v2_surface_relative_paths() {
        for (surface, path, expected) in [
            (
                Some(ApiSurface::Anthropic),
                "/v1/messages",
                "/anthropic/v1/messages",
            ),
            (
                Some(ApiSurface::Anthropic),
                "/anthropic/v1/messages",
                "/anthropic/anthropic/v1/messages",
            ),
            (
                Some(ApiSurface::Gemini),
                "/v1beta/models/gemini-3:generateContent",
                "/gemini/v1beta/models/gemini-3:generateContent",
            ),
            (
                Some(ApiSurface::Gemini),
                "/gemini/v1/foo",
                "/gemini/gemini/v1/foo",
            ),
            (None, "/gemini/v1/foo", "/gemini/v1/foo"),
        ] {
            let envelope = SurfaceEnvelope {
                surface,
                path: path.to_string(),
                ..SurfaceEnvelope::default()
            };

            assert_eq!(envelope.canonical_path(), expected);
        }
    }

    #[test]
    fn nested_wire_rejects_missing_or_wrong_version() {
        for wire in [
            serde_json::json!({
                "body": {"encoding": "utf8", "data": "ok"}
            }),
            serde_json::json!({
                "version": 1,
                "body": {"encoding": "utf8", "data": "ok"}
            }),
        ] {
            let payload = serde_json::json!({"wire": wire})
                .as_object()
                .unwrap()
                .clone();
            assert!(SurfaceEnvelope::from_payload(&payload).is_err());
        }
    }

    #[test]
    fn selected_target_protocol_round_trips() {
        let envelope = SurfaceEnvelope {
            selected_target_protocol: Some("openai_responses".to_string()),
            cache_identity: Some("c1_0123456789abcdef".to_string()),
            ..SurfaceEnvelope::default()
        };
        let payload = wire_payload(envelope).unwrap();
        let decoded = SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(
            decoded.selected_target_protocol.as_deref(),
            Some("openai_responses")
        );
        assert_eq!(
            decoded.cache_identity.as_deref(),
            Some("c1_0123456789abcdef")
        );
    }

    #[test]
    fn shared_golden_wire_v2_fixture() {
        let payload: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(include_str!("../../../testdata/surface-wire-v2.json")).unwrap();
        let envelope = SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(envelope.version, WIRE_VERSION);
        assert_eq!(envelope.path, "/v1beta/files/a%2fb%3Fc%2Ftail");
        assert_eq!(envelope.raw_query, "tag=one&tag=two&escape=%2f&escape=%2F");
        assert_eq!(envelope.body.decode().unwrap(), [0, 255, 1, 254, 2]);
        let headers = envelope.header_map().unwrap();
        assert_eq!(
            headers
                .get_all("set-cookie")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["a=1", "b=2"]
        );
        let encoded = serde_json::to_value(envelope).unwrap();
        assert_eq!(
            encoded.get("has_query"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    #[tokio::test]
    async fn v2_envelope_reaches_upstream_without_normalizing_target_or_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    if request.len() >= headers_end + 4 + 5 {
                        break;
                    }
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            request
        });

        let payload = serde_json::json!({
            "wire": {
                "version": 2,
                "surface": "anthropic",
                "method": "POST",
                "escaped_relative_path": "/v1/files/a%2fb%3Fc%2Ftail",
                "raw_query": "tag=one&tag=two&escape=%2f&escape=%2F",
                "body": {"encoding": "base64", "data": "AP8B/gI="}
            }
        })
        .as_object()
        .unwrap()
        .clone();
        let envelope = SurfaceEnvelope::from_payload(&payload).unwrap();
        let target = format!(
            "http://{address}{}?{}",
            envelope.canonical_path(),
            envelope.raw_query
        );
        let response = reqwest::Client::new()
            .post(target)
            .body(envelope.body.decode().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let request = upstream.await.unwrap();
        let headers_end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let request_line_end = request.windows(2).position(|part| part == b"\r\n").unwrap();
        assert_eq!(
            &request[..request_line_end],
            b"POST /anthropic/v1/files/a%2fb%3Fc%2Ftail?tag=one&tag=two&escape=%2f&escape=%2F HTTP/1.1"
        );
        assert_eq!(&request[headers_end + 4..], [0, 255, 1, 254, 2]);
    }
}
