use bytes::Bytes;
use futures_util::{StreamExt, stream};
use http::{HeaderMap, Method};
use http_body_util::BodyExt;
use std::borrow::Cow;
use warp::{Reply, reply::Response};

pub(super) fn for_tool(
    method: &Method,
    path: &str,
    request_headers: &HeaderMap,
    response: Response,
) -> Response {
    // xAI's pager and headless sampler set this on ordinary custom-model requests.
    let client = request_headers.get("x-grok-client-identifier");
    if method != Method::POST
        || path != "/v1/responses"
        || !matches!(
            client.map(|value| value.as_bytes()),
            Some(b"grok-pager" | b"grok-shell")
        )
        || !response.status().is_success()
        || !response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        || response
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        return response;
    }

    let (parts, body) = response.into_parts();
    let mut filter = MetadataFilter::default();
    let output = body
        .into_data_stream()
        .map(Some)
        .chain(stream::iter([None]))
        .flat_map(move |item| {
            let (bytes, error) = match item {
                Some(Ok(chunk)) => (filter.push(&chunk), None),
                // An incomplete SSE record belongs to the caller, even on a read error.
                tail => (
                    Bytes::from(std::mem::take(&mut filter.record)),
                    tail.and_then(Result::err),
                ),
            };
            stream::iter(
                (!bytes.is_empty())
                    .then_some(Ok(bytes))
                    .into_iter()
                    .chain(error.map(Err)),
            )
        });
    Response::from_parts(
        parts,
        warp::reply::stream(output).into_response().into_body(),
    )
}

#[derive(Default)]
struct MetadataFilter {
    record: Vec<u8>,
    in_line: bool,
    after_cr: bool,
    passthrough: bool,
}

impl MetadataFilter {
    fn push(&mut self, chunk: &[u8]) -> Bytes {
        let mut output = Vec::new();
        for part in chunk.split_inclusive(|b| matches!(b, b'\r' | b'\n')) {
            let crlf_suffix = self.after_cr && part == b"\n";
            self.after_cr = part.ends_with(b"\r");
            if crlf_suffix && self.record.is_empty() {
                output.push(b'\n');
                continue;
            }
            // Keep oversized same-protocol events streaming, including across chunk boundaries.
            if self.record.len() + part.len() > crate::protocol::stream::MAX_STREAM_BYTES {
                output.append(&mut self.record);
                self.passthrough = true;
            }
            if self.passthrough {
                output.extend_from_slice(part);
            } else {
                self.record.extend_from_slice(part);
            }
            if crlf_suffix {
                continue;
            }
            let end_of_line = self.after_cr || part.ends_with(b"\n");
            let blank = end_of_line && !self.in_line && part.len() == 1;
            self.in_line = !end_of_line;
            if blank {
                comment_metadata(&mut self.record);
                output.append(&mut self.record);
                self.passthrough = false;
            } else if end_of_line {
                // Forward named output and errors as soon as the name is known.
                self.passthrough |= has_other_event_name(&self.record);
                if self.passthrough
                    || self.record.starts_with(b":")
                    || self.record.starts_with(b"id:")
                    || self.record.starts_with(b"retry:")
                {
                    output.append(&mut self.record);
                }
            }
        }
        output.into()
    }
}

fn has_other_event_name(record: &[u8]) -> bool {
    let Some(name) = record.strip_prefix(b"event:") else {
        return false;
    };
    let name = name
        .split(|b| matches!(b, b'\r' | b'\n'))
        .next()
        .unwrap_or_default();
    !matches!(
        name.strip_prefix(b" ").unwrap_or(name),
        b"" | b"message" | b"response.metadata"
    )
}

