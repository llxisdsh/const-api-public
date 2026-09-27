use anyhow::{Result, anyhow};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::{fmt::Display, time::Duration};

pub(crate) const NON_STREAM_SSE_BUFFER_MAX_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const NON_STREAM_SSE_BUFFER_MAX_DURATION: Duration =
    Duration::from_secs(crate::protocol::stream::MAX_BUFFERED_STREAM_DURATION_SECS);

pub(crate) async fn read_sse_body_with_limits(resp: reqwest::Response) -> Result<String> {
    read_sse_stream_with_limits(
        resp.bytes_stream(),
        NON_STREAM_SSE_BUFFER_MAX_BYTES,
        NON_STREAM_SSE_BUFFER_MAX_DURATION,
    )
    .await
}

pub(crate) async fn read_sse_stream_with_limits<S, E>(
    stream: S,
    max_bytes: usize,
    max_duration: Duration,
) -> Result<String>
where
    S: Stream<Item = std::result::Result<Bytes, E>>,
    E: Display,
{
    let deadline = tokio::time::Instant::now() + max_duration;
    let mut stream = Box::pin(stream);
    let mut raw = Vec::new();
    let mut bytes_read = 0usize;

    loop {
        let next = tokio::time::timeout_at(deadline, stream.next())
            .await
            .map_err(|_| {
                anyhow!(
                    "SSE buffer exceeded max duration of {}ms",
                    max_duration.as_millis()
                )
            })?;
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|err| anyhow!("SSE buffer read failed: {err}"))?;
        bytes_read = bytes_read
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow!("SSE buffer exceeded max bytes ({max_bytes})"))?;
        if bytes_read > max_bytes {
            return Err(anyhow!(
                "SSE buffer exceeded max bytes ({bytes_read} > {max_bytes})"
            ));
        }
        raw.extend_from_slice(&chunk);
    }

    String::from_utf8(raw).map_err(|error| anyhow!("SSE buffer is not valid UTF-8: {error}"))
}
