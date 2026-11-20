// Images SSE can carry the final base64 image and usage in the same event.
// Reuse the private transport's bounded, anonymous spill file; deserialize only
// metadata so a large image is not copied into a second in-memory JSON tree.
struct ImageUsageObserver {
    usage: SupplierUpstreamUsageAccumulator,
    event: SupplierMessageBuffer,
    prefix: Vec<u8>,
    data_line: bool,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ImageUsageProjection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<serde_json::Value>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    event_type: Option<String>,
}

impl ImageUsageObserver {
    fn new() -> Self {
        Self {
            usage: SupplierUpstreamUsageAccumulator::new("openai_images"),
            event: SupplierMessageBuffer::with_limit(crate::MODEL_MESSAGE_MAX_BYTES),
            prefix: Vec::with_capacity(5),
            data_line: false,
        }
    }

    fn push(&mut self, mut chunk: &[u8]) -> Result<()> {
        use std::io::Write;
        while !chunk.is_empty() {
            let newline = chunk.iter().position(|byte| *byte == b'\n');
            let length = newline.unwrap_or(chunk.len());
            let mut line = &chunk[..length];
            if self.prefix.len() < 5 {
                let length = (5 - self.prefix.len()).min(line.len());
                self.prefix.extend_from_slice(&line[..length]);
                line = &line[length..];
                self.data_line = self.prefix == b"data:";
            }
            if self.data_line {
                self.event.write_all(line)?;
            }
            chunk = &chunk[length..];
            if newline.is_some() {
                if self.data_line {
                    self.event.write_all(b"\n")?;
                } else if trim_ascii(&self.prefix).is_empty() {
                    self.finish_event()?;
                }
                self.prefix.clear();
                self.data_line = false;
                chunk = &chunk[1..];
            }
        }
        Ok(())
    }

