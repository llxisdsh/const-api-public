use crate::{
    account::select_account_endpoint,
    client_toasts::{clear_platform_announcement, record_platform_announcement},
    config::load_config_from_path,
    model::{AppState, Endpoint},
};
use anyhow::{Context, Result, anyhow};
use reqwest::{
    StatusCode,
    header::{ACCEPT, ETAG, HeaderMap, HeaderValue, IF_NONE_MATCH},
};
use serde::Deserialize;
use std::time::Duration;

const ANNOUNCEMENT_PATH: &str = "/api/announcements/current";
const ANNOUNCEMENT_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const ANNOUNCEMENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const ANNOUNCEMENT_VERSION_LIMIT: usize = 160;
const ANNOUNCEMENT_TITLE_LIMIT: usize = 120;
const ANNOUNCEMENT_CONTENT_LIMIT: usize = 4000;

#[derive(Debug, Deserialize)]
struct AnnouncementEnvelope {
    announcement: Option<PlatformAnnouncement>,
}

#[derive(Debug, Deserialize)]
struct PlatformAnnouncement {
    version: String,
    title: String,
    content: String,
}

enum AnnouncementFetch {
    Current {
        announcement: Option<PlatformAnnouncement>,
        etag: Option<String>,
    },
    NotModified,
    Unsupported,
}

fn announcement_endpoint(config: &crate::model::ClientConfig) -> Result<Endpoint> {
    let required_platform = config.account_platform_id.trim();
    let (_, endpoint) = select_account_endpoint(
        config,
        (!required_platform.is_empty()).then_some(required_platform),
    )?;
    Ok(endpoint.clone())
}

fn normalize_announcement(announcement: PlatformAnnouncement) -> Option<PlatformAnnouncement> {
    let version = announcement.version.trim().to_string();
    let title = announcement.title.trim().to_string();
    let content = announcement
        .content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string();
    if version.is_empty()
        || version.chars().count() > ANNOUNCEMENT_VERSION_LIMIT
        || title.is_empty()
        || title.chars().count() > ANNOUNCEMENT_TITLE_LIMIT
        || content.is_empty()
        || content.chars().count() > ANNOUNCEMENT_CONTENT_LIMIT
    {
        return None;
    }
    Some(PlatformAnnouncement {
        version,
        title,
        content,
    })
}

async fn fetch_announcement(
    state: &AppState,
    endpoint: &Endpoint,
    etag: Option<&str>,
) -> Result<AnnouncementFetch> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if let Some(etag) = etag {
        headers.insert(
            IF_NONE_MATCH,
            HeaderValue::from_str(etag).context("invalid announcement ETag")?,
        );
    }
    let response = tokio::time::timeout(
        ANNOUNCEMENT_REQUEST_TIMEOUT,
        state
            .platform_transport
            .get(endpoint, ANNOUNCEMENT_PATH, headers),
    )
    .await
    .map_err(|_| anyhow!("announcement request timed out"))??;
    match response.status() {
        StatusCode::NOT_MODIFIED => return Ok(AnnouncementFetch::NotModified),
        StatusCode::NOT_FOUND => return Ok(AnnouncementFetch::Unsupported),
        status if !status.is_success() => {
            return Err(anyhow!("announcement server returned HTTP {status}"));
        }
        _ => {}
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let envelope = response
        .json::<AnnouncementEnvelope>()
        .await
        .context("decode announcement response")?;
    let announcement = match envelope.announcement {
        Some(announcement) => Some(
            normalize_announcement(announcement)
                .ok_or_else(|| anyhow!("announcement response is invalid"))?,
        ),
        None => None,
    };
    Ok(AnnouncementFetch::Current { announcement, etag })
}

pub(crate) async fn run_platform_announcement_refresh_loop(state: AppState) {
    let mut active_endpoint = String::new();
    let mut unsupported_endpoint = String::new();
    let mut etag: Option<String> = None;
    let mut delivered_version = String::new();

    loop {
        let endpoint = load_config_from_path(&state.config_path)
            .ok()
            .and_then(|config| announcement_endpoint(&config).ok());
        if let Some(endpoint) = endpoint {
            let endpoint_key = format!(
                "{}|{}",
                endpoint.base_url.trim(),
                endpoint.supplier_quic_url.trim()
            );
            if endpoint_key != active_endpoint {
                if !delivered_version.is_empty() {
                    clear_platform_announcement(delivered_version.clone());
                }
                active_endpoint = endpoint_key.clone();
                unsupported_endpoint.clear();
                etag = None;
                delivered_version.clear();
            }
            if unsupported_endpoint != endpoint_key {
                match fetch_announcement(&state, &endpoint, etag.as_deref()).await {
                    Ok(AnnouncementFetch::Current {
                        announcement,
                        etag: next_etag,
                    }) => {
                        etag = next_etag;
                        if let Some(announcement) = announcement {
                            if announcement.version != delivered_version {
                                delivered_version = announcement.version.clone();
                                record_platform_announcement(
                                    announcement.version,
                                    announcement.title,
                                    announcement.content,
                                );
                            }
                        } else {
                            if !delivered_version.is_empty() {
                                clear_platform_announcement(delivered_version.clone());
                            }
                            delivered_version.clear();
                        }
                    }
                    Ok(AnnouncementFetch::NotModified) => {}
                    Ok(AnnouncementFetch::Unsupported) => {
                        if !delivered_version.is_empty() {
                            clear_platform_announcement(delivered_version.clone());
                            delivered_version.clear();
                        }
                        unsupported_endpoint = endpoint_key;
                    }
                    Err(error) => {
                        log::debug!("[const-api][announcement] refresh deferred: {error:#}");
                    }
                }
            }
        } else {
            if !delivered_version.is_empty() {
                clear_platform_announcement(delivered_version.clone());
            }
            active_endpoint.clear();
            unsupported_endpoint.clear();
            etag = None;
            delivered_version.clear();
        }
        tokio::time::sleep(ANNOUNCEMENT_REFRESH_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_or_oversized_announcement_payloads() {
        assert!(
            normalize_announcement(PlatformAnnouncement {
                version: "announcement_1".to_string(),
                title: "维护通知".to_string(),
                content: "服务已经恢复。".to_string(),
            })
            .is_some()
        );
        assert!(
            normalize_announcement(PlatformAnnouncement {
                version: "announcement_2".to_string(),
                title: String::new(),
                content: "content".to_string(),
            })
            .is_none()
        );
        assert!(
            normalize_announcement(PlatformAnnouncement {
                version: "announcement_3".to_string(),
                title: "title".to_string(),
                content: "x".repeat(ANNOUNCEMENT_CONTENT_LIMIT + 1),
            })
            .is_none()
        );
    }
}
