use crate::config::*;
use crate::model::*;
use crate::*;
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

pub(crate) mod availability;
include!("supplier/register_update.rs");
include!("supplier/channel_probe.rs");
include!("supplier/lifecycle.rs");
include!("supplier/auto_recovery.rs");
include!("supplier/payload_encryption.rs");
include!("supplier/message_chunks.rs");
#[cfg(test)]
include!("supplier/transport_regression_tests.rs");
include!("supplier/transport.rs");
include!("supplier/platform_duplex.rs");
include!("supplier/execution_evidence.rs");
include!("supplier/upstream_usage.rs");
include!("supplier/inbound.rs");
include!("supplier/register_payload.rs");
include!("supplier/stream_batch.rs");
include!("supplier/forward.rs");
include!("supplier/dialect.rs");
include!("supplier/cache_identity.rs");
include!("supplier/codex_subscription_endpoint.rs");
include!("supplier/codex_http_continuation.rs");
include!("supplier/subscription.rs");
include!("supplier/quota.rs");
include!("supplier/safety.rs");
include!("supplier/usage_db.rs");
include!("supplier/usage_log.rs");
include!("supplier/quota_snapshot.rs");
include!("supplier/utils.rs");
