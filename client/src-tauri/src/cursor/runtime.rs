//! One HTTP/2 stream per agent run. MCP continuations retain that stream for a
//! bounded interval, scoped to channel/account/caller and the declared tool set.
//! No upstream exec request is ever executed on the supplier's computer.
use super::{
    request::{self, Prepared, Tool},
    wire::{self as w, ValueRef},
    *,
};
use crate::protocol::ir::{
    BlockHeader, CanonicalStreamEvent as Event, ContentBlockKind, FinishDetail, FinishReason,
    ResponseMeta, ResponseStatus, ToolKind, Usage, UsageValue,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::{Semaphore, mpsc, oneshot};

type Output = mpsc::Sender<Result<Event>>;
struct Resume {
    result: Vec<u8>,
    output: Output,
}
struct Waiting {
    scope: String,
    resume: oneshot::Sender<Resume>,
    expires: Instant,
}
type Pending = Mutex<HashMap<String, Waiting>>;
static PENDING: OnceLock<Pending> = OnceLock::new();
static STREAM_LIMIT: OnceLock<Arc<Semaphore>> = OnceLock::new();
const WAIT: Duration = Duration::from_secs(5 * 60);
const IDLE: Duration = Duration::from_secs(100);

pub(crate) async fn start(
    client: &Client,
    token: &str,
    prepared: &Prepared,
    override_url: Option<&str>,
) -> Result<mpsc::Receiver<Result<Event>>> {
    let (output, receiver) = mpsc::channel(16);
    // Replacing the credential at the same channel path must not resume the
    // previous account's agent. JWT sub remains stable across token refreshes.
    let credential = super::normalize_token(json!({"access_token":token}), None)?;
    let account = credential["account_id"].as_str().unwrap_or(token);
    let scope = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(serde_json::to_vec(&(
            &prepared.scope,
            account,
        ))?))
    };
    if prepared.results.len() == 1 {
        let (call_id, result) = &prepared.results[0];
        let waiting = {
            let mut pending = PENDING
                .get_or_init(Default::default)
                .lock()
                .map_err(|_| anyhow!("Cursor continuation registry poisoned"))?;
            pending.retain(|_, p| p.expires > Instant::now());
            if pending.get(call_id).is_some_and(|p| p.scope == scope) {
                pending.remove(call_id)
            } else {
                None
            }
        };
        if let Some(waiting) = waiting {
            if waiting
                .resume
                .send(Resume {
                    result: result.clone(),
                    output: output.clone(),
                })
                .is_ok()
            {
                return Ok(receiver);
            }
            // The previous transport ended before receiving the result. No
            // request was sent; reconstruct the supplied native history below.
        }
    }
    let permit = STREAM_LIMIT
        .get_or_init(|| Arc::new(Semaphore::new(64)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError {
            status: 429,
            code: "rate_limited".into(),
            detail:
                "Local Cursor connection capacity is full; finish pending tool calls or retry later"
                    .into(),
        })?;
    let url = match override_url {
        Some(url) => url.to_string(),
        None => super::endpoint::agent_url(client, token).await?,
    };
    let (tx, rx) = mpsc::channel::<Vec<u8>>(16);
    let upload = futures_util::stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|b| (Ok::<_, std::io::Error>(b), rx))
    });
    tx.send(w::frame(&prepared.run)?)
        .await
        .context("Cursor request body closed")?;
    let request = super::headers(client.post(url), token)
        .version(reqwest::Version::HTTP_2)
        .header("content-type", "application/connect+proto")
        .header("connect-accept-encoding", "gzip")
        .body(reqwest::Body::wrap_stream(upload));
    crate::upstream_transport::claim_inference_probe_attempt()?;
    let response = tokio::time::timeout(Duration::from_secs(30), super::send(request))
        .await
        .context("Cursor response headers timed out")??;
    if !response.status().is_success() {
        return Err(super::rejection(response, &[token]).await?.into());
    }
    ensure!(
        response.version() == reqwest::Version::HTTP_2,
        "Cursor requires a full-duplex HTTP/2 connection"
    );
    let mut actor = Actor {
        tx,
        output: Some(output.clone()),
        model: prepared.model.clone(),
        tools: prepared.tools.clone(),
        scope,
        blobs: Blobs::new(prepared.blobs.clone())?,
        index: 0,
        block: None,
        current_call: None,
        waiting: None,
        waiting_since: None,
        queued: VecDeque::new(),
        queued_bytes: 0,
        split_turn: false,
    };
    tokio::spawn(async move {
        let _permit = permit;
        let result = actor.run(response).await;
        if let Err(error) = result
            && let Some(output) = actor.output.take()
        {
            let _ = output.send(Err(error)).await;
        }
        if let Some(id) = actor.current_call.take()
            && let Ok(mut pending) = PENDING.get_or_init(Default::default).lock()
        {
            pending.remove(&id);
        }
        // Dropping response and upload sender resets only this H2 stream; the
        // shared reqwest pool remains available to unrelated requests.
    });
    Ok(receiver)
}

