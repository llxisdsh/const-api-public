//! Local builds use bundled model metadata and never discover official services or updates.
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Debug)]
pub(crate) struct ActiveReleaseSources;
#[derive(Clone, Debug)]
pub(crate) struct CatalogSourceLocation {
    pub(crate) manifest_url: String,
    pub(crate) component_base_url: String,
}
#[derive(Debug)]
pub(crate) struct ReleaseSourceRefresh {
    pub(crate) active: ActiveReleaseSources,
    pub(crate) changed: bool,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseSourceStatus {
    sequence: i64,
    delivery_source: String,
    bootstrap_sources: Vec<String>,
    source_ids: Vec<String>,
    endpoint_sources: Vec<String>,
    catalog_sources: Vec<String>,
    catalog_component_sources: Vec<String>,
    update_sources: Vec<String>,
    server_update_sources: Vec<String>,
}
impl ActiveReleaseSources {
    pub(crate) fn sequence(&self) -> i64 {
        0
    }
    pub(crate) fn delivery_source(&self) -> &str {
        "local_only"
    }
    pub(crate) fn endpoint_registry_urls(&self) -> Vec<String> {
        Vec::new()
    }
    pub(crate) fn catalog_registry_urls(&self) -> Vec<String> {
        Vec::new()
    }
    pub(crate) fn catalog_sources(&self) -> Vec<CatalogSourceLocation> {
        Vec::new()
    }
    pub(crate) fn client_update_urls(&self) -> Vec<String> {
        Vec::new()
    }
    pub(crate) fn status(&self) -> ReleaseSourceStatus {
        ReleaseSourceStatus {
            sequence: 0,
            delivery_source: "local_only".into(),
            bootstrap_sources: vec![],
            source_ids: vec![],
            endpoint_sources: vec![],
            catalog_sources: vec![],
            catalog_component_sources: vec![],
            update_sources: vec![],
            server_update_sources: vec![],
        }
    }
}
pub(crate) fn bundled_release_sources() -> Result<ActiveReleaseSources> {
    Ok(ActiveReleaseSources)
}
pub(crate) fn trusted_release_sources(_: &Path) -> Result<ActiveReleaseSources> {
    bundled_release_sources()
}
pub(crate) async fn refresh_release_sources(
    _: &reqwest::Client,
    _: &Path,
) -> Result<ReleaseSourceRefresh> {
    Ok(ReleaseSourceRefresh {
        active: ActiveReleaseSources,
        changed: false,
    })
}
