pub(crate) async fn response_from_target_as_inbound(
    resp: reqwest::Response,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
    stream_requested: bool,
    tool_mapping: crate::protocol::conversion::ToolWireMap,
) -> Result<warp::reply::Response> {
    let status = warp::http::StatusCode::from_u16(resp.status().as_u16())?;
    let mut headers = resp.headers().clone();
    let content_type = response_content_type(&resp);
    if status.is_success() && stream_requested && content_type.contains("event-stream") {
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        let converter = inbound_sse_stream_converter(inbound_protocol, target_protocol, model)?.with_tool_mapping(tool_mapping);
        return response_from_converted_reqwest_stream(
            status,
            &headers,
            resp.bytes_stream(),
            converter,
        );
    }
    let raw = if content_type.contains("event-stream") {
        crate::sse_buffer::read_sse_body_with_limits(resp).await?
    } else {
        resp.text().await?
    };
    let failure_payload = (!status.is_success()).then(|| {
        let mut payload =
            subscription_http_response_payload(status.as_u16(), &content_type, raw.clone());
        insert_subscription_route_model_failure_evidence(
            &mut payload,
            status.as_u16(),
            &raw,
            Some(model),
        );
        payload
    });
    let body = if !status.is_success() {
        raw
    } else if content_type.contains("event-stream") {
        if stream_requested {
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("text/event-stream"),
            );
            if tool_mapping.is_empty() {
                convert_complete_sse(raw.as_bytes(), target_protocol, inbound_protocol, model)?
            } else {
                let mut converter = inbound_sse_stream_converter(inbound_protocol, target_protocol, model)?.with_tool_mapping(tool_mapping);
                let mut output = converter.push(raw.as_bytes()).to_vec();
                output.extend_from_slice(&converter.finish());
                String::from_utf8(output)?
            }
        } else {
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
            sse_body_from_target_as_inbound_with_report_with_tools(&raw, inbound_protocol, target_protocol, model, &tool_mapping)?.body
        }
    } else if stream_requested {
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        sse_body_from_target_body_as_inbound_with_report_with_tools(&raw, inbound_protocol, target_protocol, model, &tool_mapping)?.body
    } else {
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        response_body_from_target_as_inbound_with_report_with_tools(&raw, inbound_protocol, target_protocol, model, &tool_mapping)?.body
    };
    let mut response = response_from_body(status, &headers, body.into())?;
    if let Some(payload) = failure_payload.as_ref() {
        attach_local_model_failure_evidence(&mut response, payload);
    }
    Ok(response)
}

pub(crate) enum InboundSseStreamConverter {
    Passthrough,
    Canonical(Box<crate::protocol::stream::StreamConverter>),
}

impl InboundSseStreamConverter {
    pub(crate) fn with_tool_mapping(self, mapping: crate::protocol::conversion::ToolWireMap) -> Self {
        match self {
            Self::Canonical(converter) => Self::Canonical(Box::new((*converter).with_tool_mapping(mapping))),
            other => other,
        }
    }
    pub(crate) fn push(&mut self, chunk: &[u8]) -> bytes::Bytes {
        match self {
            Self::Passthrough => bytes::Bytes::copy_from_slice(chunk),
            Self::Canonical(converter) => converter.push(chunk),
        }
    }

    pub(crate) fn finish(&mut self) -> bytes::Bytes {
        match self {
            Self::Passthrough => bytes::Bytes::new(),
            Self::Canonical(converter) => converter.finish(),
        }
    }

    pub(crate) fn fail(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
    ) -> bytes::Bytes {
        let message = message.into();
        match self {
            Self::Canonical(converter) => converter.fail(code, message),
            Self::Passthrough => bytes::Bytes::from(format!(
                "event: error\ndata: {}\n\n",
                serde_json::json!({
                    "type":"error",
                    "error":{"type":code,"message":message}
                })
            )),
        }
    }

    pub(crate) fn failure(&self) -> Option<String> {
        match self {
            Self::Passthrough => None,
            Self::Canonical(converter) => converter.failure().map(ToString::to_string),
        }
    }

    pub(crate) fn take_notices(
        &mut self,
    ) -> Vec<crate::protocol::stream::StreamConversionNotice> {
        match self {
            Self::Passthrough => Vec::new(),
            Self::Canonical(converter) => converter.take_notices(),
        }
    }
}