struct Actor {
    tx: mpsc::Sender<Vec<u8>>,
    output: Option<Output>,
    model: String,
    tools: Vec<Tool>,
    scope: String,
    blobs: Blobs,
    index: u32,
    block: Option<(u32, bool)>,
    current_call: Option<String>,
    waiting: Option<(oneshot::Receiver<Resume>, u64, String)>,
    waiting_since: Option<Instant>,
    queued: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    // One AgentService turn can span several public tool-call requests. Its
    // final counters cover the whole turn, not only the last HTTP response.
    split_turn: bool,
}

impl Actor {
    async fn emit(&self, event: Event) -> Result<()> {
        self.output
            .as_ref()
            .expect("active Cursor segment")
            .send(Ok(event))
            .await
            .context("Cursor downstream cancelled")
    }
    async fn write(&self, message: Vec<u8>) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30), self.tx.send(w::frame(&message)?))
            .await
            .context("Cursor upload stalled")?
            .context("Cursor upload closed")
    }
    async fn begin(&mut self) -> Result<()> {
        self.index = 0;
        self.block = None;
        self.emit(Event::ResponseStart(ResponseMeta {
            id: Some(format!("resp_{}", uuid::Uuid::new_v4().simple())),
            model: Some(self.model.clone()),
            status: ResponseStatus::InProgress,
            provider_event_id: None,
        }))
        .await
    }
    async fn close_block(&mut self) -> Result<()> {
        if let Some((index, _)) = self.block.take() {
            self.emit(Event::BlockDone { index, block: None }).await?;
        }
        Ok(())
    }
    async fn done(&mut self, reason: FinishReason) -> Result<()> {
        self.close_block().await?;
        self.emit(Event::ResponseDone(FinishDetail {
            reason,
            original_reason: None,
            incomplete_details: None,
        }))
        .await
    }
    async fn text(&mut self, text: String, thinking: bool) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if self.block.is_some_and(|(_, old)| old != thinking) {
            self.close_block().await?;
        }
        let index = if let Some((index, _)) = self.block {
            index
        } else {
            let index = self.index;
            self.index += 1;
            self.emit(Event::BlockStart {
                index,
                block: BlockHeader {
                    kind: if thinking {
                        ContentBlockKind::Reasoning
                    } else {
                        ContentBlockKind::Text
                    },
                    source_item_id: None,
                    call_id: None,
                    name: None,
                    tool_kind: None,
                    tool_namespace: None,
                    artifacts: vec![],
                },
            })
            .await?;
            self.block = Some((index, thinking));
            index
        };
        self.emit(if thinking {
            Event::ReasoningDelta { index, text }
        } else {
            Event::TextDelta { index, text }
        })
        .await
    }

    async fn run(&mut self, response: Response) -> Result<()> {
        let mut stream = response.bytes_stream();
        let mut frames = w::Frames::default();
        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let mut last_received = Instant::now();
        let mut received = 0_usize;
        self.begin().await?;
        loop {
            if self.waiting.is_none()
                && let Some(payload) = self.queued.pop_front()
            {
                self.queued_bytes -= payload.len();
                if self.process(&payload).await? {
                    return Ok(());
                }
                continue;
            }
            let active = self.output.clone();
            tokio::select! {
                biased;
                _ = async { match &active { Some(out) => out.closed().await, None => std::future::pending().await } } => anyhow::bail!("Cursor downstream cancelled"),
                resume = async { match &mut self.waiting { Some((rx, _, _)) => rx.await, None => std::future::pending().await } } => {
                    let resume = resume.context("Cursor continuation expired or cancelled")?;
                    let (_, id, exec) = self.waiting.take().expect("pending Cursor continuation");
                    self.current_call = None;
                    self.waiting_since = None;
                    self.output = Some(resume.output);
                    last_received = Instant::now();
                    self.begin().await?;
                    self.write(exec_result(id, &exec, 11, resume.result)).await?;
                }
                _ = heartbeat.tick() => {
                    ensure!(self.waiting.is_some() || last_received.elapsed() < IDLE, "Cursor stream inactivity deadline exceeded");
                    ensure!(self.waiting_since.is_none_or(|at| at.elapsed() < WAIT), "Cursor tool result wait expired");
                    self.write(w::bytes(7, [])).await?;
                }
                chunk = stream.next() => {
                    let Some(chunk) = chunk else {
                        frames.finish()?;
                        anyhow::bail!("Cursor stream ended without turn completion");
                    };
                    let chunk = chunk.map_err(|e| { crate::http_client::observe_error(&e); anyhow!(e.without_url()) })?;
                    last_received = Instant::now();
                    received += chunk.len();
                    ensure!(received <= crate::protocol::stream::MAX_STREAM_BYTES, "Cursor stream exceeded size limit");
                    for (ended, payload) in frames.push(&chunk)? {
                        if ended {
                            let end: Value = serde_json::from_slice(&payload).context("invalid Cursor Connect termination")?;
                            if let Some(error) = end.get("error") {
                                return Err(connect_error(error).into());
                            }
                            anyhow::bail!("Cursor Connect stream ended before turn completion");
                        }
                        if self.process(&payload).await? { return Ok(()); }
                    }
                }
            }
        }
    }

    async fn process(&mut self, payload: &[u8]) -> Result<bool> {
        for (number, value) in w::fields(payload)? {
            let ValueRef::Bytes(body) = value else {
                continue;
            };
            if number == 4 {
                self.kv(body).await?;
                continue;
            }
            if number == 3 {
                continue;
            } // Checkpoints are not shared across callers.
            if number == 2 {
                let fields = w::fields(body)?;
                let id = w::integer(&fields, 1).unwrap_or(0);
                let exec = w::string(&fields, 15)?;
                let variant = fields
                    .iter()
                    .find(|(n, v)| matches!(v, ValueRef::Bytes(_)) && !matches!(n, 15 | 19 | 57));
                let Some((kind, ValueRef::Bytes(args))) = variant else {
                    anyhow::bail!("Cursor exec message has no operation");
                };
                match *kind {
                    10 => {
                        self.write(exec_result(
                            id,
                            &exec,
                            10,
                            w::message(1, &[w::bytes(1, request::context(&self.tools))]),
                        ))
                        .await?
                    }
                    36 => {
                        self.write(exec_result(id, &exec, 36, request::mcp_state(&self.tools)))
                            .await?
                    }
                    17 => {
                        self.write(exec_result(id, &exec, 17, w::bytes(1, [])))
                            .await?
                    }
                    11 if self.waiting.is_none() => self.tool(id, &exec, args).await?,
                    11 => self.queue(w::bytes(number, body))?,
                    _ => self.write(reject_exec(id, &exec, *kind, args)?).await?,
                }
            } else if number == 1 {
                for (kind, value) in w::fields(body)? {
                    let ValueRef::Bytes(update) = value else {
                        continue;
                    };
                    if self.waiting.is_some() && matches!(kind, 1 | 4 | 5 | 14) {
                        self.queue(w::message(1, &[w::bytes(kind, update)]))?;
                        continue;
                    }
                    match kind {
                        1 | 4 => {
                            self.text(w::string(&w::fields(update)?, 1)?, kind == 4)
                                .await?
                        }
                        5 => self.close_block().await?,
                        14 => {
                            let usage = turn_usage(update)?;
                            // Earlier tool-call segments have already been metered.
                            // Never bill cumulative turn usage again on continuation.
                            if !self.split_turn {
                                self.emit(Event::UsageUpdate(Box::new(usage))).await?;
                            }
                            self.done(FinishReason::Stop).await?;
                            return Ok(true);
                        }
                        _ => {} // Optional progress/telemetry, not a completion signal.
                    }
                }
            } else if number == 7 {
                anyhow::bail!(
                    "capability_mismatch: Cursor requested an unsupported interactive approval"
                );
            }
        }
        Ok(false)
    }
    fn queue(&mut self, payload: Vec<u8>) -> Result<()> {
        ensure!(
            self.queued_bytes + payload.len() <= 1024 * 1024,
            "Cursor pending continuation queue exceeded limit"
        );
        self.queued_bytes += payload.len();
        self.queued.push_back(payload);
        Ok(())
    }
    async fn kv(&mut self, body: &[u8]) -> Result<()> {
        let fields = w::fields(body)?;
        let mut reply = vec![w::int(1, w::integer(&fields, 1).unwrap_or(0))];
        if let Some(meta) = w::data(&fields, 4) {
            reply.push(w::bytes(4, meta));
        }
        if let Some(get) = w::data(&fields, 2) {
            let get = w::fields(get)?;
            let key =
                w::data(&get, 1).ok_or_else(|| anyhow!("Cursor blob request is missing ID"))?;
            reply.push(w::message(
                2,
                &self
                    .blobs
                    .data
                    .get(key)
                    .map(|v| vec![w::bytes(1, v)])
                    .unwrap_or_default(),
            ));
        } else if let Some(set) = w::data(&fields, 3) {
            let set = w::fields(set)?;
            let key =
                w::data(&set, 1).ok_or_else(|| anyhow!("Cursor blob update is missing ID"))?;
            self.blobs
                .put(key.to_vec(), w::data(&set, 2).unwrap_or_default().to_vec())?;
            reply.push(w::bytes(3, []));
        } else {
            anyhow::bail!("unsupported Cursor blob operation");
        }
        self.write(w::message(3, &reply)).await
    }
    async fn tool(&mut self, id: u64, exec: &str, raw: &[u8]) -> Result<()> {
        let fields = w::fields(raw)?;
        let mut name = w::string(&fields, 5)?;
        if name.is_empty() {
            name = w::string(&fields, 1)?;
        }
        if !self.tools.iter().any(|t| t.name == name) {
            return self
                .write(exec_result(
                    id,
                    exec,
                    11,
                    w::message(
                        2,
                        &[w::bytes(
                            1,
                            "Tool is not declared. Use the provided CONST MCP tools.",
                        )],
                    ),
                ))
                .await;
        }
        let mut args = serde_json::Map::new();
        for (number, value) in &fields {
            if let (2, ValueRef::Bytes(entry)) = (number, value) {
                let entry = w::fields(entry)?;
                args.insert(
                    w::string(&entry, 1)?,
                    w::decode_json(w::data(&entry, 2).unwrap_or_default(), 0)?,
                );
            }
        }
        let call_id = format!("call_cursor_{}", uuid::Uuid::new_v4().simple());
        let (resume, rx) = oneshot::channel();
        PENDING
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow!("Cursor continuation registry poisoned"))?
            .insert(
                call_id.clone(),
                Waiting {
                    scope: self.scope.clone(),
                    resume,
                    expires: Instant::now() + WAIT,
                },
            );
        self.current_call = Some(call_id.clone());
        self.waiting = Some((rx, id, exec.into()));
        self.waiting_since = Some(Instant::now());
        self.close_block().await?;
        let index = self.index;
        self.index += 1;
        self.emit(Event::BlockStart {
            index,
            block: BlockHeader {
                kind: ContentBlockKind::ToolCall,
                source_item_id: None,
                call_id: Some(call_id),
                name: Some(name),
                tool_kind: Some(ToolKind::Function),
                tool_namespace: None,
                artifacts: vec![],
            },
        })
        .await?;
        self.emit(Event::ToolArgumentsDelta {
            index,
            data: Value::Object(args).to_string(),
        })
        .await?;
        self.emit(Event::BlockDone { index, block: None }).await?;
        self.split_turn = true;
        self.done(FinishReason::ToolCalls).await?;
        self.output = None;
        Ok(())
    }
}

