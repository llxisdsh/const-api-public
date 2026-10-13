//! Incremental AWS EventStream framing. CRC32 is IEEE, not CRC32C.
use anyhow::{Context, Result, anyhow, ensure};
use std::collections::HashMap;

const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) headers: HashMap<String, String>,
    pub(crate) payload: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct Decoder {
    pending: Vec<u8>,
    frame_len: Option<usize>,
}

impl Decoder {
    pub(crate) fn push(&mut self, mut input: &[u8]) -> Result<Vec<Frame>> {
        let mut frames = Vec::new();
        while !input.is_empty() {
            let target = self.frame_len.unwrap_or(12);
            let count = (target - self.pending.len()).min(input.len());
            self.pending.extend_from_slice(&input[..count]);
            input = &input[count..];
            if self.pending.len() != target {
                continue;
            }
            if self.frame_len.is_none() {
                ensure!(
                    crc32fast::hash(&self.pending[..8]) == u32_at(&self.pending, 8)?,
                    "Kiro EventStream prelude checksum mismatch"
                );
                let total = u32_at(&self.pending, 0)? as usize;
                let headers = u32_at(&self.pending, 4)? as usize;
                ensure!(
                    (16..=MAX_FRAME).contains(&total) && headers <= total - 16,
                    "Kiro EventStream frame length is invalid"
                );
                self.frame_len = Some(total);
            } else {
                ensure!(
                    crc32fast::hash(&self.pending[..target - 4])
                        == u32_at(&self.pending, target - 4)?,
                    "Kiro EventStream message checksum mismatch"
                );
                let end = 12 + u32_at(&self.pending, 4)? as usize;
                frames.push(Frame {
                    headers: parse_headers(&self.pending[12..end])?,
                    payload: self.pending[end..target - 4].to_vec(),
                });
                self.pending.clear();
                self.frame_len = None;
            }
        }
        Ok(frames)
    }

    pub(crate) fn finish(&self) -> Result<()> {
        ensure!(
            self.pending.is_empty(),
            "Kiro EventStream ended in a partial frame"
        );
        Ok(())
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| anyhow!("truncated Kiro EventStream integer"))?;
    Ok(u32::from_be_bytes(slice.try_into()?))
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8]> {
    ensure!(count <= bytes.len(), "truncated Kiro EventStream header");
    let (head, tail) = bytes.split_at(count);
    *bytes = tail;
    Ok(head)
}

fn parse_headers(mut bytes: &[u8]) -> Result<HashMap<String, String>> {
    let mut headers = HashMap::new();
    while !bytes.is_empty() {
        let length = take(&mut bytes, 1)?[0] as usize;
        ensure!(length > 0, "empty Kiro EventStream header name");
        let name = std::str::from_utf8(take(&mut bytes, length)?)
            .context("invalid Kiro EventStream header name")?;
        let kind = take(&mut bytes, 1)?[0];
        let length = match kind {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            9 => 16,
            6 | 7 => u16::from_be_bytes(take(&mut bytes, 2)?.try_into()?) as usize,
            _ => anyhow::bail!("unknown Kiro EventStream header type"),
        };
        let value = take(&mut bytes, length)?;
        if matches!(
            name,
            ":message-type" | ":event-type" | ":exception-type" | ":error-code" | ":error-message"
        ) {
            ensure!(
                kind == 7,
                "Kiro EventStream control header must be a string"
            );
            let value =
                std::str::from_utf8(value).context("invalid Kiro EventStream header value")?;
            ensure!(
                headers.insert(name.into(), value.into()).is_none(),
                "duplicate Kiro EventStream control header"
            );
        }
    }
    ensure!(
        headers.contains_key(":message-type"),
        "Kiro EventStream is missing message type"
    );
    Ok(headers)
}

#[cfg(test)]
pub(super) fn test_frame(kind: &str, event: &str, payload: &[u8]) -> Vec<u8> {
    let mut headers = Vec::new();
    for (key, value) in [(":message-type", kind), (":event-type", event)] {
        headers.push(key.len() as u8);
        headers.extend_from_slice(key.as_bytes());
        headers.push(7);
        headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
        headers.extend_from_slice(value.as_bytes());
    }
    let mut frame = Vec::new();
    frame.extend_from_slice(&((headers.len() + payload.len() + 16) as u32).to_be_bytes());
    frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    frame.extend_from_slice(&crc32fast::hash(&frame).to_be_bytes());
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&crc32fast::hash(&frame).to_be_bytes());
    frame
}
