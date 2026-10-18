use crate::{
    detection::subscription_account_identity,
    model::{ChannelConfig, ChannelExecutorLocator},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) fn channel_cache_domain_id(
    channel: &ChannelConfig,
    server_scope: &str,
) -> Option<String> {
    let identity = match &channel.v2.executor {
        ChannelExecutorLocator::RetainedSubscription { provider } => {
            let account = subscription_account_identity(channel)?;
            format!(
                "subscription\0{}\0{}",
                provider.trim().to_ascii_lowercase(),
                account
            )
        }
        ChannelExecutorLocator::HttpSurface => {
            let credential = [
                channel.v2.credential_ref.trim(),
                channel.upstream_api_key.trim(),
            ]
            .into_iter()
            .find(|value| !value.is_empty())?;
            let endpoints = channel
                .surface_bindings
                .iter()
                .filter_map(|binding| normalize_http_endpoint(&binding.base_url))
                .chain(normalize_http_endpoint(&channel.upstream_base_url))
                .collect::<BTreeSet<_>>();
            if endpoints.is_empty() {
                return None;
            }
            format!(
                "http\0{}\0{}",
                credential,
                endpoints.into_iter().collect::<Vec<_>>().join("\0")
            )
        }
    };
    let scope = server_scope.trim();
    if scope.is_empty() {
        return None;
    }
    let digest =
        Sha256::digest(format!("const-api-cache-domain-v1\0{scope}\0{identity}").as_bytes());
    Some(format!("cache-{}", hex::encode(digest)))
}

fn normalize_http_endpoint(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    let normalized_path = url.path().trim_end_matches('/').to_string();
    url.set_path(if normalized_path.is_empty() {
        "/"
    } else {
        &normalized_path
    });
    let mut normalized = url.to_string();
    while normalized.ends_with('/') {
        normalized.pop();
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{default_config, source_driver_surface_bindings},
        source_driver::SourceDriverId,
    };

    fn http_channel(
        id: &str,
        source_driver: SourceDriverId,
        base_url: &str,
        credential: &str,
    ) -> ChannelConfig {
        let mut channel = default_config().channels.remove(0);
        channel.id = id.to_string();
        channel.name = id.to_string();
        channel.set_source_driver(source_driver);
        channel.v2.credential_ref = credential.to_string();
        channel.upstream_api_key = credential.to_string();
        channel.upstream_base_url = base_url.to_string();
        channel.surface_bindings = source_driver_surface_bindings(source_driver, base_url);
        channel
    }

    #[test]
    fn endpoint_normalization_ignores_cosmetic_url_differences() {
        assert_eq!(
            normalize_http_endpoint(" HTTPS://API.Example.com/v1/?ignored=yes#fragment "),
            Some("https://api.example.com/v1".to_string())
        );
    }

    #[test]
    fn cache_domain_is_stable_and_never_contains_http_credentials() {
        let first = http_channel(
            "first",
            SourceDriverId::OpenAiApi,
            "https://api.example.com/v1",
            "super-secret-key",
        );
        let second = http_channel(
            "second",
            SourceDriverId::CustomEndpoint,
            "https://api.example.com/v1/",
            "super-secret-key",
        );
        let first_id = channel_cache_domain_id(&first, "https://platform.example.com").unwrap();
        let second_id = channel_cache_domain_id(&second, "https://platform.example.com").unwrap();

        assert_eq!(first_id, second_id);
        assert!(first_id.starts_with("cache-"));
        assert!(!first_id.contains("super-secret-key"));
        assert_ne!(
            first_id,
            channel_cache_domain_id(&first, "https://other.example.com").unwrap()
        );
    }
}
