use reqwest::{Client, Method, RequestBuilder, Response, Version};
use std::{
    fmt,
    future::Future,
    sync::{Arc, Mutex as StdMutex},
};

tokio::task_local! {
    static HTTP_VERSION_OBSERVER: Arc<StdMutex<Option<Version>>>;
    static PROBE_BUDGET: Vec<Arc<std::sync::atomic::AtomicUsize>>;
}

#[derive(Debug)]
pub(crate) enum UpstreamTransportError {
    ProbeBudgetExceeded,
    Request(reqwest::Error),
    Bun(wreq::Error),
    AmbiguousRequest(reqwest::Error),
}

impl UpstreamTransportError {
    pub(crate) fn is_timeout(&self) -> bool {
        match self {
            Self::ProbeBudgetExceeded => false,
            Self::Bun(error) => error.is_timeout(),
            Self::Request(error) | Self::AmbiguousRequest(error) => error.is_timeout(),
        }
    }

    pub(crate) fn is_ambiguous(&self) -> bool {
        matches!(self, Self::AmbiguousRequest(_))
            || matches!(self, Self::Bun(error) if !error.is_builder() && !error.is_connect())
    }
}

impl fmt::Display for UpstreamTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProbeBudgetExceeded => {
                formatter.write_str("probe attempt budget exhausted before sending upstream")
            }
            Self::Request(error) => error.fmt(formatter),
            Self::Bun(error) => error.fmt(formatter),
            Self::AmbiguousRequest(error) => write!(
                formatter,
                "upstream request outcome is unknown; automatic replay was refused: {error}"
            ),
        }
    }
}

impl std::error::Error for UpstreamTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ProbeBudgetExceeded => None,
            Self::Bun(error) => Some(error),
            Self::Request(error) | Self::AmbiguousRequest(error) => Some(error),
        }
    }
}

pub(crate) fn with_probe_budget<F: Future>(
    limit: usize,
    future: F,
) -> impl Future<Output = (F::Output, usize)> {
    // Keep composed protocol probes off the stack before adding a task-local
    // wrapper. Read the parent budget when polled, as before.
    let future = Box::pin(future);
    async move {
        let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(limit));
        let mut budgets = PROBE_BUDGET.try_with(Clone::clone).unwrap_or_default();
        budgets.push(remaining.clone());
        let output = PROBE_BUDGET.scope(budgets, future).await;
        (
            output,
            limit.saturating_sub(remaining.load(std::sync::atomic::Ordering::Relaxed)),
        )
    }
}

pub(crate) fn probe_budget_active() -> bool {
    PROBE_BUDGET
        .try_with(|budgets| !budgets.is_empty())
        .unwrap_or(false)
}

fn claim_probe_attempt(request: &reqwest::Request) -> Result<(), UpstreamTransportError> {
    claim_http_probe_attempt(request.method(), request.url(), request.headers())
}

pub(crate) fn claim_http_probe_attempt(
    method: &Method,
    url: &reqwest::Url,
    headers: &reqwest::header::HeaderMap,
) -> Result<(), UpstreamTransportError> {
    if !probe_budget_active() {
        return Ok(());
    }
    let path = url.path().to_ascii_lowercase();
    let inference = method == Method::POST
        && [
            "/responses",
            "/messages",
            "/chat/completions",
            "generatecontent",
        ]
        .iter()
        .any(|suffix| path.contains(suffix));
    let inference = inference
        || method == Method::POST
            && headers
                .get("x-amz-target")
                .is_some_and(|target| target == "KiroRuntimeService.GenerateAssistantResponse");
    if !inference {
        return Ok(());
    }
    claim_inference_probe_attempt()
}

/// For proprietary transports that do not use the adaptive HTTP sender.
pub(crate) fn claim_inference_probe_attempt() -> Result<(), UpstreamTransportError> {
    PROBE_BUDGET
        .try_with(|budgets| {
            use std::sync::atomic::Ordering::Relaxed;
            for (index, remaining) in budgets.iter().enumerate() {
                if remaining
                    .try_update(Relaxed, Relaxed, |v| v.checked_sub(1))
                    .is_err()
                {
                    for reserved in &budgets[..index] {
                        reserved.fetch_add(1, Relaxed);
                    }
                    return Err(UpstreamTransportError::ProbeBudgetExceeded);
                }
            }
            Ok(())
        })
        .unwrap_or(Ok(()))
}

