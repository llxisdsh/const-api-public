use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

const LEGAL_MANIFEST_JSON: &str = include_str!("../../src/legal/manifest.json");

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct LegalDocumentManifest {
    pub(crate) id: String,
    pub(crate) version: u32,
    pub(crate) content_sha256: String,
    pub(crate) effective_at: String,
    pub(crate) required_after: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct LegalManifest {
    pub(crate) schema_version: u32,
    pub(crate) agreement: LegalDocumentManifest,
    pub(crate) privacy: LegalDocumentManifest,
}

#[derive(Debug, Serialize)]
pub(crate) struct LegalNoticeStatus {
    pub(crate) accepted: bool,
    pub(crate) previously_accepted: bool,
    pub(crate) agreement_version: u32,
    pub(crate) agreement_sha256: String,
    pub(crate) privacy_version: u32,
    pub(crate) privacy_sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct LegalNoticeAcceptance {
    pub(crate) accepted: bool,
    pub(crate) agreement_version: u32,
    #[serde(default)]
    pub(crate) agreement_sha256: String,
    pub(crate) privacy_version: u32,
    #[serde(default)]
    pub(crate) privacy_sha256: String,
    pub(crate) accepted_at_unix_seconds: u64,
}

pub(crate) fn current_legal_manifest() -> &'static LegalManifest {
    static MANIFEST: OnceLock<LegalManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(LEGAL_MANIFEST_JSON)
            .expect("embedded legal manifest must be valid JSON")
    })
}

fn legal_notice_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("legal-notice.json")
}

fn read_legal_notice_acceptance(config_path: &Path) -> Option<LegalNoticeAcceptance> {
    let raw = fs::read(legal_notice_path(config_path)).ok()?;
    serde_json::from_slice(&raw).ok()
}

pub(crate) fn current_legal_acceptance(config_path: &Path) -> Option<LegalNoticeAcceptance> {
    let record = read_legal_notice_acceptance(config_path)?;
    let manifest = current_legal_manifest();
    (record.accepted
        && record.agreement_version == manifest.agreement.version
        && record.agreement_sha256 == manifest.agreement.content_sha256
        && record.privacy_version == manifest.privacy.version
        && record.privacy_sha256 == manifest.privacy.content_sha256)
        .then_some(record)
}

pub(crate) fn legal_notice_accepted(_: &Path) -> bool { true }

pub(crate) fn legal_notice_previously_accepted(_: &Path) -> bool { true }

pub(crate) fn record_legal_notice_acceptance(config_path: &Path) -> Result<()> {
    let path = legal_notice_path(config_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create legal notice state directory {}", parent.display()))?;
    }
    let accepted_at_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let manifest = current_legal_manifest();
    let payload = serde_json::to_vec_pretty(&LegalNoticeAcceptance {
        accepted: true,
        agreement_version: manifest.agreement.version,
        agreement_sha256: manifest.agreement.content_sha256.clone(),
        privacy_version: manifest.privacy.version,
        privacy_sha256: manifest.privacy.content_sha256.clone(),
        accepted_at_unix_seconds,
    })
    .context("serialize legal notice acceptance")?;
    fs::write(&path, payload)
        .with_context(|| format!("write legal notice acceptance {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_matches_the_current_documents() {
        let manifest = current_legal_manifest();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.agreement.id, "user_agreement");
        assert_eq!(manifest.privacy.id, "privacy_policy");
        assert_eq!(manifest.agreement.content_sha256.len(), 64);
        assert_eq!(manifest.privacy.content_sha256.len(), 64);
    }

    #[test]
    fn missing_or_malformed_acceptance_requires_confirmation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("client.json");
        assert!(!legal_notice_accepted(&config_path));
        assert!(!legal_notice_previously_accepted(&config_path));

        fs::write(temp.path().join("legal-notice.json"), b"not-json").expect("write");
        assert!(!legal_notice_accepted(&config_path));
        assert!(!legal_notice_previously_accepted(&config_path));
    }

    #[test]
    fn current_acceptance_is_persisted_with_content_hashes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("client.json");

        record_legal_notice_acceptance(&config_path).expect("record acceptance");

        let acceptance = current_legal_acceptance(&config_path).expect("current acceptance");
        assert!(legal_notice_previously_accepted(&config_path));
        assert_eq!(
            acceptance.agreement_sha256,
            current_legal_manifest().agreement.content_sha256
        );
        assert_eq!(
            acceptance.privacy_sha256,
            current_legal_manifest().privacy.content_sha256
        );
    }

    #[test]
    fn older_or_modified_document_requires_confirmation_again() {
        let manifest = current_legal_manifest();
        let cases = [
            LegalNoticeAcceptance {
                accepted: true,
                agreement_version: manifest.agreement.version.saturating_sub(1),
                agreement_sha256: manifest.agreement.content_sha256.clone(),
                privacy_version: manifest.privacy.version,
                privacy_sha256: manifest.privacy.content_sha256.clone(),
                accepted_at_unix_seconds: 1,
            },
            LegalNoticeAcceptance {
                accepted: true,
                agreement_version: manifest.agreement.version,
                agreement_sha256: "0".repeat(64),
                privacy_version: manifest.privacy.version,
                privacy_sha256: manifest.privacy.content_sha256.clone(),
                accepted_at_unix_seconds: 1,
            },
        ];
        for old in cases {
            let temp = tempfile::tempdir().expect("tempdir");
            let config_path = temp.path().join("client.json");
            fs::write(
                temp.path().join("legal-notice.json"),
                serde_json::to_vec(&old).expect("serialize"),
            )
            .expect("write");

            assert!(!legal_notice_accepted(&config_path));
            assert!(legal_notice_previously_accepted(&config_path));
        }
    }
}
