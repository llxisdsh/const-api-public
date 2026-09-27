pub(crate) const EXECUTION_EVIDENCE_FIELD: &str = "_const_execution";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct ExecutionFailureDiagnostics {
    pub(crate) error_class: String,
    pub(crate) phase: String,
    pub(crate) cause_codes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) os_error: Option<i32>,
    pub(crate) chain_depth: usize,
    pub(crate) chain_fingerprint: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct ExecutionRecommendation {
    pub(crate) consumer_charge: String,
    pub(crate) supplier_settlement: String,
    pub(crate) consumer_compensation: String,
    pub(crate) supplier_penalty: String,
    pub(crate) retry: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct ExecutionEvidence {
    pub(crate) version: u8,
    pub(crate) outcome: String,
    pub(crate) stage: String,
    pub(crate) fault_domain_hint: String,
    pub(crate) error_code: String,
    pub(crate) upstream_attempted: bool,
    pub(crate) upstream_response_received: bool,
    pub(crate) response_started: bool,
    pub(crate) usable_output_produced: bool,
    pub(crate) recommendation: ExecutionRecommendation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) diagnostics: Option<ExecutionFailureDiagnostics>,
}

#[derive(Debug)]
pub(crate) struct SupplierExecutionFailure {
    pub(crate) evidence: ExecutionEvidence,
    message: String,
}

impl std::fmt::Display for SupplierExecutionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SupplierExecutionFailure {}

fn recommendation(
    consumer_charge: &str,
    supplier_settlement: &str,
    consumer_compensation: &str,
    supplier_penalty: &str,
    retry: &str,
) -> ExecutionRecommendation {
    ExecutionRecommendation {
        consumer_charge: consumer_charge.to_string(),
        supplier_settlement: supplier_settlement.to_string(),
        consumer_compensation: consumer_compensation.to_string(),
        supplier_penalty: supplier_penalty.to_string(),
        retry: retry.to_string(),
    }
}

pub(crate) fn execution_failure(
    stage: &str,
    fault_domain_hint: &str,
    error_code: &str,
    upstream_attempted: bool,
    upstream_response_received: bool,
    response_started: bool,
    usable_output_produced: bool,
    recommendation: ExecutionRecommendation,
    error: impl std::fmt::Display,
) -> anyhow::Error {
    anyhow::Error::new(SupplierExecutionFailure {
        evidence: ExecutionEvidence {
            version: 1,
            outcome: "failure".to_string(),
            stage: stage.to_string(),
            fault_domain_hint: fault_domain_hint.to_string(),
            error_code: error_code.to_string(),
            upstream_attempted,
            upstream_response_received,
            response_started,
            usable_output_produced,
            recommendation,
            diagnostics: None,
        },
        message: format!("{error_code}: {error}"),
    })
}

pub(crate) fn platform_execution_failure(
    stage: &str,
    error_code: &str,
    error: impl std::fmt::Display,
) -> anyhow::Error {
    execution_failure(
        stage,
        "platform",
        error_code,
        false,
        false,
        false,
        false,
        recommendation("deny", "deny", "none", "ineligible", "none"),
        error,
    )
}

pub(crate) fn response_conversion_failure(
    response_started: bool,
    usable_output_produced: bool,
    error: impl std::fmt::Display,
) -> anyhow::Error {
    execution_failure(
        "response_protocol_convert",
        "unknown",
        "response_conversion_failed",
        true,
        true,
        response_started,
        usable_output_produced,
        recommendation("deny", "review", "none", "review", "other_channel"),
        error,
    )
}

pub(crate) fn request_conversion_failure(error: impl std::fmt::Display) -> anyhow::Error {
    execution_failure(
        "request_protocol_convert",
        "platform",
        "conversion_failed",
        false,
        false,
        false,
        false,
        recommendation("deny", "deny", "none", "ineligible", "other_channel"),
        error,
    )
}

pub(crate) fn upstream_stream_failure(
    response_started: bool,
    usable_output_produced: bool,
    error: impl std::fmt::Display,
) -> anyhow::Error {
    execution_failure(
        "upstream_stream",
        "upstream_provider",
        "stream_inactivity_timeout",
        true,
        true,
        response_started,
        usable_output_produced,
        recommendation("deny", "review", "none", "eligible", "other_channel"),
        error,
    )
}

