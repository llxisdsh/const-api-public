//! In-memory HTTP validators for small public, signed control-plane documents.
//! The caller revalidates signatures, expiry and rollback bounds on EVERY use,
//! including 304 and concurrent reads. This never caches model/API responses.

use anyhow::{Context, Result, anyhow};
use reqwest::{
    RequestBuilder, StatusCode, Url,
    header::{ETAG, HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

const MAX_SOURCES: usize = 16;
const MAX_CACHED_BODY_BYTES: usize = 512 * 1024;

#[cfg(test)]
#[path = "conditional_document/tests.rs"]
pub(crate) mod tests;

struct Document {
    raw: Vec<u8>,
    etag: Option<HeaderValue>,
    last_modified: Option<HeaderValue>,
    final_url: Url,
    checked_at: Instant,
}

type DocumentSlot = Arc<tokio::sync::Mutex<Option<Document>>>;

#[derive(Default)]
pub(crate) struct ConditionalDocumentCache {
    sources: Mutex<VecDeque<(String, DocumentSlot)>>,
}

pub(crate) fn public_document_cache() -> &'static ConditionalDocumentCache {
    static CACHE: OnceLock<ConditionalDocumentCache> = OnceLock::new();
    CACHE.get_or_init(ConditionalDocumentCache::default)
}

impl ConditionalDocumentCache {
    fn slot(&self, key: &str) -> DocumentSlot {
        let mut sources = self
            .sources
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = sources.iter().position(|(source, _)| source == key) {
            if let Some(entry) = sources.remove(index) {
                let slot = entry.1.clone();
                sources.push_back(entry);
                return slot;
            }
        }
        if sources.len() >= MAX_SOURCES {
            if let Some(index) = sources
                .iter()
                .position(|(_, slot)| Arc::strong_count(slot) == 1)
            {
                sources.remove(index);
            }
        }
        let slot = Arc::new(tokio::sync::Mutex::new(None));
        // Never evict an in-flight source and create a second writer for it.
        if sources.len() < MAX_SOURCES {
            sources.push_back((key.to_string(), slot.clone()));
        }
        slot
    }

    pub(crate) async fn fetch<T>(
        &self,
        key: &str,
        request: RequestBuilder,
        max_body_bytes: Option<u64>,
        validate: impl Fn(&[u8], &Url) -> Result<T>,
    ) -> Result<T> {
        let started_at = Instant::now();
        let slot = self.slot(key);
        let mut cached = slot.lock().await;
        // Join only genuinely overlapping reads, not a new freshness interval.
        if let Some(document) = cached
            .as_ref()
            .filter(|entry| entry.checked_at >= started_at)
        {
            if max_body_bytes.is_none_or(|limit| document.raw.len() as u64 <= limit) {
                if let Ok(value) = validate(&document.raw, &document.final_url) {
                    return Ok(value);
                }
            }
            *cached = None;
        }
        for attempt in 0..2 {
            let mut next = request
                .try_clone()
                .context("public document request is not replayable")?;
            if let Some(document) = cached.as_ref() {
                if let Some(etag) = &document.etag {
                    next = next.header(IF_NONE_MATCH, etag.clone());
                }
                if let Some(modified) = &document.last_modified {
                    next = next.header(IF_MODIFIED_SINCE, modified.clone());
                }
            }
            let mut response = next.send().await.context("fetch public document")?;
            if response.status() == StatusCode::NOT_MODIFIED {
                if let Some(document) = cached.as_mut() {
                    let has_validator = document.etag.is_some() || document.last_modified.is_some();
                    let matches = has_validator
                        && document.etag.as_ref().is_none_or(|etag| {
                            response
                                .headers()
                                .get(ETAG)
                                .is_none_or(|returned| returned == etag)
                        })
                        && max_body_bytes.is_none_or(|limit| document.raw.len() as u64 <= limit);
                    // Check the validator before calling a verifier that may
                    // also persist the verified last-known-good document.
                    if matches {
                        if let Ok(value) = validate(&document.raw, response.url()) {
                            document.checked_at = Instant::now();
                            document.final_url = response.url().clone();
                            if let Some(etag) = response.headers().get(ETAG) {
                                document.etag = Some(etag.clone());
                            }
                            if let Some(modified) = response.headers().get(LAST_MODIFIED) {
                                document.last_modified = Some(modified.clone());
                            }
                            return Ok(value);
                        }
                    }
                }
                *cached = None;
                if attempt == 0 {
                    // Missing, expired or no-longer-trusted cache: one ordinary
                    // GET can repair it. Existing mirror fallback handles failure.
                    continue;
                }
                return Err(anyhow!(
                    "public document returned 304 without a valid cache"
                ));
            }
            if !response.status().is_success() {
                return Err(anyhow!(
                    "public document returned HTTP {}",
                    response.status()
                ));
            }
            if max_body_bytes
                .is_some_and(|limit| response.content_length().is_some_and(|size| size > limit))
            {
                return Err(anyhow!("public document is too large"));
            }
            let etag = response.headers().get(ETAG).cloned();
            let last_modified = response.headers().get(LAST_MODIFIED).cloned();
            let final_url = response.url().clone();
            let mut raw = Vec::new();
            while let Some(chunk) = response.chunk().await.context("read public document")? {
                if max_body_bytes.is_some_and(|limit| raw.len() as u64 + chunk.len() as u64 > limit)
                {
                    return Err(anyhow!("public document is too large"));
                }
                raw.extend_from_slice(&chunk);
            }
            let value = validate(&raw, &final_url)?;
            *cached = (raw.len() <= MAX_CACHED_BODY_BYTES).then(|| Document {
                raw,
                etag,
                last_modified,
                final_url,
                checked_at: Instant::now(),
            });
            return Ok(value);
        }
        Err(anyhow!("public document cache repair exhausted"))
    }
}
