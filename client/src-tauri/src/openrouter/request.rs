//! OpenRouter-only request options. Never change the caller's routing policy,
//! model fallback list, tool state or prompt/cache prefix.
use crate::{model::ChannelConfig, source_driver::SourceDriverId, surface::ApiOperation};
use reqwest::header::{HeaderMap, HeaderValue};

pub(crate) fn prepare_headers(
    channel: &ChannelConfig,
    operation: ApiOperation,
    headers: &mut HeaderMap,
    body: &[u8],
    cache_identity: Option<&str>,
) {
    if channel.source_driver() != SourceDriverId::Openrouter
        || !matches!(
            operation,
            ApiOperation::ChatCompletions | ApiOperation::Responses | ApiOperation::Messages
        )
    {
        return;
    }
    headers
        .entry("x-openrouter-metadata")
        .or_insert(HeaderValue::from_static("enabled"));
    // An explicit caller session/cache key retains its meaning. The platform's
    // already user/model-scoped identity is only a missing-session fallback.
    if headers.contains_key("x-session-id")
        || crate::supplier::top_level_json_string(body, "session_id").is_some()
        || crate::supplier::top_level_json_string(body, "prompt_cache_key").is_some()
    {
        return;
    }
    if let Some(identity) = cache_identity.filter(|s| !s.is_empty() && s.len() <= 256) {
        if let Ok(value) = HeaderValue::from_str(identity) {
            headers.insert("x-session-id", value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_openrouter_inference_gets_missing_session_and_diagnostics() {
        for driver in crate::source_driver::ALL_SOURCE_DRIVER_IDS {
            let mut channel =
                crate::channel_from_supplier("test".into(), &crate::default_supplier_config());
            channel.set_source_driver(driver);
            for operation in [
                ApiOperation::ListModels,
                ApiOperation::Responses,
                ApiOperation::Messages,
                ApiOperation::ChatCompletions,
            ] {
                let mut headers = HeaderMap::new();
                prepare_headers(
                    &channel,
                    operation,
                    &mut headers,
                    br#"{"model":"x"}"#,
                    Some("c1_user_model_session"),
                );
                let expected =
                    driver == SourceDriverId::Openrouter && operation != ApiOperation::ListModels;
                assert_eq!(headers.contains_key("x-openrouter-metadata"), expected);
                assert_eq!(headers.contains_key("x-session-id"), expected);
            }
        }
    }

    #[test]
    fn explicit_session_cache_and_metadata_preferences_are_preserved() {
        let mut channel =
            crate::channel_from_supplier("test".into(), &crate::default_supplier_config());
        channel.set_source_driver(SourceDriverId::Openrouter);
        for body in [
            br#"{"session_id":"caller"}"#.as_slice(),
            br#"{"prompt_cache_key":"caller"}"#,
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-openrouter-metadata",
                HeaderValue::from_static("disabled"),
            );
            prepare_headers(
                &channel,
                ApiOperation::Responses,
                &mut headers,
                body,
                Some("scoped"),
            );
            assert!(!headers.contains_key("x-session-id"));
            assert_eq!(headers["x-openrouter-metadata"], "disabled");
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static("caller"));
        prepare_headers(
            &channel,
            ApiOperation::Messages,
            &mut headers,
            b"{}",
            Some("scoped"),
        );
        assert_eq!(headers["x-session-id"], "caller");
    }
}
