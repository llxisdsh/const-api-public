// Opt-in cross-language tests, driven by scripts/test-transport-weak-network.mjs.
// Both Rust roles and Go routing/checkpoint/billing are production code. Only
// the upstream model and the loopback socket fault injector are test doubles.
mod weak_network_tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::AtomicUsize;

    const MODEL: &str = "gpt-4.1-mini";
    const WAIT: Duration = Duration::from_secs(90);

    struct TestHome(Option<PathBuf>);
    impl Drop for TestHome {
        fn drop(&mut self) {
            crate::tool_config::set_test_home_override(self.0.take());
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Fault {
        None,
        Slow,
        Crawl,
        Loss,
        Pause,
        Reset,
        PartialResponse,
        PartialUpload,
    }

    #[derive(Clone, Copy, Debug)]
    struct Case {
        usage: &'static str,
        supply: &'static str,
        leg: &'static str,
        direction: &'static str,
        fault: Fault,
    }

    async fn control(client: &Client, base: &str, path: &str, body: Value) -> Value {
        client
            .post(format!("{base}/{path}"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    // Seeded incompressible-ish text exercises encoded wire size, not just a
    // megabyte of repeated 'a' compressed into one packet. Includes UTF-8 and
    // opaque compaction/tool data to catch byte-boundary corruption.
    fn history(size: usize) -> String {
        let mut x = 0x1234_9876_abcd_0073u64;
        let mut text = String::with_capacity(size + 32);
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for _ in 0..size {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            text.push(ALPHABET[(x % ALPHABET.len() as u64) as usize] as char);
        }
        text.push_str("\n中文🦀 tool result\n");
        text
    }

    fn request(turn: usize, large: bool) -> Value {
        // Large compacted history is opaque, not millions of plaintext tokens.
        // Keep real context-capacity checks enabled in the supplier adapter.
        let large_compaction = large
            && std::env::var("CONST_TRANSPORT_E2E_MODE").as_deref() == Ok("large");
        let mut body = json!({"type":"response.create", "model":MODEL,
            "instructions":"Review this synthetic program, then call read_file.",
            "tools":[{"type":"function","name":"read_file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}],
            "input":[{"role":"user","content":format!("fixture turn {turn}")},
                {"type":"compaction","encrypted_content":if large_compaction {history(65*1024*1024)} else {"opaque-fixture-state".into()}},
                {"type":"function_call_output","call_id":"call-history","output":history(if large {1024*1024} else {64})}]});
        if turn == 1 {
            body["previous_response_id"] = json!("resp-weak-0");
        }
        body
    }

    fn events(turn: usize, large: bool) -> Vec<Value> {
        let id = format!("resp-weak-{turn}");
        let mut out = vec![
            json!({"type":"response.created","response":{"id":id,"status":"in_progress","output":[]}}),
        ];
        for index in 0..3 {
            let size = if large && index == 0
                && std::env::var("CONST_TRANSPORT_E2E_MODE").as_deref() == Ok("large") {
                65*1024*1024
            } else if large {300*1024} else {32};
            out.push(
                json!({"type":"response.output_text.delta", "response_id":id,
                "item_id":format!("item-{turn}"),"output_index":0,"content_index":0,
                "delta":format!("{index}:{}",history(size))}),
            );
        }
        out.push(
            json!({"type":"response.function_call_arguments.done","response_id":id,
            "item_id":"call-next","output_index":1,"arguments":"{\"path\":\"fixture.rs\"}"}),
        );
        out.push(json!({"type":"response.completed","response":{"id":id,"status":"completed","model":MODEL,
            "output":[],"usage":{"input_tokens":128,"output_tokens":64,"total_tokens":192}}}));
        out
    }

    struct UpstreamCall {
        body: Value,
        start: oneshot::Sender<()>,
        rest: oneshot::Sender<()>,
    }

    struct Upstream {
        address: SocketAddr,
        calls: mpsc::Receiver<UpstreamCall>,
        count: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Upstream {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl Upstream {
        async fn new() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (tx, calls) = mpsc::channel(16);
            let count = Arc::new(AtomicUsize::new(0));
            let executions = count.clone();
            let task = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (socket, _) = accepted.unwrap();
                            let tx = tx.clone(); let executions = executions.clone();
                            connections.spawn(async move {
                                let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                                    .max_message_size(Some(crate::MODEL_MESSAGE_MAX_BYTES))
                                    .max_frame_size(Some(crate::MODEL_MESSAGE_MAX_BYTES));
                                let mut ws = tokio_tungstenite::accept_async_with_config(socket, Some(config)).await.unwrap();
                                while let Some(Ok(frame)) = ws.next().await {
                                    if frame.is_close() { break; }
                                    if !frame.is_text() { continue; }
                                    let body: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                                    let turn = executions.fetch_add(1, Ordering::SeqCst);
                                    let (start, started) = oneshot::channel();
                                    let (rest, continued) = oneshot::channel();
                                    if tx.send(UpstreamCall { body, start, rest }).await.is_err() { break; }
                                    if started.await.is_err() { break; }
                                    let frames = events(turn, turn == 0);
                                    if ws.send(WsMessage::Text(frames[0].to_string().into())).await.is_err() { break; }
                                    // Explicit barrier: cuts are injected after a known response
                                    // prefix, never by hoping that a sleep hit the right stage.
                                    if continued.await.is_err() { break; }
                                    for event in &frames[1..] {
                                        if ws.send(WsMessage::Text(event.to_string().into())).await.is_err() { return; }
                                    }
                                }
                            });
                        }
                        result = connections.join_next(), if !connections.is_empty() => { result.unwrap().unwrap(); }
                    }
                }
            });
            Self {
                address,
                calls,
                count,
                task,
            }
        }
    }

    struct Agent {
        stop: Option<oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<()>,
        _updates: mpsc::Sender<SupplierRegistrationUpdate>,
        state: Arc<StdMutex<SupplierTransportSnapshot>>,
    }
    impl Drop for Agent {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            self.task.abort();
        }
    }
    fn agent(urls: &Value, transport: &str, upstream: SocketAddr) -> Agent {
        let mut supplier = default_supplier_config();
        supplier.channel_id = uuid::Uuid::new_v4().to_string();
        supplier.node_id = supplier.channel_id.clone();
        supplier.enabled = true;
        supplier.source_driver = crate::source_driver::SourceDriverId::OpenAiApi;
        supplier.upstream_base_url = format!("http://{upstream}/v1");
        supplier.surface_bindings = crate::source_driver_surface_bindings(
            supplier.source_driver,
            &supplier.upstream_base_url,
        );
        for binding in &mut supplier.surface_bindings {
            binding.verification.state = "verified".into();
            for protocol in &mut binding.protocols {
                protocol.verification.state = "verified".into();
            }
        }
        supplier.api_format = "openai_responses".into();
        supplier.models = vec![MODEL.into()];
        supplier.public_model = MODEL.into();
        supplier.upstream_model = MODEL.into();
        supplier.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".into(),
            model_pattern: MODEL.into(),
            context_tokens: Some(1_000_000),
            output_tokens: Some(32768),
            non_stream_json: true,
            stream_sse: true,
            tool_calls: true,
            tool_choice: true,
            parallel_tool_calls: true,
            verification_state: "verified".into(),
            ..Default::default()
        }];
        let config = SupplierAgentConfig {
            client_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            suppliers: vec![supplier],
            server_ws_url: urls["websocket"].as_str().unwrap().into(),
            server_quic_url: if transport == "quic" {
                urls["quic"].as_str().unwrap().into()
            } else {
                String::new()
            },
            access_token: Arc::new(StdMutex::new("weak-supplier".into())),
            account_refresh_notify: Arc::new(Notify::new()),
        };
        let (stop, shutdown) = oneshot::channel();
        let (updates, changes) = mpsc::channel(4);
        let state = Arc::new(StdMutex::new(SupplierTransportSnapshot::default()));
        let task = tokio::spawn(run_supplier_agent(config, shutdown, changes, state.clone()));
        Agent {
            stop: Some(stop),
            task,
            _updates: updates,
            state,
        }
    }

    async fn state(client: &Client, base: &str) -> Value {
        control(client, base, "state", Value::Null).await
    }

    async fn fault(client: &Client, base: &str, case: Case) -> Value {
        let mut body = json!({"leg":case.leg,"direction":case.direction});
        match case.fault {
            Fault::None => {}
            Fault::Slow => {
                body["bytes_per_second"] = json!(256 * 1024);
                body["delay_ms"] = json!(1);
            }
            Fault::Crawl => {
                body["bytes_per_second"] = json!(24 * 1024);
            }
            Fault::Loss => {
                body["loss_every"] = json!(19);
                body["reorder_every"] = json!(13);
            }
            Fault::Pause => {
                body["pause_ms"] = json!(2200);
            }
            Fault::Reset | Fault::PartialResponse | Fault::PartialUpload => {
                body["cut"] = json!(true);
            }
        }
        control(client, base, "fault", body).await
    }

    async fn wait_for_bytes(client: &Client, base: &str, case: Case, before: i64) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let now = state(client, base).await["links"][case.leg][case.direction]["bytes"]
                    .as_i64()
                    .unwrap();
                if now >= before + 64 * 1024 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("fault did not reach the intended partial-message boundary");
    }

    async fn clear_fault(client: &Client, base: &str, case: Case) {
        control(
            client,
            base,
            "fault",
            json!({"leg":case.leg,"direction":case.direction}),
        )
        .await;
    }

    async fn next_event(receiver: &mut PlatformDuplexReceiver) -> Value {
        let wait = if std::env::var("CONST_TRANSPORT_E2E_MODE").as_deref() == Ok("large") {
            Duration::from_secs(300)
        } else { WAIT };
        match tokio::time::timeout(wait, receiver.next())
            .await
            .expect("response stalled")
            .expect("response closed")
            .expect("transport failed")
        {
            PlatformDuplexFrame::Text(text) => serde_json::from_str(&text).unwrap(),
            _ => panic!("unexpected non-text response"),
        }
    }
    fn assert_event(actual: &Value, expected: &Value) {
        // Production adds turn metadata. The original event must be preserved.
        for (key, value) in expected.as_object().unwrap() {
            assert!(
                actual.get(key) == Some(value),
                "response field {key} was lost, duplicated, reordered, or corrupted ({})",
                actual["type"]
            );
        }
    }

    async fn run_case(client: &Client, base: &str, case: Case) {
        let started = Instant::now();
        let urls = control(client, base, "reset", Value::Null).await;
        let mut upstream = Upstream::new().await;
        let mut agent = agent(&urls["supply"], case.supply, upstream.address);
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if state(client, base).await["nodes"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("supplier registration failed");
        assert!(
            agent
                .state
                .lock()
                .unwrap()
                .nodes
                .values()
                .any(|node| node.connected_transport == case.supply),
            "supplier did not use the requested transport"
        );
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "weak-network-fixture".into(),
            enabled: true,
            base_url: urls["usage"]["base_url"].as_str().unwrap().into(),
            supplier_ws_url: urls["usage"]["websocket"].as_str().unwrap().into(),
            supplier_quic_url: if case.usage == "quic" {
                urls["usage"]["quic"].as_str().unwrap().into()
            } else {
                String::new()
            },
        };
        let pool = PlatformDuplexPool::default();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            "Bearer weak-consumer".parse().unwrap(),
        );
        let open = || {
            open_platform_responses_duplex(
                &pool,
                &endpoint,
                Arc::new(StdMutex::new("weak-consumer".into())),
                Arc::new(Notify::new()),
                headers.clone(),
            )
        };
        let (session, transport) = open().await.expect("usage tunnel failed");
        assert_eq!(
            transport, case.usage,
            "the test silently fell back before fault injection"
        );
        let (mut sender, mut receiver) = session.split();
        if matches!(case.fault, Fault::PartialUpload) {
            control(
                client,
                base,
                "fault",
                json!({"leg":case.leg,"direction":case.direction,"bytes_per_second":32*1024}),
            )
            .await;
            let before = state(client, base).await["links"][case.leg][case.direction]["bytes"]
                .as_i64()
                .unwrap();
            let upload = tokio::spawn(async move {
                let result = sender
                    .send(PlatformDuplexFrame::Text(request(0, true).to_string()))
                    .await;
                (sender, result)
            });
            wait_for_bytes(client, base, case, before).await;
            let previous_registration = agent
                .state
                .lock()
                .unwrap()
                .nodes
                .values()
                .map(|node| node.last_register_ack_unix)
                .max()
                .unwrap_or_default();
            assert_eq!(
                upstream.count.load(Ordering::SeqCst),
                0,
                "a partial request executed"
            );
            assert!(
                fault(client, base, case).await["cut_connections"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
            // A partial upload is not permission to execute again. Model a caller
            // cancelling that ambiguous turn, then making an explicit new request.
            // The first-frame expiry itself is covered by the Go virtual-clock test.
            let (old_sender, _) = tokio::time::timeout(Duration::from_secs(5), upload)
                .await
                .unwrap()
                .unwrap();
            drop(old_sender);
            drop(receiver);
            clear_fault(client, base, case).await;
            if case.leg == "supply" {
                // With only a WS candidate, the real agent has a reconnect
                // backoff. A new call before its next registration is a routing
                // rejection, not a failed replay of the abandoned upload.
                tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        if agent
                            .state
                            .lock()
                            .unwrap()
                            .nodes
                            .values()
                            .any(|node| node.last_register_ack_unix > previous_registration)
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                })
                .await
                .expect("supplier did not register again after the partial upload");
            }
            let (new, _) = open().await.unwrap();
            (sender, receiver) = new.split();
            assert_eq!(
                upstream.count.load(Ordering::SeqCst),
                0,
                "reconnect replayed an incomplete request"
            );
        } else if !matches!(case.fault, Fault::Reset | Fault::PartialResponse) {
            fault(client, base, case).await;
        }
        // Turn 0: large coding history. Turn 1: same conversation after recovery.
        // Turn 2: a new conversation on the same pool; neither may be poisoned.
        for turn in 0..3 {
            if turn > 0 && matches!(case.fault, Fault::Pause) {
                fault(client, base, case).await;
            }
            if turn == 2 {
                let (next, _) = open().await.unwrap();
                (sender, receiver) = next.split();
            }
            let expected = request(turn, turn == 0);
            let encoded = expected.to_string();
            if turn == 0 && std::env::var("CONST_TRANSPORT_E2E_MODE").as_deref() == Ok("large") {
                assert!(encoded.len() > 64 * 1024 * 1024);
            }
            let send = sender.send(PlatformDuplexFrame::Text(encoded));
            let upload = async {
                // Debug Rust plus Go's race instrumentation parses a real
                // 65 MiB payload at each boundary; this is not a network SLA.
                let wait = if std::env::var("CONST_TRANSPORT_E2E_MODE").as_deref() == Ok("large") {
                    Duration::from_secs(300)
                } else {
                    WAIT
                };
                let call = tokio::time::timeout(wait, async {
                    tokio::select! {
                        call=upstream.calls.recv()=>call.expect("upstream fixture stopped"),
                        error=receiver.next()=>panic!("platform replied before upstream execution: {error:?}"),
                    }
                }).await.expect("request never reached upstream");
                for key in [
                    "model",
                    "instructions",
                    "tools",
                    "input",
                    "previous_response_id",
                ] {
                    assert!(
                        call.body[key] == expected[key],
                        "request field {key} corrupted"
                    );
                }
                call
            };
            let (sent, call) = tokio::join!(send, upload);
            sent.expect("request upload failed");
            call.start.send(()).unwrap();
            let expected_events = events(turn, turn == 0);
            assert_event(&next_event(&mut receiver).await, &expected_events[0]);
            if matches!(case.fault, Fault::Reset) {
                let result = fault(client, base, case).await;
                assert!(
                    result["cut_connections"].as_u64().unwrap() > 0,
                    "reset did not hit a live connection"
                );
            }
            if turn == 0 && matches!(case.fault, Fault::PartialResponse) {
                control(
                    client,
                    base,
                    "fault",
                    json!({"leg":case.leg,"direction":case.direction,"bytes_per_second":32*1024}),
                )
                .await;
                let before = state(client, base).await["links"][case.leg][case.direction]["bytes"]
                    .as_i64()
                    .unwrap();
                call.rest.send(()).unwrap();
                wait_for_bytes(client, base, case, before).await;
                assert!(
                    fault(client, base, case).await["cut_connections"]
                        .as_u64()
                        .unwrap()
                        > 0
                );
                clear_fault(client, base, case).await;
            } else {
                call.rest.send(()).unwrap();
            }
            for event in &expected_events[1..] {
                assert_event(&next_event(&mut receiver).await, event);
            }
            assert_eq!(
                upstream.count.load(Ordering::SeqCst),
                turn + 1,
                "network recovery re-executed upstream"
            );
            if turn == 0 && matches!(case.fault, Fault::Crawl) {
                assert!(
                    started.elapsed() > Duration::from_secs(30),
                    "slow upload did not cross the former first-frame deadline"
                );
                clear_fault(client, base, case).await;
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let s = state(client, base).await;
                if s["usage"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|record| record["http_status"] == 200)
                    .count()
                    >= 3
                {
                    break s;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("settlement did not finish");
        let usage = result["usage"].as_array().unwrap();
        assert_eq!(
            usage
                .iter()
                .filter(|record| record["http_status"] == 200)
                .count(),
            3,
            "duplicate settlement"
        );
        for record in usage {
            if matches!(case.fault, Fault::PartialUpload) && record["http_status"] != 200 {
                assert_eq!(record["charged"], 0.0, "incomplete upload was billed");
                continue;
            }
            assert_eq!(record["http_status"], 200, "failed settlement: {record}");
            assert_eq!(record["input_tokens"], 128);
            assert_eq!(record["output_tokens"], 64);
        }
        if matches!(case.fault, Fault::Loss) {
            let direction = &result["links"][case.leg][case.direction];
            assert!(
                direction["dropped"].as_i64().unwrap() > 0
                    && direction["reordered"].as_i64().unwrap() > 0,
                "UDP faults were not exercised"
            );
        }
        drop(sender);
        drop(receiver);
        let _ = agent.stop.take().unwrap().send(());
        tokio::time::timeout(Duration::from_secs(5), &mut agent.task)
            .await
            .expect("supplier did not stop")
            .unwrap();
        println!(
            "PASS {case:?} elapsed_ms={} executions=3 settlements=3",
            started.elapsed().as_millis()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires the real Go platform fixture: node scripts/test-transport-weak-network.mjs"]
    async fn real_platform_weak_network_matrix() {
        let base = std::env::var("CONST_TRANSPORT_E2E_URL").expect("use the integration runner");
        let mode = std::env::var("CONST_TRANSPORT_E2E_MODE").unwrap_or_else(|_| "all".into());
        let _lock = crate::tool_config::test_home_lock().lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _home = TestHome(crate::tool_config::set_test_home_override(Some(
            home.path().into(),
        )));
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        let mut cases = Vec::new();
        for transport in ["quic", "websocket"] {
            if mode != "all" && mode != "large" && mode != transport {
                continue;
            }
            let baseline = Case {
                usage: transport,
                supply: transport,
                leg: "usage",
                direction: "up",
                fault: Fault::None,
            };
            cases.push(baseline);
            if mode == "large" {
                continue;
            }
            cases.push(Case {
                fault: Fault::Crawl,
                ..baseline
            });
            for fault in [Fault::Slow, Fault::Loss, Fault::Pause] {
                if matches!(fault, Fault::Loss) && transport != "quic" {
                    continue;
                }
                for leg in ["usage", "supply"] {
                    for direction in ["up", "down"] {
                        cases.push(Case {
                            leg,
                            direction,
                            fault,
                            ..baseline
                        });
                    }
                }
            }
            for leg in ["usage", "supply"] {
                cases.push(Case {
                    leg,
                    fault: Fault::Reset,
                    ..baseline
                });
            }
            for (leg, direction) in [("usage", "down"), ("supply", "up")] {
                cases.push(Case {
                    leg,
                    direction,
                    fault: Fault::PartialResponse,
                    ..baseline
                });
            }
            for (leg, direction) in [("usage", "up"), ("supply", "down")] {
                cases.push(Case {
                    leg,
                    direction,
                    fault: Fault::PartialUpload,
                    ..baseline
                });
            }
        }
        if mode == "all" {
            cases.push(Case {
                usage: "quic",
                supply: "websocket",
                leg: "supply",
                direction: "down",
                fault: Fault::Slow,
            });
            cases.push(Case {
                usage: "websocket",
                supply: "quic",
                leg: "usage",
                direction: "down",
                fault: Fault::Slow,
            });
        }
        let count = cases.len();
        for case in cases {
            run_case(&client, &base, case).await;
        }
        println!(
            "weak-network matrix: {count} scenarios passed (real Rust / Go / Rust, loopback upstream)"
        );
    }
}
