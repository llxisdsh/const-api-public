use anyhow::Result;
use std::{error::Error, fmt, future::Future, sync::Arc, time::Instant};

#[derive(Default)]
pub(crate) struct RefreshState {
    last_failure: Option<(Instant, SharedRefreshError)>,
}

impl RefreshState {
    pub(crate) async fn run<T>(
        &mut self,
        started_at: Instant,
        refresh: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        // Only callers already waiting share the failure. Later calls retry
        // normally; an outage must not become a sticky credential failure.
        if let Some((completed_at, error)) = &self.last_failure
            && *completed_at >= started_at
        {
            return Err(error.clone().into());
        }
        match refresh.await {
            Ok(value) => {
                self.last_failure = None;
                Ok(value)
            }
            Err(error) => {
                if let Some(request_error) = error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
                {
                    crate::http_client::observe_error(request_error);
                }
                let error = SharedRefreshError(Arc::new(error));
                self.last_failure = Some((Instant::now(), error.clone()));
                Err(error.into())
            }
        }
    }
}

// Preserve the complete typed cause chain, including OAuth status and network
// errors. Copying only the message would change routing/recovery decisions.
#[derive(Clone, Debug)]
struct SharedRefreshError(Arc<anyhow::Error>);

impl fmt::Display for SharedRefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl Error for SharedRefreshError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription_oauth_error::{SubscriptionOAuthError, oauth_wire_error_from_anyhow};

    #[tokio::test]
    async fn shared_failure_preserves_oauth_classification_without_sticky_cooldown() {
        for (status, code) in [
            (503, "unavailable"),
            (429, "rate_limited"),
            (400, "invalid_grant"),
        ] {
            let mut state = RefreshState::default();
            let started_at = Instant::now();
            let error = SubscriptionOAuthError::from_response(
                "openai",
                status,
                &serde_json::json!({"error": code}),
            );
            let expected = error.wire_error();
            let first = state
                .run::<()>(started_at, async { Err(error.into()) })
                .await
                .unwrap_err();
            let waiting = state
                .run::<()>(started_at, async { panic!("waiting caller retried") })
                .await
                .unwrap_err();
            assert_eq!(oauth_wire_error_from_anyhow(&first), Some(expected.clone()));
            assert_eq!(oauth_wire_error_from_anyhow(&waiting), Some(expected));
            state.run(Instant::now(), async { Ok(()) }).await.unwrap();
            assert!(state.last_failure.is_none());
        }
    }

    #[tokio::test]
    async fn cancelled_refresh_does_not_block_later_attempts() {
        let state = tokio::sync::Mutex::new(RefreshState::default());
        let cancelled = tokio::time::timeout(std::time::Duration::from_millis(10), async {
            state
                .lock()
                .await
                .run::<()>(Instant::now(), std::future::pending())
                .await
        })
        .await;
        assert!(cancelled.is_err());
        state
            .lock()
            .await
            .run(Instant::now(), async { Ok(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shared_timeout_keeps_the_network_cause() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stalled = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut state = RefreshState::default();
        let started_at = Instant::now();
        let first = state
            .run(started_at, async {
                Ok(client
                    .get(format!("http://{address}/token"))
                    .timeout(std::time::Duration::from_millis(50))
                    .send()
                    .await?)
            })
            .await
            .unwrap_err();
        let waiting = state
            .run::<()>(started_at, async { panic!("waiting caller retried") })
            .await
            .unwrap_err();
        for error in [first, waiting] {
            let cause = error
                .chain()
                .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
                .unwrap();
            assert!(cause.is_timeout());
            assert!(oauth_wire_error_from_anyhow(&error).is_none());
        }
        stalled.abort();
    }
}
