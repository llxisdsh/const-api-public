//! Process-local channel admission shared by local and supplier execution.
//! A slot follows the response body, not just the HTTP response headers.
use bytes::Bytes;
use http_body::Body;
use reqwest::{Response, ResponseBuilderExt};
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Mutex, OnceLock},
    task::{Context, Poll},
};

static CONCURRENCY: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();

fn counters() -> &'static Mutex<HashMap<String, u32>> {
    CONCURRENCY.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Default)]
pub(crate) struct Guard {
    channel_id: Option<String>,
}

impl Guard {
    pub(crate) fn acquire(channel_id: &str, limit: u32) -> Result<Self, ()> {
        let mut counters = counters().lock().map_err(|_| ())?;
        let value = counters.entry(channel_id.to_string()).or_default();
        if limit > 0 && *value >= limit {
            return Err(());
        }
        *value = value.saturating_add(1);
        Ok(Self {
            channel_id: Some(channel_id.to_string()),
        })
    }

    pub(crate) fn hold_response(self, response: Response) -> Response {
        if self.channel_id.is_none() {
            return response;
        }
        // Preserve native frames (including trailers), status, headers, URL
        // and extensions; admission must not buffer or transform the response.
        let mut builder = http::Response::builder().url(response.url().clone());
        let original: http::Response<reqwest::Body> = response.into();
        let (mut parts, inner) = original.into_parts();
        if let Some(extensions) = builder.extensions_mut() {
            parts.extensions.extend(std::mem::take(extensions));
        }
        let guard = if inner.is_end_stream() {
            None
        } else {
            Some(self)
        };
        let body = GuardedBody {
            inner: Box::pin(inner),
            guard,
        };
        http::Response::from_parts(parts, reqwest::Body::wrap(body)).into()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Some(channel_id) = &self.channel_id else {
            return;
        };
        if let Ok(mut counters) = counters().lock() {
            if let Some(value) = counters.get_mut(channel_id) {
                *value = value.saturating_sub(1);
                if *value == 0 {
                    counters.remove(channel_id);
                }
            }
        }
    }
}

pub(crate) fn current(channel_id: &str) -> u32 {
    counters()
        .lock()
        .ok()
        .and_then(|values| values.get(channel_id).copied())
        .unwrap_or(0)
}

struct GuardedBody {
    inner: Pin<Box<reqwest::Body>>,
    guard: Option<Guard>,
}

impl Body for GuardedBody {
    type Data = Bytes;
    type Error = reqwest::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        let result = self.inner.as_mut().poll_frame(cx);
        if matches!(result, Poll::Ready(None | Some(Err(_)))) || self.inner.is_end_stream() {
            self.guard.take();
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

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn admission_is_atomic_and_separate_for_each_channel() {
        let guard = Guard::acquire("capacity-atomic", 1).unwrap();
        assert_eq!(current("capacity-atomic"), 1);
        assert!(Guard::acquire("capacity-atomic", 1).is_err());
        let independent = Guard::acquire("capacity-other", 1).unwrap();
        let unlimited = Guard::acquire("capacity-atomic", 0).unwrap();
        assert_eq!(current("capacity-atomic"), 2);
        drop((guard, independent, unlimited));
        assert_eq!(current("capacity-atomic"), 0);
        assert!(Guard::acquire("capacity-atomic", 1).is_ok());
    }

    #[test]
    fn simultaneous_admission_never_exceeds_limit() {
        let start = std::sync::Barrier::new(17);
        let admitted = std::sync::Barrier::new(17);
        let release = std::sync::Barrier::new(17);
        let occupied = std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    start.wait();
                    let guard = Guard::acquire("capacity-race", 3);
                    admitted.wait();
                    release.wait();
                    drop(guard);
                });
            }
            start.wait();
            admitted.wait();
            let occupied = current("capacity-race");
            release.wait();
            occupied
        });
        assert_eq!(occupied, 3);
        assert_eq!(current("capacity-race"), 0);
    }

    #[tokio::test]
    async fn response_holds_slot_through_data_and_trailers_then_releases() {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-test-trailer", "kept".parse().unwrap());
        let frames = futures_util::stream::iter([
            Ok::<_, std::io::Error>(http_body::Frame::data(Bytes::from_static(
                b"original bytes",
            ))),
            Ok(http_body::Frame::trailers(trailers.clone())),
        ]);
        let mut response: Response = http::Response::builder()
            .status(201)
            .header("x-test-header", "kept")
            .url("https://example.test/original".parse().unwrap())
            .body(reqwest::Body::wrap(http_body_util::StreamBody::new(frames)))
            .unwrap()
            .into();
        response.extensions_mut().insert(123_u32);
        let response = Guard::acquire("capacity-body", 1)
            .unwrap()
            .hold_response(response);
        assert!(Guard::acquire("capacity-body", 1).is_err());
        assert_eq!(response.status(), 201);
        assert_eq!(response.headers()["x-test-header"], "kept");
        assert_eq!(response.url().as_str(), "https://example.test/original");
        assert_eq!(response.extensions().get::<u32>(), Some(&123));
        let response: http::Response<reqwest::Body> = response.into();
        let mut body = response.into_body();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "original bytes"
        );
        assert!(Guard::acquire("capacity-body", 1).is_err());
        assert_eq!(
            body.frame()
                .await
                .unwrap()
                .unwrap()
                .into_trailers()
                .unwrap(),
            trailers
        );
        assert!(body.frame().await.is_none());
        assert_eq!(current("capacity-body"), 0);
    }

    #[tokio::test]
    async fn error_or_cancel_releases_capacity() {
        let frames = futures_util::stream::iter([Err::<http_body::Frame<Bytes>, _>(
            std::io::Error::other("test stream failed"),
        )]);
        let response: Response =
            http::Response::new(reqwest::Body::wrap(http_body_util::StreamBody::new(frames)))
                .into();
        let response = Guard::acquire("capacity-error", 1)
            .unwrap()
            .hold_response(response);
        assert!(response.bytes().await.is_err());
        assert_eq!(current("capacity-error"), 0);

        let pending = futures_util::stream::pending::<Result<Bytes, std::io::Error>>();
        let response: Response = http::Response::new(reqwest::Body::wrap_stream(pending)).into();
        let response = Guard::acquire("capacity-cancel", 1)
            .unwrap()
            .hold_response(response);
        assert_eq!(current("capacity-cancel"), 1);
        drop(response);
        assert_eq!(current("capacity-cancel"), 0);
    }
}
