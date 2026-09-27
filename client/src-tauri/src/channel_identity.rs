use crate::{
    detection::subscription_account_identity,
    model::{ChannelConfig, ChannelExecutorLocator},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ChannelDuplicateCandidate {
    pub(crate) channel_id: String,
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelConnectionIdentity {
    Subscription {
        provider: String,
        account: String,
    },
    Http {
        credential: String,
        endpoints: BTreeSet<String>,
    },
}

pub(crate) fn duplicate_channel_candidates(
    channels: &[ChannelConfig],
    candidate: &ChannelConfig,
) -> Vec<ChannelDuplicateCandidate> {
    let Some(candidate_identity) = channel_connection_identity(candidate) else {
        return Vec::new();
    };
    channels
        .iter()
        .filter(|existing| existing.id != candidate.id)
        .filter_map(|existing| {
            let existing_identity = channel_connection_identity(existing)?;
            connection_identities_match(&candidate_identity, &existing_identity).then(|| {
                ChannelDuplicateCandidate {
                    channel_id: existing.id.clone(),
                    name: existing.name.clone(),
                }
            })
        })
        .collect()
}

fn channel_connection_identity(channel: &ChannelConfig) -> Option<ChannelConnectionIdentity> {
    match &channel.v2.executor {
        ChannelExecutorLocator::RetainedSubscription { provider } => {
            let provider = provider.trim().to_ascii_lowercase();
            let account = subscription_account_identity(channel).or_else(|| {
                let label = channel
                    .subscription
                    .account_label
                    .trim()
                    .to_ascii_lowercase();
                if !label.is_empty() && label != "unknown account" {
                    Some(format!("label:{label}"))
                } else {
                    normalized_credential_path(&channel.v2.credential_ref)
                        .map(|path| format!("path:{path}"))
                }
            })?;
            Some(ChannelConnectionIdentity::Subscription { provider, account })
        }
        ChannelExecutorLocator::HttpSurface => {
            let endpoints = channel
                .surface_bindings
                .iter()
                .filter_map(|binding| normalize_http_endpoint(&binding.base_url))
                .chain(normalize_http_endpoint(&channel.upstream_base_url))
                .collect::<BTreeSet<_>>();
            if endpoints.is_empty() {
                return None;
            }
            Some(ChannelConnectionIdentity::Http {
                credential: channel.v2.credential_ref.trim().to_string(),
                endpoints,
            })
        }
    }
}

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

fn connection_identities_match(
    left: &ChannelConnectionIdentity,
    right: &ChannelConnectionIdentity,
) -> bool {
    match (left, right) {
        (
            ChannelConnectionIdentity::Subscription {
                provider: left_provider,
                account: left_account,
            },
            ChannelConnectionIdentity::Subscription {
                provider: right_provider,
                account: right_account,
            },
        ) => left_provider == right_provider && left_account == right_account,
        (
            ChannelConnectionIdentity::Http {
                credential: left_credential,
                endpoints: left_endpoints,
            },
            ChannelConnectionIdentity::Http {
                credential: right_credential,
                endpoints: right_endpoints,
            },
        ) => {
            left_credential == right_credential
                && left_endpoints
                    .iter()
                    .any(|endpoint| right_endpoints.contains(endpoint))
        }
        _ => false,
    }
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

fn normalized_credential_path(value: &str) -> Option<String> {
    let path = Path::new(value.trim());
    if value.trim().is_empty() {
        return None;
    }
    let normalized = path.to_string_lossy().replace('/', "\\");
    #[cfg(target_os = "windows")]
    let normalized = normalized.to_ascii_lowercase();
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{default_config, source_driver_surface_bindings},
        detection::prepare_subscription_token_and_import_in_dir,
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
    fn http_identity_matches_across_driver_categories() {
        let left = ChannelConnectionIdentity::Http {
            credential: "same-key".to_string(),
            endpoints: BTreeSet::from(["https://api.example.com/v1".to_string()]),
        };
        let right = ChannelConnectionIdentity::Http {
            credential: "same-key".to_string(),
            endpoints: BTreeSet::from([
                "https://api.example.com/v1".to_string(),
                "https://other.example.com/v1".to_string(),
            ]),
        };
        assert!(connection_identities_match(&left, &right));
    }

    #[test]
    fn http_identity_keeps_credentials_and_paths_distinct() {
        let baseline = ChannelConnectionIdentity::Http {
            credential: "first-key".to_string(),
            endpoints: BTreeSet::from(["https://api.example.com/v1".to_string()]),
        };
        let other_credential = ChannelConnectionIdentity::Http {
            credential: "second-key".to_string(),
            endpoints: BTreeSet::from(["https://api.example.com/v1".to_string()]),
        };
        let other_path = ChannelConnectionIdentity::Http {
            credential: "first-key".to_string(),
            endpoints: BTreeSet::from(["https://api.example.com/v2".to_string()]),
        };
        assert!(!connection_identities_match(&baseline, &other_credential));
        assert!(!connection_identities_match(&baseline, &other_path));
    }

    #[test]
    fn endpoint_normalization_ignores_cosmetic_url_differences() {
        assert_eq!(
            normalize_http_endpoint(" HTTPS://API.Example.com/v1/?ignored=yes#fragment "),
            Some("https://api.example.com/v1".to_string())
        );
    }

    #[test]
    fn duplicate_candidates_match_same_api_connection_across_presets() {
        let existing = http_channel(
            "official",
            SourceDriverId::OpenAiApi,
            "https://api.example.com/v1",
            "same-key",
        );
        let candidate = http_channel(
            "custom",
            SourceDriverId::CustomEndpoint,
            "https://api.example.com/v1/",
            "same-key",
        );

        assert_eq!(
            duplicate_channel_candidates(&[existing], &candidate),
            vec![ChannelDuplicateCandidate {
                channel_id: "official".to_string(),
                name: "official".to_string(),
            }]
        );
    }

    #[test]
    fn duplicate_candidates_match_subscription_account_across_fresh_tokens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = prepare_subscription_token_and_import_in_dir(
            "claude",
            serde_json::json!({
                "type": "claude",
                "access_token": "first-access",
                "refresh_token": "first-refresh",
                "account": {"uuid": "same-account"},
            }),
            dir.path(),
        )
        .expect("first prepared channel");
        let second = prepare_subscription_token_and_import_in_dir(
            "claude",
            serde_json::json!({
                "type": "claude",
                "access_token": "second-access",
                "refresh_token": "second-refresh",
                "account": {"uuid": "same-account"},
            }),
            dir.path(),
        )
        .expect("second prepared channel");

        assert_ne!(
            first.id, second.id,
            "channel instances must not reuse the subscription account identity"
        );
        assert_eq!(
            duplicate_channel_candidates(&[first.clone()], &second),
            vec![ChannelDuplicateCandidate {
                channel_id: first.id,
                name: first.name,
            }]
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