pub(crate) fn inbound_sse_stream_converter(
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Result<InboundSseStreamConverter> {
    let inbound = crate::protocol::kind::ProtocolKind::parse(inbound_protocol)?;
    let target = crate::protocol::kind::ProtocolKind::parse(target_protocol)?;
    Ok(if inbound == target {
        InboundSseStreamConverter::Passthrough
    } else {
        InboundSseStreamConverter::Canonical(Box::new(
            crate::protocol::stream::stream_converter(target, inbound, model),
        ))
    })
}

fn response_from_converted_reqwest_stream<S>(
    status: warp::http::StatusCode,
    headers: &reqwest::header::HeaderMap,
    stream: S,
    converter: InboundSseStreamConverter,
) -> Result<warp::reply::Response>
where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    let stream: BoxStream<'static, Result<bytes::Bytes, reqwest::Error>> = Box::pin(stream);
    let converted = futures_util::stream::unfold(
        (stream, converter, false),
        |(mut stream, mut converter, done)| async move {
            if done {
                return None;
            }
            loop {
                let next = match tokio::time::timeout(
                    Duration::from_secs(
                        crate::protocol::stream::STREAM_INACTIVITY_TIMEOUT_SECS,
                    ),
                    stream.next(),
                )
                .await
                {
                    Ok(next) => next,
                    Err(_) => {
                        let error = converter.fail(
                            "stream_inactivity_timeout",
                            "upstream stream produced no event before the inactivity deadline",
                        );
                        return Some((Ok(error), (stream, converter, true)));
                    }
                };
                let Some(next) = next else {
                    break;
                };
                match next {
                    Ok(chunk) => {
                        let converted = converter.push(&chunk);
                        if !converted.is_empty() {
                            return Some((Ok(converted), (stream, converter, false)));
                        }
                    }
                    Err(error) => return Some((Err(error), (stream, converter, true))),
                }
            }
            let tail = converter.finish();
            (!tail.is_empty()).then_some((Ok(tail), (stream, converter, true)))
        },
    );
    response_from_stream(status, headers, converted)
}

pub(crate) struct ReportedResponseConversion {
    pub(crate) body: String,
    pub(crate) faults: Vec<ImprovementFault>,
}