// Cursor AgentService TurnEndedUpdate (optional int64 counters). Input includes
// cache reads/writes; reasoning is included in output. Verified against the
// same runs' DashboardService usage events, whose input excludes both caches.
fn turn_usage(raw: &[u8]) -> Result<Usage> {
    let fields = w::fields(raw)?;
    let value = |field| -> Result<UsageValue> {
        w::integer(&fields, field)
            .map(|n| {
                let n = i64::try_from(n).context("invalid Cursor token counter")?;
                Ok(UsageValue::reported(n)?)
            })
            .transpose()
            .map(Option::unwrap_or_default)
    };
    Ok(Usage {
        input_tokens: value(1)?,
        output_tokens: value(2)?,
        cache_read_tokens: value(3)?,
        cache_write_tokens: value(4)?,
        reasoning_tokens: value(5)?,
        ..Default::default()
    })
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn cursor_turn_usage_preserves_missing_zero_and_cache_counters() {
        assert_eq!(turn_usage(&[]).unwrap(), Usage::default());
        let usage = turn_usage(&w::message(1, &[])).unwrap();
        assert!(usage.input_tokens.value.is_none());
        let usage = turn_usage(
            &[
                w::int(1, 0),
                w::int(2, 12),
                w::int(3, 400),
                w::int(4, 25),
                w::int(5, 7),
            ]
            .concat(),
        )
        .unwrap();
        assert_eq!(usage.input_tokens, UsageValue::reported(0).unwrap());
        assert_eq!(usage.output_tokens.value, Some(12));
        assert_eq!(usage.cache_read_tokens.value, Some(400));
        assert_eq!(usage.cache_write_tokens.value, Some(25));
        assert_eq!(usage.reasoning_tokens.value, Some(7));
        assert!(turn_usage(&w::int(1, u64::MAX)).is_err());
    }
}