fn channel_error_evidence(error: &anyhow::Error) -> Option<ExecutionEvidence> {
    let code = crate::channel_executor::channel_execution_error_code(error)?;
    if code == "ambiguous_transport" {
        return Some(ambiguous_transport_evidence());
    }
    let (stage, domain, attempted, settlement, penalty, retry) = match code {
        "conversion_failed" => (
            "request_protocol_convert",
            "platform",
            false,
            "deny",
            "ineligible",
            "other_channel",
        ),
        "surface_operation_not_supported" => (
            "request_surface_resolve",
            "capability",
            false,
            "deny",
            "ineligible",
            "other_channel",
        ),
        "authentication_failed" => (
            "upstream_auth",
            "supplier_config",
            false,
            "deny",
            "eligible",
            "other_channel",
        ),
        "upstream_timeout" => (
            "upstream_transport",
            "upstream_provider",
            true,
            "deny",
            "eligible",
            "other_channel",
        ),
        "upstream_rejected" => (
            "upstream_transport",
            "upstream_provider",
            true,
            "deny",
            "eligible",
            "other_channel",
        ),
        _ => return None,
    };
    Some(ExecutionEvidence {
        version: 1,
        outcome: "failure".to_string(),
        stage: stage.to_string(),
        fault_domain_hint: domain.to_string(),
        error_code: code.to_string(),
        upstream_attempted: attempted,
        upstream_response_received: false,
        response_started: false,
        usable_output_produced: false,
        recommendation: recommendation("deny", settlement, "none", penalty, retry),
        diagnostics: None,
    })
}

fn ambiguous_transport_evidence() -> ExecutionEvidence {
    ExecutionEvidence {
        version: 1,
        outcome: "failure".to_string(),
        stage: "upstream_transport".to_string(),
        fault_domain_hint: "transport".to_string(),
        error_code: "ambiguous_transport".to_string(),
        upstream_attempted: true,
        upstream_response_received: false,
        response_started: false,
        usable_output_produced: false,
        recommendation: recommendation("deny", "review", "none", "ineligible", "none"),
        diagnostics: None,
    }
}

pub(crate) fn execution_evidence_for_error(error: &anyhow::Error) -> ExecutionEvidence {
    let mut evidence = classify_execution_evidence_for_error(error);
    evidence.diagnostics = Some(execution_failure_diagnostics(error));
    evidence
}

fn classify_execution_evidence_for_error(error: &anyhow::Error) -> ExecutionEvidence {
    if let Some(failure) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SupplierExecutionFailure>())
    {
        return failure.evidence.clone();
    }
    if crate::upstream_transport::is_ambiguous_transport_error(error) {
        return ambiguous_transport_evidence();
    }
    let structured_error = error.chain().find_map(|cause| {
        let value = serde_json::from_str::<serde_json::Value>(&cause.to_string()).ok()?;
        (value.get("error_kind").and_then(serde_json::Value::as_str) == Some("capability_mismatch"))
            .then_some(value)
    });
    if structured_error
        .as_ref()
        .and_then(|value| value.get("error_kind"))
        .and_then(serde_json::Value::as_str)
        == Some("capability_mismatch")
    {
        let error_code = structured_error
            .as_ref()
            .and_then(|value| value.get("issue_code"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unsupported_required_feature")
            .to_string();
        return ExecutionEvidence {
            version: 1,
            outcome: "failure".to_string(),
            stage: "request_protocol_convert".to_string(),
            fault_domain_hint: "capability".to_string(),
            error_code,
            upstream_attempted: false,
            upstream_response_received: false,
            response_started: false,
            usable_output_produced: false,
            recommendation: recommendation("deny", "deny", "none", "ineligible", "other_channel"),
            diagnostics: None,
        };
    }
    if error
        .chain()
        .any(|cause| cause.is::<crate::upstream_transport::UpstreamTransportError>())
    {
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_builder)
        }) {
            return classify_execution_evidence_for_error(&platform_execution_failure(
                "upstream_request_build",
                "invalid_upstream_request",
                "HTTP request could not be built",
            ));
        }
        // A send/read error is not an upstream rejection. Even is_connect()
        // can describe a later redirect hop: reqwest attaches the original URL
        // to that error. Without first-hop evidence, never promise safe replay.
        return ambiguous_transport_evidence();
    }
    if let Some(evidence) = channel_error_evidence(error) {
        return evidence;
    }
    ExecutionEvidence {
        version: 1,
        outcome: "failure".to_string(),
        stage: "client_execution".to_string(),
        fault_domain_hint: "unknown".to_string(),
        error_code: "supplier_execution_failed".to_string(),
        upstream_attempted: false,
        upstream_response_received: false,
        response_started: false,
        usable_output_produced: false,
        recommendation: recommendation("deny", "deny", "none", "review", "none"),
        diagnostics: None,
    }
}