pub(crate) fn improvement_faults_from_stream_notices(
    notices: Vec<crate::protocol::stream::StreamConversionNotice>,
    source_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Vec<ImprovementFault> {
    notices
        .into_iter()
        .map(|notice| {
            ImprovementFault::conversion_warning(
                notice.code,
                source_protocol,
                target_protocol,
                model,
                &notice.path,
                &notice.summary,
            )
        })
        .collect()
}

pub(crate) fn response_body_from_target_as_inbound(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Result<String> {
    Ok(response_body_from_target_as_inbound_with_report(
        raw,
        inbound_protocol,
        target_protocol,
        model,
    )?
    .body)
}

pub(crate) fn response_body_from_target_as_inbound_with_report(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Result<ReportedResponseConversion> {
    response_body_from_target_as_inbound_with_report_with_tools(raw, inbound_protocol, target_protocol, model, &Default::default())
}

pub(crate) fn response_body_from_target_as_inbound_with_report_with_tools(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
    tool_mapping: &crate::protocol::conversion::ToolWireMap,
) -> Result<ReportedResponseConversion> {
    let inbound = crate::protocol::kind::ProtocolKind::parse(inbound_protocol)?;
    let target = crate::protocol::kind::ProtocolKind::parse(target_protocol)?;
    if inbound == target {
        return Ok(ReportedResponseConversion {
            body: raw.to_string(),
            faults: Vec::new(),
        });
    }
    let value = serde_json::from_str(raw)?;
    let (source_context, inbound_context) = response_contexts(target, inbound, model);
    let converted = if tool_mapping.is_empty() {
        crate::protocol::convert_response_non_stream(target, inbound, &value, &source_context, &inbound_context)
    } else {
        let mut response = crate::protocol::decode_response(target, &value, &source_context)?;
        tool_mapping.restore_response(&mut response)?;
        crate::protocol::convert_canonical_response(target, inbound, response, &value, &source_context, &inbound_context)
    }
    .map_err(|error| anyhow!("{}: {error}", error.code()))?;
    let faults = improvement_faults_from_conversion_plan(
        &converted.plan,
        target.as_str(),
        inbound.as_str(),
        model,
    );
    Ok(ReportedResponseConversion {
        body: serde_json::to_string(&converted.body)?,
        faults,
    })
}

pub(crate) fn sse_body_from_target_as_inbound(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Result<String> {
    Ok(sse_body_from_target_as_inbound_with_report(
        raw,
        inbound_protocol,
        target_protocol,
        model,
    )?
    .body)
}

pub(crate) fn sse_body_from_target_as_inbound_with_report(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Result<ReportedResponseConversion> {
    sse_body_from_target_as_inbound_with_report_with_tools(raw, inbound_protocol, target_protocol, model, &Default::default())
}

pub(crate) fn sse_body_from_target_as_inbound_with_report_with_tools(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
    tool_mapping: &crate::protocol::conversion::ToolWireMap,
) -> Result<ReportedResponseConversion> {
    let inbound = crate::protocol::kind::ProtocolKind::parse(inbound_protocol)?;
    let target = crate::protocol::kind::ProtocolKind::parse(target_protocol)?;
    let (mut response, notices) =
        canonical_response_from_target_sse_with_notices(raw.as_bytes(), target, model)?;
    if inbound != target { tool_mapping.restore_response(&mut response)?; }
    let mut faults = improvement_faults_from_stream_notices(
        notices,
        target.as_str(),
        inbound.as_str(),
        model,
    );
    if inbound == target {
        let context = crate::protocol::adapters::AdapterContext::runtime(
            format!("response:{}", target.as_str()),
            "response-upstream",
            None,
            model,
        );
        let body = crate::protocol::encode_response(
            target,
            &response,
            &crate::protocol::conversion::ConversionPlan::native_passthrough(target),
            &context,
        )
        .map_err(|error| anyhow!("{}: {error}", error.code()))?;
        return Ok(ReportedResponseConversion {
            body: serde_json::to_string(&body)?,
            faults,
        });
    }
    let (source_context, inbound_context) = response_contexts(target, inbound, model);
    let converted = crate::protocol::convert_canonical_response(
        target,
        inbound,
        response,
        &serde_json::Value::Null,
        &source_context,
        &inbound_context,
    )
    .map_err(|error| anyhow!("{}: {error}", error.code()))?;
    faults.extend(improvement_faults_from_conversion_plan(
        &converted.plan,
        target.as_str(),
        inbound.as_str(),
        model,
    ));
    Ok(ReportedResponseConversion {
        body: serde_json::to_string(&converted.body)?,
        faults,
    })
}

pub(crate) fn sse_body_from_target_body_as_inbound(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Result<String> {
    Ok(sse_body_from_target_body_as_inbound_with_report(
        raw,
        inbound_protocol,
        target_protocol,
        model,
    )?
    .body)
}

pub(crate) fn sse_body_from_target_body_as_inbound_with_report(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Result<ReportedResponseConversion> {
    sse_body_from_target_body_as_inbound_with_report_with_tools(raw, inbound_protocol, target_protocol, model, &Default::default())
}

pub(crate) fn sse_body_from_target_body_as_inbound_with_report_with_tools(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
    tool_mapping: &crate::protocol::conversion::ToolWireMap,
) -> Result<ReportedResponseConversion> {
    let inbound = crate::protocol::kind::ProtocolKind::parse(inbound_protocol)?;
    let target = crate::protocol::kind::ProtocolKind::parse(target_protocol)?;
    let source_context = crate::protocol::adapters::AdapterContext::runtime(
        format!("response:{}", target.as_str()),
        "response-upstream",
        None,
        model,
    );
    let value = serde_json::from_str(raw)?;
    let mut response = crate::protocol::decode_response(target, &value, &source_context)
        .map_err(|error| anyhow!("{}: {error}", error.code()))?;
    if inbound != target { tool_mapping.restore_response(&mut response)?; }
    let (events, mut notices) =
        crate::protocol::stream::events_from_response_with_notices(&response)?;
    notices.extend(crate::protocol::stream::stream_rendering_notices(
        &events, inbound,
    ));
    let faults = improvement_faults_from_stream_notices(
        notices,
        target.as_str(),
        inbound.as_str(),
        model,
    );
    Ok(ReportedResponseConversion {
        body: render_events(&events, inbound, model)?,
        faults,
    })
}

#[cfg(test)]
pub(crate) fn canonical_stream_events_from_target_sse(
    raw: &str,
    target_protocol: &str,
    model: &str,
) -> Result<Vec<crate::protocol::ir::CanonicalStreamEvent>> {
    let target = crate::protocol::kind::ProtocolKind::parse(target_protocol)?;
    parse_stream_events(raw.as_bytes(), target, model)
}

pub(crate) struct ReportedCanonicalStreamEvents {
    pub(crate) events: Vec<crate::protocol::ir::CanonicalStreamEvent>,
    pub(crate) faults: Vec<ImprovementFault>,
}

pub(crate) fn canonical_stream_events_from_target_sse_with_report(
    raw: &str,
    inbound_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Result<ReportedCanonicalStreamEvents> {
    let inbound = crate::protocol::kind::ProtocolKind::parse(inbound_protocol)?;
    let target = crate::protocol::kind::ProtocolKind::parse(target_protocol)?;
    let (events, notices) = parse_stream_events_with_notices(raw.as_bytes(), target, model)?;
    Ok(ReportedCanonicalStreamEvents {
        events,
        faults: improvement_faults_from_stream_notices(
            notices,
            target.as_str(),
            inbound.as_str(),
            model,
        ),
    })
}

pub(crate) fn sse_from_canonical_stream_events(
    events: &[crate::protocol::ir::CanonicalStreamEvent],
    inbound_protocol: &str,
    model: &str,
) -> Result<String> {
    let inbound = crate::protocol::kind::ProtocolKind::parse(inbound_protocol)?;
    render_events(events, inbound, model)
}

fn convert_complete_sse(
    raw: &[u8],
    source: &str,
    target: &str,
    model: &str,
) -> Result<String> {
    let source = crate::protocol::kind::ProtocolKind::parse(source)?;
    let target = crate::protocol::kind::ProtocolKind::parse(target)?;
    if source == target {
        return String::from_utf8(raw.to_vec()).map_err(Into::into);
    }
    let events = parse_stream_events(raw, source, model)?;
    render_events(&events, target, model)
}

fn parse_stream_events(
    raw: &[u8],
    protocol: crate::protocol::kind::ProtocolKind,
    model: &str,
) -> Result<Vec<crate::protocol::ir::CanonicalStreamEvent>> {
    Ok(parse_stream_events_with_notices(raw, protocol, model)?.0)
}

fn parse_stream_events_with_notices(
    raw: &[u8],
    protocol: crate::protocol::kind::ProtocolKind,
    model: &str,
) -> Result<(
    Vec<crate::protocol::ir::CanonicalStreamEvent>,
    Vec<crate::protocol::stream::StreamConversionNotice>,
)> {
    let mut parser = crate::protocol::stream::StreamParser::new(protocol, model);
    let mut events = parser.push(raw)?;
    events.extend(parser.finish()?);
    Ok((events, parser.take_notices()))
}

fn canonical_response_from_target_sse_with_notices(
    raw: &[u8],
    protocol: crate::protocol::kind::ProtocolKind,
    model: &str,
) -> Result<(
    crate::protocol::ir::CanonicalResponseV2,
    Vec<crate::protocol::stream::StreamConversionNotice>,
)> {
    let (events, notices) = parse_stream_events_with_notices(raw, protocol, model)?;
    let mut accumulator = crate::protocol::stream::CanonicalAccumulator::new();
    for event in &events {
        accumulator.push(event)?;
    }
    Ok((accumulator.finish()?, notices))
}

fn render_events(
    events: &[crate::protocol::ir::CanonicalStreamEvent],
    protocol: crate::protocol::kind::ProtocolKind,
    model: &str,
) -> Result<String> {
    let mut renderer = crate::protocol::stream::StreamRenderer::new(protocol, model);
    let mut output = Vec::new();
    for event in events {
        output.extend_from_slice(&renderer.push(event)?);
    }
    output.extend_from_slice(&renderer.finish()?);
    String::from_utf8(output).map_err(Into::into)
}

fn response_contexts(
    source: crate::protocol::kind::ProtocolKind,
    target: crate::protocol::kind::ProtocolKind,
    model: &str,
) -> (
    crate::protocol::adapters::AdapterContext,
    crate::protocol::adapters::AdapterContext,
) {
    (
        crate::protocol::adapters::AdapterContext::runtime(
            format!("response:{}", source.as_str()),
            "response-upstream",
            None,
            model,
        ),
        crate::protocol::adapters::AdapterContext::runtime(
            format!("response:{}", target.as_str()),
            "response-inbound",
            None,
            model,
        ),
    )
}

#[cfg(test)]
pub(crate) fn responses_body_to_chat_body(raw: &str, model: &str) -> Result<String> {
    response_body_from_target_as_inbound(raw, "openai_chat", "openai_responses", model)
}

#[cfg(test)]
pub(crate) fn anthropic_message_to_chat_body(raw: &str, model: &str) -> Result<String> {
    response_body_from_target_as_inbound(raw, "openai_chat", "anthropic_messages", model)
}

#[cfg(test)]
pub(crate) fn gemini_native_response_to_chat_body(raw: &str, model: &str) -> Result<String> {
    response_body_from_target_as_inbound(raw, "openai_chat", "gemini_native", model)
}