fn exec_result(id: u64, exec: &str, kind: u32, result: Vec<u8>) -> Vec<u8> {
    w::message(
        2,
        &[w::int(1, id), w::bytes(15, exec), w::bytes(kind, result)],
    )
}
fn reject_exec(id: u64, exec: &str, kind: u32, args: &[u8]) -> Result<Vec<u8>> {
    let fields = w::fields(args)?;
    let path = w::string(&fields, 1)?;
    let reason = "Local execution is unavailable. Use the provided CONST MCP tools.";
    let (target, result) = match kind {
        2 | 14 | 52 => (
            if kind == 52 { 55 } else { kind },
            w::message(
                if kind == 14 { 5 } else { 4 },
                &[
                    w::bytes(1, path),
                    w::bytes(2, w::string(&fields, 2)?),
                    w::bytes(3, reason),
                ],
            ),
        ),
        3 | 4 => (
            kind,
            w::message(6, &[w::bytes(1, path), w::bytes(2, reason)]),
        ),
        7 | 8 => (
            kind,
            w::message(3, &[w::bytes(1, path), w::bytes(2, reason)]),
        ),
        16 => (
            kind,
            w::message(
                3,
                &[
                    w::bytes(1, path),
                    w::bytes(2, w::string(&fields, 2)?),
                    w::bytes(3, reason),
                ],
            ),
        ),
        20 => (
            kind,
            w::message(2, &[w::bytes(1, path), w::bytes(2, reason)]),
        ),
        5 | 9 | 23 => (kind, w::message(2, &[w::bytes(1, reason)])),
        _ => anyhow::bail!("capability_mismatch: unsupported Cursor exec operation {kind}"),
    };
    Ok(exec_result(id, exec, target, result))
}
fn connect_error(error: &Value) -> ApiError {
    let code = error["code"].as_str().unwrap_or("unknown");
    let status = match code {
        "unauthenticated" => 401,
        "permission_denied" => 403,
        "resource_exhausted" => 429,
        "invalid_argument" | "failed_precondition" => 400,
        "not_found" => 404,
        _ => 502,
    };
    ApiError {
        status,
        code: code.into(),
        detail: crate::upstream_failure::safe_excerpt(
            error["message"]
                .as_str()
                .unwrap_or("Cursor agent stream failed"),
        ),
    }
}

