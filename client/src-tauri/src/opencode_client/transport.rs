//! The Console adapter selects this transport only for canonical free inference.
use bytes::Bytes;
use reqwest::{Method, Response, Url, header::HeaderMap};

pub(crate) async fn send(
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Bytes,
    use_system_proxy: bool,
) -> Result<Response, crate::upstream_transport::UpstreamTransportError> {
    static TRANSPORT: crate::bun_transport::Transport = crate::bun_transport::Transport::new();
    TRANSPORT
        .send(method, url, headers, body, use_system_proxy)
        .await
}
