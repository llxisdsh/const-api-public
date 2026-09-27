use super::*;
use std::{collections::VecDeque, time::Duration};
use warp::{Filter, http::HeaderMap};

type Reply = (u16, Option<&'static str>, Value);
type Seen = Arc<StdMutex<Vec<(Method, HeaderMap)>>>;

fn payload(model: &str) -> Value {
    json!({"object":"account_model_compatibility", "revision":1,
        "release_id":"catalog-test", "availability_as_of":"old-time",
        "model_groups":[], "template_model_groups":[], "catalog_models":[],
        "platform_available_models":[model]})
}

async fn mock_server(
    replies: Vec<Reply>,
    delay_ms: u64,
) -> (ClientConfig, Seen, tokio::task::JoinHandle<()>) {
    mock_server_with_gate(replies, delay_ms, None).await
}

async fn mock_server_with_gate(
    replies: Vec<Reply>,
    delay_ms: u64,
    gate: Option<Arc<tokio::sync::Notify>>,
) -> (ClientConfig, Seen, tokio::task::JoinHandle<()>) {
    let replies = Arc::new(StdMutex::new(VecDeque::from(replies)));
    let seen: Seen = Default::default();
    let captured = seen.clone();
    let route = warp::any()
        .and(warp::method())
        .and(warp::header::headers_cloned())
        .and_then(move |method: Method, headers: HeaderMap| {
            captured.lock().unwrap().push((method, headers));
            let reply = replies.lock().unwrap().pop_front().unwrap_or((
                500,
                None,
                json!({"error":"unexpected request"}),
            ));
            let gate = gate.clone();
            async move {
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                let mut response = warp::http::Response::builder()
                    .status(reply.0)
                    .header("x-const-availability-as-of", "new-time");
                if let Some(etag) = reply.1 {
                    response = response.header("etag", etag);
                }
                let body = if reply.0 == 304 {
                    String::new()
                } else {
                    reply.2.to_string()
                };
                Ok::<_, std::convert::Infallible>(response.body(body).unwrap())
            }
        });
    let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
    let mut config = crate::config::default_config();
    config.platform_id = "cache-test".to_string();
    config.account_platform_id = "cache-test".to_string();
    config.account_user_id = "user-one".to_string();
    config.account_device_api_key = "mock-device-key".to_string();
    config.endpoints = vec![crate::model::Endpoint {
        server_id: String::new(),
        name: "mock-server".to_string(),
        base_url: format!("http://{address}"),
        supplier_ws_url: String::new(),
        supplier_quic_url: String::new(),
        enabled: true,
    }];
    (config, seen, tokio::spawn(server))
}

async fn read(
    config: &ClientConfig,
    cache: &StdMutex<ModelCompatibilityHttpCache>,
) -> std::result::Result<Value, RemoteModelCompatibilityError> {
    request_account_model_compatibility(config, Method::GET, None, None, cache).await
}

#[tokio::test]
async fn conditional_read_reuses_body_and_preserves_live_supply_updates() {
    let (config, seen, server) = mock_server(
        vec![
            (200, Some("W/\"v1\""), payload("a")),
            (304, Some("W/\"v1\""), Value::Null),
            (200, Some("W/\"v2\""), payload("b")),
        ],
        0,
    )
    .await;
    let cache = StdMutex::new(ModelCompatibilityHttpCache::default());
    assert_eq!(
        read(&config, &cache).await.unwrap()["platform_available_models"],
        json!(["a"])
    );
    let unchanged = read(&config, &cache).await.unwrap();
    assert_eq!(unchanged["platform_available_models"], json!(["a"]));
    assert_eq!(unchanged["availability_as_of"], "new-time");
    assert_eq!(
        read(&config, &cache).await.unwrap()["platform_available_models"],
        json!(["b"])
    );
    let seen = seen.lock().unwrap();
    assert!(!seen[0].1.contains_key(IF_NONE_MATCH));
    assert_eq!(seen[1].1[IF_NONE_MATCH], "W/\"v1\"");
    assert_eq!(seen[2].1[IF_NONE_MATCH], "W/\"v1\"");
    assert_eq!(seen[1].1["authorization"], "Bearer mock-device-key");
    server.abort();
}

#[tokio::test]
async fn old_server_full_responses_still_work_without_validators() {
    let (config, seen, server) = mock_server(
        vec![(200, None, payload("a")), (200, None, payload("b"))],
        0,
    )
    .await;
    let cache = StdMutex::new(ModelCompatibilityHttpCache::default());
    read(&config, &cache).await.unwrap();
    assert_eq!(
        read(&config, &cache).await.unwrap()["platform_available_models"],
        json!(["b"])
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|(_, headers)| !headers.contains_key(IF_NONE_MATCH))
    );
    server.abort();
}