#[cfg(test)]
mod probe_budget_tests {
    use super::*;
    #[tokio::test]
    async fn proprietary_runtime_is_counted_but_root_metadata_posts_are_free() {
        let client = Client::new();
        let request = |target| {
            client
                .post("http://127.0.0.1:9/")
                .header("x-amz-target", target)
                .build()
                .unwrap()
        };
        let (_, attempts) = with_probe_budget(2, async {
            claim_probe_attempt(&request(
                "KiroControlPlaneBearerService.ListAvailableModels",
            ))
            .unwrap();
            claim_probe_attempt(&request("AmazonCodeWhispererService.GetUsageLimits")).unwrap();
            claim_probe_attempt(&request("KiroRuntimeService.GenerateAssistantResponse")).unwrap();
            claim_inference_probe_attempt().unwrap();
            assert!(claim_inference_probe_attempt().is_err());
            assert!(
                claim_probe_attempt(&request("KiroRuntimeService.GenerateAssistantResponse"))
                    .is_err()
            );
        })
        .await;
        assert_eq!(attempts, 2);
    }
    #[tokio::test]
    async fn metadata_is_free_and_nested_attempts_share_one_budget() {
        let client = Client::new();
        let catalog = client.get("http://127.0.0.1:9/models").build().unwrap();
        let auth = client
            .post("http://127.0.0.1:9/oauth/token")
            .build()
            .unwrap();
        let infer = client
            .post("http://127.0.0.1:9/v1/responses")
            .build()
            .unwrap();
        let (_, used) = with_probe_budget(2, async {
            claim_probe_attempt(&catalog).unwrap();
            claim_probe_attempt(&auth).unwrap();
            claim_probe_attempt(&infer).unwrap();
            claim_probe_attempt(&infer).unwrap();
            assert!(matches!(
                claim_probe_attempt(&infer),
                Err(UpstreamTransportError::ProbeBudgetExceeded)
            ));
        })
        .await;
        assert_eq!(used, 2);
        claim_probe_attempt(&infer).unwrap();
        let (_, used) = with_probe_budget(1, async {
            let (_, nested_used) = with_probe_budget(8, async {
                claim_probe_attempt(&infer).unwrap();
                assert!(claim_probe_attempt(&infer).is_err());
            })
            .await;
            assert_eq!(nested_used, 1);
        })
        .await;
        assert_eq!(used, 1);
    }
}

mod http3;
pub(crate) use http3::send_for_channel;

/// The adapter owns the client's TLS/HTTP profile. This default entry never
/// probes or upgrades. Only send_for_channel consults verified HTTP3 origins.
/// CONST platform HTTP3/QUIC is independently managed by platform_transport.
pub(crate) async fn send(request: RequestBuilder) -> Result<Response, UpstreamTransportError> {
    let (client, request) = request.build_split();
    let request = request.map_err(UpstreamTransportError::Request)?;
    claim_probe_attempt(&request)?;
    execute_observed(&client, request).await
}

pub(crate) fn observe_http_version<F>(
    future: F,
) -> impl Future<Output = (F::Output, Option<Version>)>
where
    F: Future,
{
    // Subscription probes pass the whole executor future through this observer.
    let future = Box::pin(future);
    async move {
        let observed = Arc::new(StdMutex::new(None));
        let output = HTTP_VERSION_OBSERVER.scope(observed.clone(), future).await;
        let version = observed.lock().ok().and_then(|value| *value);
        (output, version)
    }
}

pub(crate) fn http_version_transport(version: Version) -> &'static str {
    match version {
        Version::HTTP_3 => "http3",
        Version::HTTP_2 => "http2",
        Version::HTTP_11 | Version::HTTP_10 | Version::HTTP_09 => "http1",
        _ => "http",
    }
}

pub(crate) fn record_http_version(version: Version) {
    let _ = HTTP_VERSION_OBSERVER.try_with(|observed| {
        if let Ok(mut value) = observed.lock() {
            *value = Some(version);
        }
    });
}

async fn execute_observed(
    client: &Client,
    request: reqwest::Request,
) -> Result<Response, UpstreamTransportError> {
    let safe_method = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let response = client.execute(request).await.map_err(|error| {
        crate::http_client::observe_error(&error);
        // A failed POST may already have reached the upstream. Keep this
        // evidence for adapter fallback loops even without HTTP3 negotiation.
        if !safe_method && !error.is_builder() && !error.is_connect() {
            UpstreamTransportError::AmbiguousRequest(error.without_url())
        } else {
            UpstreamTransportError::Request(error.without_url())
        }
    })?;
    crate::http_client::observe_response(&response);
    record_http_version(response.version());
    Ok(response)
}

pub(crate) trait UpstreamRequestBuilderExt {
    fn send_upstream(self) -> impl Future<Output = Result<Response, UpstreamTransportError>>;
}

impl UpstreamRequestBuilderExt for RequestBuilder {
    fn send_upstream(self) -> impl Future<Output = Result<Response, UpstreamTransportError>> {
        send(self)
    }
}

pub(crate) fn is_ambiguous_transport_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<UpstreamTransportError>()
            .is_some_and(UpstreamTransportError::is_ambiguous)
    }) || crate::channel_executor::channel_execution_error_code(error)
        == Some("ambiguous_transport")
}

#[cfg(test)]
mod tests;
