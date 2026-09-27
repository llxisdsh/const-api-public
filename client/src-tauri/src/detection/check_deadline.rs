//! Interactive checks have one deadline, but commit evidence after each step.
//! Discovery-only refreshes never enter this scope.
use super::*;
use std::future::Future;

tokio::task_local! {
    static DEADLINE: tokio::time::Instant;
}

#[derive(Debug)]
struct DeadlineReached;

impl std::fmt::Display for DeadlineReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("capability check deadline reached; completed evidence retained")
    }
}
impl std::error::Error for DeadlineReached {}

pub(super) fn scope<F: Future>(duration: Duration, future: F) -> impl Future<Output = F::Output> {
    let deadline = tokio::time::Instant::now() + duration;
    let deadline = DEADLINE
        .try_with(|parent| deadline.min(*parent))
        .unwrap_or(deadline);
    // Detection composes many protocol futures. Keep the scope on the heap so
    // Windows worker/test stacks do not scale with the entire probe tree.
    DEADLINE.scope(deadline, Box::pin(future))
}

pub(super) fn current() -> Option<tokio::time::Instant> {
    DEADLINE.try_with(|deadline| *deadline).ok()
}

pub(super) fn observe<F: Future>(future: F) -> impl Future<Output = Result<F::Output>> {
    let future = Box::pin(future);
    async move {
        match DEADLINE.try_with(|deadline| *deadline) {
            Ok(deadline) => {
                // timeout_at may poll an immediately-ready future at an expired
                // deadline. Explicitly avoid starting another request in that case.
                if tokio::time::Instant::now() >= deadline {
                    return Err(DeadlineReached.into());
                }
                tokio::time::timeout_at(deadline, future)
                    .await
                    .map_err(|_| DeadlineReached.into())
            }
            Err(_) => Ok(future.await),
        }
    }
}

pub(super) fn run<T>(future: impl Future<Output = Result<T>>) -> impl Future<Output = Result<T>> {
    let future = observe(future);
    async move { future.await? }
}

pub(super) fn step<T>(
    duration: Duration,
    future: impl Future<Output = Result<T>>,
) -> impl Future<Output = Result<T>> {
    let future = Box::pin(future);
    run(async move { tokio::time::timeout(duration, future).await? })
}

pub(super) fn incomplete(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<DeadlineReached>()
            || cause.is::<tokio::time::error::Elapsed>()
            || cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|error| error.is_timeout() || error.is_connect())
            || matches!(
                cause.downcast_ref::<crate::upstream_transport::UpstreamTransportError>(),
                Some(crate::upstream_transport::UpstreamTransportError::ProbeBudgetExceeded)
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn expired_parent_never_starts_another_step_or_gets_extended() {
        scope(Duration::from_millis(1), async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let mut started = false;
            let result = scope(
                Duration::from_secs(120),
                observe(async {
                    started = true;
                }),
            )
            .await;
            assert!(result.is_err());
            assert!(!started);
        })
        .await;
    }

    #[tokio::test]
    async fn expired_clock_does_not_reclassify_a_decisive_error_or_error_text() {
        scope(Duration::ZERO, async {
            assert!(!incomplete(&anyhow!("unsupported protocol")));
            assert!(
                !incomplete(&anyhow!(
                    "probe attempt budget exhausted before sending upstream"
                )),
                "untrusted response text must not impersonate a typed local failure"
            );
            let error = run(async { Ok(()) }).await.unwrap_err();
            assert!(incomplete(&error.context("wrapped cause")));
        })
        .await;
    }
}