#[tokio::test]
async fn overlapping_reads_and_failures_each_make_one_request_without_new_ttl() {
    for status in [200, 503] {
        let (config, seen, server) = mock_server(
            vec![
                (status, Some("\"v1\""), payload("a")),
                (200, Some("\"v2\""), payload("b")),
            ],
            40,
        )
        .await;
        let cache = StdMutex::new(ModelCompatibilityHttpCache::default());
        let results = futures_util::future::join_all((0..20).map(|_| read(&config, &cache))).await;
        assert!(results.iter().all(|value| value.is_ok() == (status == 200)));
        assert_eq!(seen.lock().unwrap().len(), 1);
        // A subsequent (not overlapping) refresh checks the server immediately.
        assert_eq!(
            read(&config, &cache).await.unwrap()["platform_available_models"],
            json!(["b"])
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
        server.abort();
    }
}

#[tokio::test]
async fn account_credential_and_endpoint_changes_do_not_reuse_old_validators() {
    let (mut config, seen, server) =
        mock_server(vec![(200, Some("\"v1\""), payload("a")); 6], 0).await;
    let cache = StdMutex::new(ModelCompatibilityHttpCache::default());
    read(&config, &cache).await.unwrap();
    config.account_user_id = "user-two".to_string();
    read(&config, &cache).await.unwrap();
    config.account_device_api_key = "rotated-key".to_string();
    read(&config, &cache).await.unwrap();
    config.allow_unverified_platform_routes = !config.allow_unverified_platform_routes;
    read(&config, &cache).await.unwrap();
    config.platform_id = "platform-two".to_string();
    config.account_platform_id = "platform-two".to_string();
    read(&config, &cache).await.unwrap();
    config.endpoints[0].base_url.push_str("/alternate-endpoint");
    read(&config, &cache).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 6);
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .enumerate()
            .all(|(index, (_, headers))| headers.contains_key(IF_NONE_MATCH) == (index == 3))
    );
    server.abort();
}

#[tokio::test]
async fn saves_resets_and_rejected_writes_invalidate_the_read_cache() {
    let (config, seen, server) = mock_server(
        vec![
            (200, Some("\"v1\""), payload("a")),
            (200, None, payload("a")),
            (200, Some("\"v2\""), payload("b")),
            (409, None, json!({"error":"revision conflict"})),
            (200, Some("\"v3\""), payload("c")),
        ],
        0,
    )
    .await;
    let cache = StdMutex::new(ModelCompatibilityHttpCache::default());
    read(&config, &cache).await.unwrap();
    request_account_model_compatibility(
        &config,
        Method::PUT,
        Some(json!({"revision":1})),
        None,
        &cache,
    )
    .await
    .unwrap();
    read(&config, &cache).await.unwrap();
    let error = request_account_model_compatibility(&config, Method::DELETE, None, Some(2), &cache)
        .await
        .unwrap_err();
    assert_eq!(error.status, Some(409));
    assert_eq!(
        read(&config, &cache).await.unwrap()["platform_available_models"],
        json!(["c"])
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|(_, headers)| !headers.contains_key(IF_NONE_MATCH))
    );
    assert_eq!(seen.lock().unwrap().len(), 5);
    server.abort();
}

#[tokio::test]
async fn authorization_failures_are_not_hidden_by_cached_success() {
    for status in [401, 403] {
        let (config, seen, server) = mock_server(
            vec![
                (200, Some("\"v1\""), payload("a")),
                (status, None, json!({"error":{"message":"access revoked"}})),
                (200, Some("\"v2\""), payload("b")),
            ],
            0,
        )
        .await;
        let cache = StdMutex::new(ModelCompatibilityHttpCache::default());
        read(&config, &cache).await.unwrap();
        assert_eq!(
            read(&config, &cache).await.unwrap_err().status,
            Some(status)
        );
        read(&config, &cache).await.unwrap();
        assert!(!seen.lock().unwrap()[2].1.contains_key(IF_NONE_MATCH));
        server.abort();
    }
}

#[tokio::test]
async fn missing_or_mismatched_304_cache_repairs_once_and_never_replays_writes() {
    let (config, seen, server) = mock_server(
        vec![
            (304, None, Value::Null),
            (200, Some("\"v1\""), payload("a")),
            (304, Some("\"different\""), Value::Null),
            (200, Some("\"v2\""), payload("b")),
            (304, None, Value::Null),
        ],
        0,
    )
    .await;
    let cache = StdMutex::new(ModelCompatibilityHttpCache::default());
    read(&config, &cache).await.unwrap();
    assert_eq!(
        read(&config, &cache).await.unwrap()["platform_available_models"],
        json!(["b"])
    );
    assert!(!seen.lock().unwrap()[1].1.contains_key(IF_NONE_MATCH));
    assert!(!seen.lock().unwrap()[3].1.contains_key(IF_NONE_MATCH));
    assert_eq!(
        request_account_model_compatibility(&config, Method::PUT, Some(json!({})), None, &cache)
            .await
            .unwrap_err()
            .status,
        Some(304)
    );
    assert_eq!(seen.lock().unwrap().len(), 5);
    server.abort();
    let (config, seen, server) = mock_server(vec![(304, None, Value::Null); 2], 0).await;
    assert_eq!(
        read(&config, &StdMutex::new(Default::default()))
            .await
            .unwrap_err()
            .status,
        Some(304)
    );
    assert_eq!(seen.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn account_switch_does_not_wait_for_or_get_populated_by_old_response() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let (first_config, first_seen, first_server) = mock_server_with_gate(
        vec![(200, Some("\"a\""), payload("a"))],
        0,
        Some(gate.clone()),
    )
    .await;
    let (second_config, second_seen, second_server) = mock_server(
        vec![
            (200, Some("\"b\""), payload("b")),
            (304, Some("\"b\""), Value::Null),
        ],
        0,
    )
    .await;
    let cache = Arc::new(StdMutex::new(ModelCompatibilityHttpCache::default()));
    let first_cache = cache.clone();
    let old = tokio::spawn(async move { read(&first_config, &first_cache).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while first_seen.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let value = tokio::time::timeout(Duration::from_secs(5), read(&second_config, &cache))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value["platform_available_models"], json!(["b"]));
    assert!(!old.is_finished());
    gate.notify_one();
    old.await.unwrap().unwrap();
    assert_eq!(
        read(&second_config, &cache).await.unwrap()["platform_available_models"],
        json!(["b"])
    );
    assert_eq!(second_seen.lock().unwrap()[1].1[IF_NONE_MATCH], "\"b\"");
    first_server.abort();
    second_server.abort();
}