fn execution_failure_diagnostics(error: &anyhow::Error) -> ExecutionFailureDiagnostics {
    let mut diagnostics = ExecutionFailureDiagnostics {
        error_class: "unknown".to_string(),
        phase: "unknown".to_string(),
        cause_codes: Vec::new(),
        os_error: None,
        chain_depth: 0,
        chain_fingerprint: String::new(),
    };
    let mut fingerprint = Sha256::new();
    for cause in error.chain().take(16) {
        diagnostics.chain_depth += 1;
        // Only the digest leaves this function. Never forward free-form source
        // strings, URLs, headers, local paths, or provider response bodies.
        fingerprint.update(cause.to_string().as_bytes());
        fingerprint.update([0]);
        if cause.is::<crate::upstream_transport::UpstreamTransportError>() {
            diagnostics.phase = "upstream_send".to_string();
        }
        if let Some(request) = cause.downcast_ref::<reqwest::Error>() {
            diagnostics.error_class = if request.is_builder() {
                "request_build"
            } else if request.is_redirect() {
                "redirect"
            } else if request.is_connect() && request.is_timeout() {
                "connect_timeout"
            } else if request.is_connect() {
                "connect"
            } else if request.is_timeout() {
                "request_timeout"
            } else if request.is_body() {
                "body_io"
            } else if request.is_decode() {
                "response_decode"
            } else {
                "http_request"
            }
            .to_string();
        }
        let code = if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            diagnostics.os_error = io.raw_os_error().or(diagnostics.os_error);
            Some(match io.kind() {
                std::io::ErrorKind::ConnectionRefused => "connection_refused",
                std::io::ErrorKind::ConnectionReset => "connection_reset",
                std::io::ErrorKind::ConnectionAborted => "connection_aborted",
                std::io::ErrorKind::NotConnected => "not_connected",
                std::io::ErrorKind::TimedOut => "timed_out",
                std::io::ErrorKind::UnexpectedEof => "unexpected_eof",
                std::io::ErrorKind::BrokenPipe => "broken_pipe",
                std::io::ErrorKind::PermissionDenied => "permission_denied",
                std::io::ErrorKind::NotFound => "not_found",
                std::io::ErrorKind::InvalidData => "invalid_data",
                std::io::ErrorKind::InvalidInput => "invalid_input",
                _ => "io_error",
            })
        } else if let Some(json) = cause.downcast_ref::<serde_json::Error>() {
            Some(match json.classify() {
                serde_json::error::Category::Io => "json_io",
                serde_json::error::Category::Syntax => "json_syntax",
                serde_json::error::Category::Data => "json_data",
                serde_json::error::Category::Eof => "json_eof",
            })
        } else {
            None
        };
        if let Some(code) = code {
            if !diagnostics
                .cause_codes
                .iter()
                .any(|existing| existing == code)
            {
                diagnostics.cause_codes.push(code.to_string());
            }
        }
    }
    diagnostics.chain_fingerprint = hex::encode(&fingerprint.finalize()[..12]);
    diagnostics
}

pub(crate) fn attach_execution_evidence(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    evidence: &ExecutionEvidence,
) {
    if let Ok(value) = serde_json::to_value(evidence) {
        payload.insert(EXECUTION_EVIDENCE_FIELD.to_string(), value);
    }
}

#[cfg(test)]
mod execution_evidence_tests {
    use super::*;

