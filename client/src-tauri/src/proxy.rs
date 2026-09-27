use crate::config::*;
use crate::model::*;
use crate::supplier::*;
use crate::*;
use anyhow::{Result, anyhow};
use base64::Engine as _;
use futures_util::{SinkExt, Stream, StreamExt, TryStreamExt, stream::BoxStream};
use http_body_util::BodyExt;
use reqwest::Client;
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    fs,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use warp::{Filter, Reply, http::StatusCode};

pub(crate) struct ThreadSafeStream<S> {
    inner: std::sync::Mutex<std::pin::Pin<Box<S>>>,
}

impl<S> ThreadSafeStream<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            inner: std::sync::Mutex::new(Box::pin(stream)),
        }
    }
}

impl<S> Stream for ThreadSafeStream<S>
where
    S: Stream + Send,
{
    type Item = S::Item;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut stream = self
            .as_ref()
            .get_ref()
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stream.as_mut().poll_next(context)
    }
}

include!("proxy/resource_owner.rs");
include!("proxy/gemini_upload_session.rs");
include!("proxy/runtime.rs");
include!("proxy/model_health.rs");
include!("proxy/websocket.rs");
include!("proxy/websocket_failure.rs");
include!("proxy/native_platform.rs");
include!("proxy/platform_voice_call.rs");
include!("proxy/supplier_voice_call.rs");
include!("proxy/responses_ws_pool.rs");
include!("proxy/streaming_upload.rs");
include!("proxy/request_conversion.rs");
include!("proxy/sse.rs");
include!("proxy/response_conversion.rs");
include!("proxy/http.rs");
include!("proxy/tests.rs");
