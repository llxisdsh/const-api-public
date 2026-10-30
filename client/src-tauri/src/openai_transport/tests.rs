use super::*;

fn account(id: &str) -> serde_json::Value {
    serde_json::json!({"type":"openai", "account_id": id, "access_token":"fixture"})
}

fn store(jar: &InfrastructureCookies, url: &str, values: &[&str]) {
    let values: Vec<HeaderValue> = values.iter().map(|value| value.parse().unwrap()).collect();
    jar.set_cookies(&mut values.iter(), &Url::parse(url).unwrap());
}

#[test]
fn cookie_scope_allowlist_expiry_and_deletion() {
    let jar = InfrastructureCookies::default();
    let url = "https://chatgpt.com/backend-api/codex/responses";
    store(
        &jar,
        url,
        &[
            "__oailb=route; Path=/backend-api; Secure",
            "__cf_bm=host; Path=/; Secure",
            "cf_chl_rc_i=challenge; Path=/; Secure",
            "session=private; Path=/; Secure",
            "__Secure-next-auth.session-token=private; Path=/; Secure",
            "access_token=private; Path=/; Secure",
            "__cflb=expired; Max-Age=0; Path=/; Secure",
        ],
    );
    let header = jar.cookies(&Url::parse(url).unwrap()).unwrap();
    assert!(header.is_sensitive());
    let text = header.to_str().unwrap();
    assert!(text.contains("__oailb=route"));
    assert!(text.contains("cf_chl_rc_i=challenge"));
    assert!(!text.contains("private"));
    assert!(!text.contains("session"));
    assert!(!text.contains("expired"));
    assert!(
        !jar.cookies(&Url::parse("https://chatgpt.com/elsewhere").unwrap())
            .unwrap()
            .to_str()
            .unwrap()
            .contains("__oailb")
    );
    for forbidden in [
        "http://chatgpt.com/backend-api",
        "https://api.openai.com/",
        "https://auth.openai.com/",
        "https://chatgpt.com.evil.test/",
        "https://evilchatgpt.com/",
        "https://sub.chatgpt.com/",
    ] {
        assert!(
            jar.cookies(&Url::parse(forbidden).unwrap()).is_none(),
            "{forbidden}"
        );
    }
    store(
        &jar,
        url,
        &["__oailb=gone; Max-Age=0; Path=/backend-api; Secure"],
    );
    assert!(
        !jar.cookies(&Url::parse(url).unwrap())
            .unwrap()
            .to_str()
            .unwrap()
            .contains("__oailb")
    );
    for forbidden in ["http://chatgpt.com/", "https://example.com/"] {
        store(
            &jar,
            forbidden,
            &["__cfruid=bad; Domain=chatgpt.com; Path=/"],
        );
    }
    assert!(
        !jar.cookies(&Url::parse(url).unwrap())
            .unwrap()
            .to_str()
            .unwrap()
            .contains("bad")
    );
}

#[test]
fn accounts_and_refreshes_have_correct_jar_ownership() {
    let transports = AccountTransports::default();
    let first = transports.get(&account("one"), Default::default());
    let mut refreshed = account("one");
    refreshed["access_token"] = "rotated".into();
    assert!(Arc::ptr_eq(
        &first,
        &transports.get(&refreshed, Default::default())
    ));
    let second = transports.get(&account("two"), Default::default());
    assert!(!Arc::ptr_eq(&first, &second));
    let url = "https://chatgpt.com/";
    store(&first.cookies, url, &["__oailb=one; Secure; Path=/"]);
    assert!(second.cookies.cookies(&Url::parse(url).unwrap()).is_none());
    for options in [
        TransportOptions {
            use_system_proxy: true,
            ..Default::default()
        },
        TransportOptions {
            profile: crate::codex_identity::CodexProfile::Tui,
            ..Default::default()
        },
    ] {
        let changed = transports.get(&account("one"), options);
        assert!(!Arc::ptr_eq(&first, &changed));
        assert!(changed.cookies.cookies(&Url::parse(url).unwrap()).is_none());
    }
    assert!(!Arc::ptr_eq(
        &transports.get(&serde_json::Value::Null, Default::default()),
        &transports.get(&serde_json::Value::Null, Default::default())
    ));
    for index in 0..MAX_ACCOUNTS + 1 {
        transports.get(&account(&index.to_string()), Default::default());
    }
    assert_eq!(transports.0.lock().unwrap().len(), MAX_ACCOUNTS);
}

#[test]
fn websocket_and_http_share_cookies_but_explicit_header_wins() {
    let credential = account("cookie-sharing-test");
    let transport = account_transport(&credential, Default::default());
    let uri: http::Uri = "wss://chatgpt.com/backend-api/codex/responses"
        .parse()
        .unwrap();
    let mut request = http::Request::get(uri).body(()).unwrap();
    attach_websocket_cookies(&mut request, &credential, Default::default());
    let cookies = request.extensions().get::<WebSocketCookies>().unwrap();
    store(
        &transport.cookies,
        "https://chatgpt.com/backend-api/codex/responses",
        &["__oailb=http; Secure; Path=/"],
    );
    let mut headers = HeaderMap::new();
    cookies.apply(&mut headers);
    assert_eq!(headers[COOKIE], "__oailb=http");
    headers.insert(COOKIE, HeaderValue::from_static("admin=explicit"));
    cookies.apply(&mut headers);
    assert_eq!(headers[COOKIE], "admin=explicit");
    let mut response = HeaderMap::new();
    response.append(
        SET_COOKIE,
        HeaderValue::from_static("__oailb=ws; Path=/; Secure"),
    );
    response.append(
        SET_COOKIE,
        HeaderValue::from_static("session=secret; Path=/; Secure"),
    );
    cookies.store(&response);
    assert_eq!(
        transport.cookies.cookies(&cookies.url).unwrap(),
        "__oailb=ws"
    );
    assert!(!crate::supplier::subscription_response_headers(&response).contains_key(SET_COOKIE));
    for url in [
        "ws://chatgpt.com/",
        "wss://api.openai.com/",
        "wss://chatgpt.com.evil.test/",
    ] {
        let mut request = http::Request::get(url).body(()).unwrap();
        attach_websocket_cookies(&mut request, &credential, Default::default());
        assert!(request.extensions().get::<WebSocketCookies>().is_none());
    }
}