// Budget only retained blob bytes, not a full transcript copy per turn. This is
// shared across all Cursor accounts, including runs awaiting external tools.
static BLOB_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct Blobs {
    data: HashMap<Vec<u8>, Vec<u8>>,
    bytes: usize,
}
impl Blobs {
    fn new(data: HashMap<Vec<u8>, Vec<u8>>) -> Result<Self> {
        let mut blobs = Self {
            data: HashMap::new(),
            bytes: 0,
        };
        for (key, value) in data {
            blobs.put(key, value)?;
        }
        Ok(blobs)
    }
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        use std::sync::atomic::Ordering::Relaxed;
        let old = self.data.get(&key).map_or(0, |v| key.len() + v.len());
        let new = key.len() + value.len();
        if new > old {
            BLOB_BYTES
                .try_update(Relaxed, Relaxed, |n| {
                    n.checked_add(new - old).filter(|n| *n <= 64 * 1024 * 1024)
                })
                .map_err(|_| anyhow!("Cursor retained blob memory limit reached"))?;
        } else {
            BLOB_BYTES.fetch_sub(old - new, Relaxed);
        }
        self.bytes = self.bytes - old + new;
        self.data.insert(key, value);
        Ok(())
    }
}
impl Drop for Blobs {
    fn drop(&mut self) {
        BLOB_BYTES.fetch_sub(self.bytes, std::sync::atomic::Ordering::Relaxed);
    }
}