fn comment_metadata(record: &mut [u8]) {
    #[derive(serde::Deserialize)]
    struct Event {
        #[serde(rename = "type")]
        kind: String,
        error: Option<serde::de::IgnoredAny>,
    }

    let Ok(text) = std::str::from_utf8(record) else {
        return;
    };
    let mut data: Option<Cow<'_, str>> = None;
    for line in text.split(['\r', '\n']) {
        if has_other_event_name(line.as_bytes()) {
            return;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        if field != "data" {
            continue;
        }
        let value = value.strip_prefix(' ').unwrap_or(value);
        match &mut data {
            None => data = Some(Cow::Borrowed(value)),
            Some(data) => {
                let data = data.to_mut();
                data.push('\n');
                data.push_str(value);
            }
        }
    }
    if !data.is_some_and(|data| {
        serde_json::from_str::<Event>(&data)
            .is_ok_and(|event| event.kind == "response.metadata" && event.error.is_none())
    }) {
        return;
    }
    // Comments dispatch no event. Preserve id/retry, line endings and byte positions
    // (including Content-Length) without exposing the private notification to the tool.
    for line in record.split_mut(|b| matches!(b, b'\r' | b'\n')) {
        if matches!(line.split(|b| *b == b':').next(), Some(b"data" | b"event")) {
            line.fill(b' ');
            line[0] = b':';
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_build_filters_only_dispatched_metadata_at_every_byte_split() {
        let delta = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"你好 response.metadata\"}\n\n";
        let private = concat!(
            "id: 41\r\nretry: 1000\r\n: heartbeat\r\n",
            "event: response.metadata\r\ndata: {\"metadata\":{},\r\ndata: \"type\":\"response.metadata\"}\r\n\r\n",
            "data: {\"type\":\"response.metadata\",\"headers\":{}}\r\r"
        );
        let rest = concat!(
            "event: response.metadata\ndata: {\"type\":\"error\",\"error\":{\"message\":\"keep\"}}\n\n",
            "data: {\"type\":\"response.metadata\",\"error\":{\"message\":\"keep too\"}}\n\n",
            "data: {\"type\":\"response.failed\"}\n\ndata: {\"type\":\"response.incomplete\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"metadata\":{\"keep\":true}}}\n\n",
            "data: {\"type\":\"response.future\"}\n\ndata: [DONE]\n\n",
            "event: response.metadata\ndata: {malformed}\n\n"
        );
        let wire = [delta, private, rest].concat();
        for split in 0..=wire.len() {
            let mut filter = MetadataFilter::default();
            let mut output = filter.push(&wire.as_bytes()[..split]).to_vec();
            output.extend(filter.push(&wire.as_bytes()[split..]));
            assert!(filter.record.is_empty());
            let output = std::str::from_utf8(&output).unwrap();
            assert_eq!(output.len(), wire.len());
            assert!(output.starts_with(delta) && output.ends_with(rest));
            let private = &output[delta.len()..delta.len() + private.len()];
            assert!(private.starts_with("id: 41\r\nretry: 1000\r\n: heartbeat\r\n"));
            assert!(!private.contains("data:") && !private.contains("response.metadata"));
        }
    }

    #[test]
    fn grok_build_emits_complete_events_before_the_next_chunk() {
        let mut filter = MetadataFilter::default();
        assert_eq!(filter.push(b": ping\n"), b": ping\n"[..]);
        for part in [
            b"event: response.output_text.delta\n".as_slice(),
            b"data: {\"delta\":\"vis",
            b"ible\"}\n\n",
        ] {
            assert_eq!(filter.push(part), part);
        }
        let delta = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"visible\"}\n\n";
        let start = [delta.as_slice(), b"data: {\"type\":\"response.met"].concat();
        assert_eq!(filter.push(&start), delta[..]);
        let end = filter.push(b"adata\"}\n\n");
        assert!(end.starts_with(b":") && end.ends_with(b"\n\n"));

        let large = vec![b'x'; crate::protocol::stream::MAX_STREAM_BYTES + 1];
        assert_eq!(filter.push(&large), large);
        assert_eq!(filter.push(b"\r\n\r\n"), b"\r\n\r\n"[..]);
        assert!(
            filter
                .push(b"data: {\"type\":\"response.metadata\"}\n\n")
                .starts_with(b":")
        );
    }

    #[tokio::test]
    async fn grok_build_preserves_a_partial_record_and_the_body_read_error() {
        let partial = Bytes::from_static(b"data: {\"type\":\"response.fail");
        let stream = stream::iter([
            Ok(partial.clone()),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "fixture read error",
            )),
        ]);
        let response = warp::reply::with_header(
            warp::reply::stream(stream),
            "content-type",
            "text/event-stream",
        )
        .into_response();
        let mut headers = HeaderMap::new();
        headers.insert("x-grok-client-identifier", "grok-pager".parse().unwrap());
        let response = for_tool(&Method::POST, "/v1/responses", &headers, response);
        let mut output = response.into_body().into_data_stream();
        assert_eq!(output.next().await.unwrap().unwrap(), partial);
        assert!(output.next().await.unwrap().is_err());
        assert!(output.next().await.is_none());
    }

    #[tokio::test]
    async fn grok_build_requires_the_consumer_and_successful_responses_sse() {
        const META: &str = "data: {\"type\":\"response.metadata\"}\n\n";
        for (id, method, path, status, content_type, filter) in [
            (
                "grok-shell",
                Method::POST,
                "/v1/responses",
                200,
                "text/event-stream",
                true,
            ),
            (
                "grok-pager",
                Method::POST,
                "/v1/responses",
                200,
                "text/event-stream; charset=utf-8",
                true,
            ),
            (
                "",
                Method::POST,
                "/v1/responses",
                200,
                "text/event-stream",
                false,
            ),
            (
                "grok-shell-extra",
                Method::POST,
                "/v1/responses",
                200,
                "text/event-stream",
                false,
            ),
            (
                "grok-pager",
                Method::GET,
                "/v1/responses",
                200,
                "text/event-stream",
                false,
            ),
            (
                "grok-pager",
                Method::POST,
                "/v1/chat/completions",
                200,
                "text/event-stream",
                false,
            ),
            (
                "grok-shell",
                Method::POST,
                "/v1/responses",
                502,
                "text/event-stream",
                false,
            ),
            (
                "grok-shell",
                Method::POST,
                "/v1/responses",
                200,
                "application/json",
                false,
            ),
            (
                "grok-shell",
                Method::POST,
                "/v1/responses",
                200,
                "text/plain; note=text/event-stream",
                false,
            ),
        ] {
            let mut headers = HeaderMap::new();
            if !id.is_empty() {
                headers.insert("x-grok-client-identifier", id.parse().unwrap());
            } else {
                headers.insert("originator", "codex_vscode".parse().unwrap());
            }
            let response = http::Response::builder()
                .status(status)
                .header("content-type", content_type)
                .header("content-length", META.len())
                .body(META.into())
                .unwrap();
            let response = for_tool(&method, path, &headers, response);
            assert_eq!(response.status().as_u16(), status);
            let length = response.headers()["content-length"]
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let output = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(output.len(), length);
            assert_eq!(output.as_ref() == META.as_bytes(), !filter);
        }
    }
}
