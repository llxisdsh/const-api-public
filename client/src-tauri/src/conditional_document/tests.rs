use super::*;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use warp::{Filter, http::HeaderMap};

pub(crate) type Reply = (u16, Option<&'static str>, Option<&'static str>, Vec<u8>);

pub(crate) struct TestServer {
    pub(crate) url: String,
    pub(crate) seen: Arc<Mutex<Vec<HeaderMap>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) async fn mock_server(replies: Vec<Reply>, delay_ms: u64) -> TestServer {
    let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let route =
        warp::any()
            .and(warp::header::headers_cloned())
            .and_then(move |headers: HeaderMap| {
                captured.lock().unwrap().push(headers);
                let reply = replies.lock().unwrap().pop_front().unwrap_or((
                    500,
                    None,
                    None,
                    b"unexpected request".to_vec(),
                ));
                async move {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    let mut response = warp::http::Response::builder().status(reply.0);
                    if let Some(etag) = reply.1 {
                        response = response.header(ETAG, etag);
                    }
                    if let Some(modified) = reply.2 {
                        response = response.header(LAST_MODIFIED, modified);
                    }
                    Ok::<_, std::convert::Infallible>(response.body(reply.3).unwrap())
                }
            });
    let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
    TestServer {
        url: format!("http://{address}/signed.json"),
        seen,
        task: tokio::spawn(server),
    }
}

fn decode(raw: &[u8], _: &Url) -> Result<String> {
    Ok(std::str::from_utf8(raw)?.to_string())
}

#[tokio::test]
async fn etag_and_last_modified_revalidate_without_redownloading_body() {
    let modified = "Thu, 10 Sep 2026 10:00:00 GMT";
    for etag in [Some("\"v1\""), None] {
        let server = mock_server(
            vec![
                (200, etag, Some(modified), b"signed-document".to_vec()),
                (304, etag, Some(modified), vec![]),
            ],
            0,
        )
        .await;
        let cache = ConditionalDocumentCache::default();
        let client = reqwest::Client::new();
        let validations = AtomicUsize::new(0);
        for _ in 0..2 {
            let value = cache
                .fetch(&server.url, client.get(&server.url), None, |raw, url| {
                    validations.fetch_add(1, Ordering::SeqCst);
                    decode(raw, url)
                })
                .await
                .unwrap();
            assert_eq!(value, "signed-document");
        }
        assert_eq!(
            validations.load(Ordering::SeqCst),
            2,
            "304 must still run the signature/expiry verifier"
        );
        let seen = server.seen.lock().unwrap();
        assert!(!seen[0].contains_key(IF_NONE_MATCH));
        assert_eq!(seen[1][IF_MODIFIED_SINCE], modified);
        assert_eq!(
            seen[1]
                .get(IF_NONE_MATCH)
                .map(|value| value.to_str().unwrap()),
            etag
        );
    }
}

