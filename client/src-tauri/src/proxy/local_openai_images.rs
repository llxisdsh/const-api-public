async fn forward_openai_subscription_image_request(
    client: &Client,
    channel: &ChannelConfig,
    path: &str,
    inbound_headers: &warp::http::HeaderMap,
    body: bytes::Bytes,
) -> Result<warp::reply::Response> {
    let operation = openai_subscription_image_operation(path)
        .ok_or_else(|| anyhow!("unsupported OpenAI subscription image operation"))?;
    let config = supplier_from_channel(channel);
    let guard = match subscription_safety_enter_with_capacity(
        &config,
        "",
        SubscriptionRequestCapacity::for_path(path),
    ) {
        Ok(guard) => guard,
        Err(payload) => {
            let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload)?;
            let mut headers = wire.header_map()?;
            if let Some(retry_after) = payload
                .get("retry_after_seconds")
                .and_then(serde_json::Value::as_i64)
                .filter(|seconds| *seconds > 0)
            {
                headers.insert(
                    reqwest::header::RETRY_AFTER,
                    retry_after.to_string().parse()?,
                );
            }
            let mut response = response_from_body(
                warp::http::StatusCode::from_u16(wire.status.unwrap_or(429))?,
                &headers,
                wire.body.decode()?.into(),
            )?;
            attach_local_model_failure_evidence(&mut response, &payload);
            return Ok(response);
        }
    };
    let upstream_url = codex_image_url_from_responses_url(&codex_responses_url(), operation);
    let payload =
        normalize_openai_subscription_image_payload(operation, inbound_headers, body).await?;
    let mut response = send_openai_subscription_image_request(
        client,
        channel,
        &upstream_url,
        inbound_headers,
        payload,
    )
    .await?;
    response.headers_mut().remove(reqwest::header::SET_COOKIE);
    // A slot follows the image bytes, not just receipt of HTTP headers.
    response_from_reqwest(guard.hold_response(response), true).await
}