    #[test]
    fn diagnostics_keep_typed_causes_without_free_form_secrets() {
        let error = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private path C:\\credentials\\token.json",
        ))
        .context(
            "credential sk-private-test-secret https://user:password@example.test/?key=secret",
        );
        let evidence = execution_evidence_for_error(&error);
        let diagnostics = evidence.diagnostics.unwrap();
        assert_eq!(diagnostics.cause_codes, ["permission_denied"]);
        assert_eq!(diagnostics.chain_depth, 2);
        assert_eq!(diagnostics.chain_fingerprint.len(), 24);
        let json = serde_json::to_string(&diagnostics).unwrap();
        for secret in [
            "token.json",
            "credentials",
            "password",
            "sk-private",
            "example.test",
        ] {
            assert!(!json.contains(secret));
        }
        assert_eq!(evidence.recommendation.retry, "none");
    }

    #[tokio::test]
    async fn transport_error_after_post_keeps_typed_diagnostics_and_refuses_replay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let read = socket.read(&mut bytes).await.unwrap();
            assert!(bytes[..read].starts_with(b"POST "));
            // The POST may have executed; drop without returning HTTP headers.
        });
        let client = Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let request = client
            .post(format!("http://{address}/?api_key=must-not-leak"))
            .body("hello");
        let error = anyhow::Error::new(crate::upstream_transport::send(request).await.unwrap_err());
        peer.await.unwrap();
        let evidence = execution_evidence_for_error(&error);
        assert_eq!(evidence.error_code, "ambiguous_transport");
        assert!(evidence.upstream_attempted);
        assert_eq!(evidence.recommendation.retry, "none");
        assert_eq!(
            evidence.diagnostics.as_ref().unwrap().phase,
            "upstream_send"
        );
        let payload = serde_json::to_string(&supplier_execution_error_payload(&error)).unwrap();
        assert!(!payload.contains("must-not-leak"));
        assert!(!payload.contains("api_key"));
        assert!(payload.contains("chain_fingerprint"));
        assert!(payload.contains("automatic replay was refused"));
    }

    #[tokio::test]
    async fn connection_failure_is_diagnostic_evidence_not_a_provider_rejection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let error = client
            .post(format!("http://{address}/?api_key=private-value"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect());
        let error = anyhow::Error::new(crate::upstream_transport::UpstreamTransportError::Request(
            error,
        ));
        let evidence = execution_evidence_for_error(&error);
        assert_eq!(
            evidence.diagnostics.as_ref().unwrap().error_class,
            "connect"
        );
        assert!(!evidence.upstream_response_received);
        assert_eq!(evidence.recommendation.retry, "none");
        assert_eq!(evidence.fault_domain_hint, "transport");
    }

    #[test]
    fn response_conversion_failure_never_recommends_automatic_money_movement() {
        let error = response_conversion_failure(false, false, "bad response");
        let evidence = execution_evidence_for_error(&error);
        assert_eq!(evidence.stage, "response_protocol_convert");
        assert_eq!(evidence.recommendation.consumer_charge, "deny");
        assert_eq!(evidence.recommendation.supplier_settlement, "review");
        assert_eq!(evidence.recommendation.supplier_penalty, "review");
        assert!(evidence.upstream_response_received);
    }

    #[test]
    fn request_conversion_failure_can_try_another_channel_without_penalty() {
        let error = request_conversion_failure("tool arguments are incomplete");
        let evidence = execution_evidence_for_error(&error);
        assert_eq!(evidence.stage, "request_protocol_convert");
        assert_eq!(evidence.error_code, "conversion_failed");
        assert_eq!(evidence.fault_domain_hint, "platform");
        assert!(!evidence.upstream_attempted);
        assert!(!evidence.response_started);
        assert_eq!(evidence.recommendation.retry, "other_channel");
        assert_eq!(evidence.recommendation.supplier_penalty, "ineligible");
    }

    #[test]
    fn unsupported_surface_operation_can_try_another_channel_without_spending_an_attempt() {
        let error = crate::channel_executor::surface_operation_not_supported_error(
            "this channel does not implement the requested resource operation",
        );
        let evidence = execution_evidence_for_error(&error);
        assert_eq!(evidence.stage, "request_surface_resolve");
        assert_eq!(evidence.error_code, "surface_operation_not_supported");
        assert_eq!(evidence.fault_domain_hint, "capability");
        assert!(!evidence.upstream_attempted);
        assert!(!evidence.upstream_response_received);
        assert_eq!(evidence.recommendation.retry, "other_channel");
        assert_eq!(evidence.recommendation.supplier_penalty, "ineligible");
    }

    #[test]
    fn ambiguous_transport_denies_replay_and_money_movement() {
        let error = crate::channel_executor::ambiguous_transport_error("outcome unknown");
        let evidence = execution_evidence_for_error(&error);
        assert_eq!(evidence.error_code, "ambiguous_transport");
        assert_eq!(evidence.stage, "upstream_transport");
        assert_eq!(evidence.fault_domain_hint, "transport");
        assert!(evidence.upstream_attempted);
        assert!(!evidence.upstream_response_received);
        assert_eq!(evidence.recommendation.consumer_charge, "deny");
        assert_eq!(evidence.recommendation.supplier_settlement, "review");
        assert_eq!(evidence.recommendation.supplier_penalty, "ineligible");
        assert_eq!(evidence.recommendation.retry, "none");
    }
}
