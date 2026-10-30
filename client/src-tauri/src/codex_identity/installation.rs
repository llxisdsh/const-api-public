//! One persistent supplier installation identity per channel. Caller devices,
//! sessions, token refreshes and sockets never determine this identity.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{Read, Seek, Write},
    path::Path,
    sync::{Mutex, OnceLock},
};

pub(crate) fn installation_id(channel_id: &str) -> Result<String> {
    anyhow::ensure!(
        !channel_id.trim().is_empty(),
        "Codex installation identity requires a channel ID"
    );
    static IDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let mut ids = IDS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("Codex installation identity cache lock poisoned"))?;
    if let Some(id) = ids.get(channel_id) {
        return Ok(id.clone());
    }
    // Ordinary unit tests must not read or create a production identity.
    #[cfg(test)]
    if std::env::var("CONST_CAPACITY_LIVE").as_deref() != Ok("1") {
        let id = uuid::Uuid::new_v4().to_string();
        ids.insert(channel_id.to_owned(), id.clone());
        return Ok(id);
    }
    let config = crate::config::default_config_path();
    let directory = config
        .parent()
        .context("client configuration has no parent directory")?;
    let id = resolve(directory, channel_id)?;
    ids.insert(channel_id.to_owned(), id.clone());
    Ok(id)
}

fn resolve(directory: &Path, channel_id: &str) -> Result<String> {
    let directory = directory.join("codex-installation-ids");
    std::fs::create_dir_all(&directory)?;
    // Persisted channel IDs are opaque; never interpret them as path segments.
    let filename = hex::encode(Sha256::digest(channel_id.as_bytes()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(filename))
        .context("open supplier Codex installation identity")?;
    file.lock()
        .context("lock supplier Codex installation identity")?;
    let mut existing = String::new();
    file.read_to_string(&mut existing)?;
    if !existing.trim().is_empty() {
        return Ok(uuid::Uuid::parse_str(existing.trim())
            .context("invalid persisted supplier Codex installation identity")?
            .to_string());
    }
    let id = uuid::Uuid::new_v4().to_string();
    file.set_len(0)?;
    file.rewind()?;
    file.write_all(id.as_bytes())?;
    file.sync_all()?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_instances_share_one_channel_identity() {
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| resolve(dir.path(), "channel-a").unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(ids.iter().all(|id| id == &ids[0]));
        assert_eq!(resolve(dir.path(), "channel-a").unwrap(), ids[0]);
        assert_ne!(resolve(dir.path(), "channel-b").unwrap(), ids[0]);
        let other = tempfile::tempdir().unwrap();
        assert_ne!(resolve(other.path(), "channel-a").unwrap(), ids[0]);
    }

    #[test]
    fn caller_identity_is_not_a_path_and_corruption_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let channel = "../opaque/channel";
        let id = resolve(dir.path(), channel).unwrap();
        assert_eq!(resolve(dir.path(), channel).unwrap(), id);
        let file = dir
            .path()
            .join("codex-installation-ids")
            .join(hex::encode(Sha256::digest(channel.as_bytes())));
        std::fs::write(file, "invalid").unwrap();
        assert!(resolve(dir.path(), channel).is_err());
    }
}