    fn finish_event(&mut self) -> Result<()> {
        if self.event.len() == 0 {
            return Ok(());
        }
        let event = std::mem::replace(
            &mut self.event,
            SupplierMessageBuffer::with_limit(crate::MODEL_MESSAGE_MAX_BYTES),
        );
        match event.into_json::<ImageUsageProjection>() {
            Ok(projection) => self.usage.observe_json(&serde_json::to_value(projection)?),
            // [DONE] and non-JSON extension events carry no usage. I/O failures
            // must remain observable instead of silently losing final metering.
            Err(error)
                if error
                    .downcast_ref::<serde_json::Error>()
                    .is_some_and(|error| !error.is_io()) => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn finish(
        mut self,
        model: &str,
    ) -> Result<(
        Option<SupplierUpstreamUsage>,
        Option<crate::upstream_failure::Failure>,
    )> {
        self.finish_event()?;
        Ok(self.usage.finish_observed(Some(model)))
    }
}

#[cfg(test)]
mod image_usage_tests {
    use super::*;

    #[tokio::test]
    async fn siliconflow_image_usage_counts_only_returned_generation_urls() {
        use crate::source_driver::SourceDriverId::{OpenAiApi, SiliconflowApi};
        use crate::surface_wire::{SurfaceEnvelope, WireBody};
        use warp::Filter as _;

        const NATIVE: &str = r#"{ "images":[{"url":"https://images.invalid/one","seed":9},{"url":" "},{"url":"filtered","error":"blocked"},{"b64_json":"AA=="}], "timings":{"inference":2.0}, "seed":9007199254740993 }"#;
        const STANDARD: &str = r#"{ "data":[{"url":"https://images.invalid/standard"}], "images":[{"url":"https://images.invalid/native"}] }"#;
        const REJECTED: &str = r#"{ "code":20012, "message":"fixture rejection", "data":null }"#;
        const EMBEDDED_ERROR: &str =
            r#"{ "error":{"message":"fixture error"}, "images":[{"url":"filtered"}] }"#;
        let route = warp::path!("v1" / "images" / "generations")
            .and(warp::body::json())
            .map(|body: serde_json::Value| {
                let (status, content) = match body["prompt"].as_str().unwrap() {
                    "native" => (200, NATIVE),
                    "standard" => (200, STANDARD),
                    "rejected" => (400, REJECTED),
                    "embedded" => (200, EMBEDDED_ERROR),
                    _ => panic!("unexpected fixture prompt"),
                };
                warp::http::Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .header("etag", "\"upstream\"")
                    .header("x-ratelimit-remaining-requests", "12")
                    .body(content)
                    .unwrap()
            });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let client = Client::new();
        for (driver, prompt, status, upstream, quantity) in [
            (SiliconflowApi, "native", 200, NATIVE, 1),
            (SiliconflowApi, "standard", 200, STANDARD, 1),
            (SiliconflowApi, "rejected", 400, REJECTED, 0),
            (SiliconflowApi, "embedded", 200, EMBEDDED_ERROR, 0),
            (OpenAiApi, "native", 200, NATIVE, 0),
        ] {
            let channel =
                crate::coding_gateway::tests::channel(driver, &format!("http://{address}/v1"));
            let wire = SurfaceEnvelope {
                surface: Some(crate::surface::ApiSurface::OpenAi),
                operation: Some(crate::surface::ApiOperation::ImagesGenerations),
                method: "POST".into(),
                path: "/v1/images/generations".into(),
                body: WireBody::from_bytes(
                    serde_json::json!({"model":"fixture","prompt":prompt,"n":5})
                        .to_string()
                        .as_bytes(),
                ),
                selected_upstream_model: Some("fixture".into()),
                stream: Some(false),
                ..Default::default()
            };
            let message = SupplierMessage {
                id: format!("{driver:?}-{prompt}"),
                kind: "http_request".into(),
                output_credit: None,
                payload: crate::surface_wire::wire_payload(wire).unwrap(),
            };
            let (outbound, _receiver) = mpsc::channel(1);
            let started = now_unix();
            let payload = forward_supplier_request(
                &client,
                &supplier_from_channel_unfiltered(&channel),
                &message,
                false,
                &outbound,
            )
            .await
            .unwrap()
            .unwrap();
            let response = SurfaceEnvelope::from_payload(&payload).unwrap();
            assert_eq!(response.status, Some(status));
            let bytes = response.body.decode().unwrap();
            let headers = response.header_map().unwrap();
            assert_eq!(headers["x-ratelimit-remaining-requests"], "12");
            if driver == SiliconflowApi && prompt == "native" {
                let mut expected: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
                expected["data"] = serde_json::json!([{"url":"https://images.invalid/one"}]);
                let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                let created = actual["created"].as_i64().unwrap();
                assert!((started..=now_unix()).contains(&created));
                expected["created"] = created.into();
                assert_eq!(actual, expected);
                assert_eq!(
                    headers["content-length"]
                        .to_str()
                        .unwrap()
                        .parse::<usize>()
                        .unwrap(),
                    bytes.len()
                );
                assert!(!headers.contains_key("etag"));
            } else {
                assert_eq!(bytes, upstream.as_bytes());
                assert_eq!(headers["etag"], "\"upstream\"");
            }
            if quantity == 0 {
                assert!(
                    payload
                        .get("upstream_usage")
                        .is_none_or(serde_json::Value::is_null)
                );
            } else {
                let usage = &payload["upstream_usage"];
                let meters = usage["usage_meters"].as_array().unwrap();
                assert_eq!(meters.len(), 1);
                assert_eq!(meters[0]["meter_kind"], "image_output_unit");
                assert_eq!(meters[0]["quantity"], quantity);
                assert!(usage["input_tokens"].is_null() && usage["output_tokens"].is_null());
            }
        }
        server.abort();
    }

    #[test]
    fn image_usage_preserves_tokens_and_never_also_counts_images() {
        for details in [
            serde_json::Value::Null,
            serde_json::json!({"image_tokens":17,"text_tokens":3}),
        ] {
            let body = serde_json::json!({"data":[{"b64_json":"AA=="}], "usage":{
                "input_tokens":10,"output_tokens":20,"input_tokens_details":{"image_tokens":4},
                "output_tokens_details":details}})
            .to_string();
            let usage = upstream_image_usage_from_body(body.as_bytes()).unwrap();
            assert_eq!(usage.input_tokens, Some(10));
            assert!(
                usage
                    .usage_meters
                    .iter()
                    .any(|m| m.meter_kind == "image_output_tokens"
                        && m.quantity == if details.is_null() { 20 } else { 17 }
                        && m.included_in_total)
            );
            assert!(
                !usage
                    .usage_meters
                    .iter()
                    .any(|m| m.meter_kind == "image_output_unit")
            );
            let chat = upstream_usage_from_body("openai_responses", body.as_bytes()).unwrap();
            if details.is_null() {
                assert!(
                    !chat
                        .usage_meters
                        .iter()
                        .any(|m| m.meter_kind == "image_output_tokens")
                );
            }
        }
        let usage = upstream_image_usage_from_body(br#"{"n":9,"error":null,"data":[{"url":"https://example.test/a"},{"url":" "},{"b64_json":"AA==","error":"filtered"}]}"#).unwrap();
        assert_eq!(usage.usage_meters[0].quantity, 1);
        assert_eq!(usage.output_tokens, None);
    }

    #[test]
    fn image_sse_spills_large_payload_and_preserves_final_usage() {
        let mut observer = ImageUsageObserver::new();
        observer.push(b"event: image_generation.completed\r\ndata: {\"type\":\"image_generation.completed\",\"b64_json\":\"").unwrap();
        let chunk = vec![b'A'; 64 * 1024];
        for _ in 0..80 {
            observer.push(&chunk).unwrap();
        }
        assert_eq!(observer.event.memory_capacity(), 0);
        observer.push(b"\",\r\ndata: \"usage\":{\"input_tokens\":3,\"output_tokens\":7}}\r\n\r\ndata: [DONE]\n\n").unwrap();
        let (usage, failure) = observer.finish("gpt-image-2").unwrap();
        assert!(failure.is_none());
        let usage = usage.unwrap();
        assert_eq!(usage.output_tokens, Some(7));
        assert!(
            usage
                .usage_meters
                .iter()
                .any(|m| m.meter_kind == "image_output_tokens" && m.quantity == 7)
        );
    }

    #[test]
    fn image_sse_handles_fragmented_prefix_and_terminal_error() {
        let mut observer = ImageUsageObserver::new();
        for chunk in b"data: {\"type\":\"error\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"rate limit\"}}\n\n".chunks(1) {
            observer.push(chunk).unwrap();
        }
        assert!(observer.finish("gpt-image-2").unwrap().1.is_some());
    }
}