#[tokio::test]
async fn old_servers_without_validators_or_ignoring_them_return_fresh_full_documents() {
    for etag in [None, Some("\"v1\"")] {
        let server = mock_server(
            vec![
                (200, etag, None, b"old".to_vec()),
                (200, None, None, b"new".to_vec()),
            ],
            0,
        )
        .await;
        let cache = ConditionalDocumentCache::default();
        let client = reqwest::Client::new();
        cache
            .fetch(&server.url, client.get(&server.url), None, decode)
            .await
            .unwrap();
        assert_eq!(
            cache
                .fetch(&server.url, client.get(&server.url), None, decode)
                .await
                .unwrap(),
            "new"
        );
        assert_eq!(server.seen.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn overlapping_reads_share_download_but_each_verifies_and_later_reads_revalidate() {
    let server = mock_server(
        vec![
            (200, Some("\"v1\""), None, b"signed".to_vec()),
            (304, Some("\"v1\""), None, vec![]),
        ],
        40,
    )
    .await;
    let cache = ConditionalDocumentCache::default();
    let client = reqwest::Client::new();
    let validations = AtomicUsize::new(0);
    let results = futures_util::future::join_all((0..20).map(|_| {
        cache.fetch(&server.url, client.get(&server.url), None, |raw, url| {
            validations.fetch_add(1, Ordering::SeqCst);
            decode(raw, url)
        })
    }))
    .await;
    assert!(
        results
            .into_iter()
            .all(|result| result.unwrap() == "signed")
    );
    assert_eq!(validations.load(Ordering::SeqCst), 20);
    assert_eq!(server.seen.lock().unwrap().len(), 1);
    cache
        .fetch(&server.url, client.get(&server.url), None, decode)
        .await
        .unwrap();
    assert_eq!(server.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn bad_304_repairs_once_without_accepting_an_unrequested_or_mismatched_validator() {
    let server = mock_server(
        vec![
            (304, None, None, vec![]),
            (200, None, None, b"no-validator".to_vec()),
            (304, None, None, vec![]),
            (200, Some("\"v1\""), None, b"old".to_vec()),
            (304, Some("\"wrong\""), None, vec![]),
            (200, Some("\"v2\""), None, b"new".to_vec()),
        ],
        0,
    )
    .await;
    let cache = ConditionalDocumentCache::default();
    let client = reqwest::Client::new();
    let validations = AtomicUsize::new(0);
    for expected in ["no-validator", "old", "new"] {
        let result = cache
            .fetch(&server.url, client.get(&server.url), None, |raw, url| {
                validations.fetch_add(1, Ordering::SeqCst);
                decode(raw, url)
            })
            .await
            .unwrap();
        assert_eq!(result, expected);
    }
    assert_eq!(
        validations.load(Ordering::SeqCst),
        3,
        "bad 304 must not cause verifier/persistence side effects"
    );
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), 6);
    for index in [1, 3, 5] {
        assert!(!seen[index].contains_key(IF_NONE_MATCH));
    }
    drop(seen);
    let server = mock_server(vec![(304, None, None, vec![]); 2], 0).await;
    assert!(
        cache
            .fetch(&server.url, client.get(&server.url), None, decode)
            .await
            .is_err()
    );
    assert_eq!(server.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn expired_or_distrusted_cache_is_revalidated_and_repaired_unconditionally() {
    let server = mock_server(
        vec![
            (200, Some("\"v1\""), None, b"old".to_vec()),
            (304, Some("\"v1\""), None, vec![]),
            (200, Some("\"v2\""), None, b"new".to_vec()),
        ],
        0,
    )
    .await;
    let cache = ConditionalDocumentCache::default();
    let client = reqwest::Client::new();
    cache
        .fetch(&server.url, client.get(&server.url), None, decode)
        .await
        .unwrap();
    let result = cache
        .fetch(&server.url, client.get(&server.url), None, |raw, url| {
            if raw == b"old" {
                return Err(anyhow!("expired or no longer trusted"));
            }
            decode(raw, url)
        })
        .await
        .unwrap();
    assert_eq!(result, "new");
    assert!(!server.seen.lock().unwrap()[2].contains_key(IF_NONE_MATCH));
}

#[tokio::test]
async fn server_and_validation_failures_never_look_like_fresh_success_or_replace_good_cache() {
    for status in [401, 503, 200] {
        let server = mock_server(
            vec![
                (200, Some("\"good\""), None, b"good".to_vec()),
                (status, Some("\"bad\""), None, b"bad".to_vec()),
                (304, Some("\"good\""), None, vec![]),
            ],
            0,
        )
        .await;
        let cache = ConditionalDocumentCache::default();
        let client = reqwest::Client::new();
        let validate = |raw: &[u8], url: &Url| {
            if raw == b"bad" {
                return Err(anyhow!("signature rejected"));
            }
            decode(raw, url)
        };
        cache
            .fetch(&server.url, client.get(&server.url), None, validate)
            .await
            .unwrap();
        assert!(
            cache
                .fetch(&server.url, client.get(&server.url), None, validate)
                .await
                .is_err()
        );
        assert_eq!(
            cache
                .fetch(&server.url, client.get(&server.url), None, validate)
                .await
                .unwrap(),
            "good"
        );
        assert_eq!(server.seen.lock().unwrap()[2][IF_NONE_MATCH], "\"good\"");
    }
}

#[tokio::test]
async fn body_limits_bound_retained_memory_without_breaking_large_valid_documents() {
    let server = mock_server(
        vec![
            (
                200,
                Some("\"large\""),
                None,
                vec![b'x'; MAX_CACHED_BODY_BYTES + 1],
            ),
            (200, Some("\"small\""), None, b"small".to_vec()),
            (200, Some("\"oversize\""), None, vec![b'x'; 1025]),
        ],
        0,
    )
    .await;
    let cache = ConditionalDocumentCache::default();
    let client = reqwest::Client::new();
    assert_eq!(
        cache
            .fetch(&server.url, client.get(&server.url), None, decode)
            .await
            .unwrap()
            .len(),
        MAX_CACHED_BODY_BYTES + 1
    );
    cache
        .fetch(&server.url, client.get(&server.url), None, decode)
        .await
        .unwrap();
    assert!(!server.seen.lock().unwrap()[1].contains_key(IF_NONE_MATCH));
    assert!(
        cache
            .fetch(&server.url, client.get(&server.url), Some(1024), decode)
            .await
            .unwrap_err()
            .to_string()
            .contains("too large")
    );
}

#[test]
fn source_slots_are_bounded_and_do_not_evict_in_flight_downloads() {
    let cache = ConditionalDocumentCache::default();
    let first = cache.slot("first");
    assert!(Arc::ptr_eq(&first, &cache.slot("first")));
    let active: Vec<_> = (1..MAX_SOURCES)
        .map(|index| cache.slot(&format!("source-{index}")))
        .collect();
    let extra = cache.slot("extra");
    assert_eq!(cache.sources.lock().unwrap().len(), MAX_SOURCES);
    assert!(Arc::ptr_eq(&first, &cache.slot("first")));
    assert!(
        !Arc::ptr_eq(&extra, &cache.slot("extra")),
        "overflow requests are uncached instead of evicting active sources"
    );
    drop(active);
    drop(extra);
    for index in 0..MAX_SOURCES * 2 {
        drop(cache.slot(&format!("later-{index}")));
    }
    assert_eq!(cache.sources.lock().unwrap().len(), MAX_SOURCES);
    assert!(Arc::ptr_eq(&first, &cache.slot("first")));
}
