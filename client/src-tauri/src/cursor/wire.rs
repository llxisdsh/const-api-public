//! The small Connect/protobuf subset used by Cursor AgentService. Unknown
//! protobuf fields are skipped, but malformed frames are never partial success.
use anyhow::{Result, anyhow, ensure};
use bytes::{Buf, BytesMut};
use serde_json::{Map, Value};
use std::io::Read;

pub(super) const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(super) enum ValueRef<'a> {
    Int(u64),
    Double(f64),
    Bytes(&'a [u8]),
    Fixed32,
}

fn read_varint(bytes: &[u8], offset: &mut usize) -> Result<u64> {
    let mut value = 0;
    for shift in (0..70).step_by(7) {
        let byte = *bytes
            .get(*offset)
            .ok_or_else(|| anyhow!("truncated Cursor protobuf varint"))?;
        *offset += 1;
        ensure!(shift != 63 || byte <= 1, "Cursor protobuf varint overflow");
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    anyhow::bail!("Cursor protobuf varint overflow")
}

pub(super) fn fields(bytes: &[u8]) -> Result<Vec<(u32, ValueRef<'_>)>> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = read_varint(bytes, &mut offset)?;
        ensure!(
            tag >> 3 > 0 && tag >> 3 < (1 << 29),
            "invalid Cursor protobuf field"
        );
        let value = match tag & 7 {
            0 => ValueRef::Int(read_varint(bytes, &mut offset)?),
            1 | 2 | 5 => {
                let size = match tag & 7 {
                    1 => 8,
                    5 => 4,
                    _ => usize::try_from(read_varint(bytes, &mut offset)?)?,
                };
                let end = offset
                    .checked_add(size)
                    .filter(|n| *n <= bytes.len())
                    .ok_or_else(|| anyhow!("truncated Cursor protobuf field"))?;
                let part = &bytes[offset..end];
                offset = end;
                match tag & 7 {
                    1 => ValueRef::Double(f64::from_le_bytes(part.try_into().unwrap())),
                    5 => ValueRef::Fixed32,
                    _ => ValueRef::Bytes(part),
                }
            }
            _ => anyhow::bail!("unsupported Cursor protobuf wire type"),
        };
        result.push(((tag >> 3) as u32, value));
    }
    Ok(result)
}

pub(super) fn data<'a>(fields: &[(u32, ValueRef<'a>)], number: u32) -> Option<&'a [u8]> {
    fields.iter().rev().find_map(|(n, v)| match v {
        ValueRef::Bytes(b) if *n == number => Some(*b),
        _ => None,
    })
}
pub(super) fn integer(fields: &[(u32, ValueRef<'_>)], number: u32) -> Option<u64> {
    fields.iter().rev().find_map(|(n, v)| match v {
        ValueRef::Int(v) if *n == number => Some(*v),
        _ => None,
    })
}
pub(super) fn string(fields: &[(u32, ValueRef<'_>)], number: u32) -> Result<String> {
    Ok(std::str::from_utf8(data(fields, number).unwrap_or_default())?.into())
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push((value as u8) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
pub(super) fn bytes(number: u32, value: impl AsRef<[u8]>) -> Vec<u8> {
    let value = value.as_ref();
    let mut out = Vec::with_capacity(value.len() + 8);
    varint(&mut out, u64::from(number) * 8 + 2);
    varint(&mut out, value.len() as u64);
    out.extend_from_slice(value);
    out
}
pub(super) fn int(number: u32, value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, u64::from(number) * 8);
    varint(&mut out, value);
    out
}
pub(super) fn message(number: u32, parts: &[Vec<u8>]) -> Vec<u8> {
    bytes(number, parts.concat())
}

/// google.protobuf.Value, not JSON text. Cursor uses this inside MCP schemas/args.
pub(super) fn json(value: &Value) -> Vec<u8> {
    match value {
        Value::Null => int(1, 0),
        Value::Bool(v) => int(4, u64::from(*v)),
        Value::String(v) => bytes(3, v),
        Value::Number(v) => {
            let mut out = vec![17];
            out.extend_from_slice(&v.as_f64().expect("JSON number").to_le_bytes());
            out
        }
        Value::Array(values) => message(
            6,
            &values.iter().map(|v| bytes(1, json(v))).collect::<Vec<_>>(),
        ),
        Value::Object(values) => message(
            5,
            &values
                .iter()
                .map(|(k, v)| message(1, &[bytes(1, k), bytes(2, json(v))]))
                .collect::<Vec<_>>(),
        ),
    }
}
pub(super) fn decode_json(input: &[u8], depth: usize) -> Result<Value> {
    ensure!(depth <= 64, "Cursor tool arguments exceed nesting limit");
    let entries = fields(input)?;
    let Some((n, value)) = entries.last() else {
        return Ok(Value::Null);
    };
    Ok(match (n, value) {
        (1, ValueRef::Int(0)) => Value::Null,
        // protobuf.Value represents every number as double. Restore exact JSON
        // integers so typed tools do not reject an integral value such as 2.0.
        (2, ValueRef::Double(n)) if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0 => {
            Value::from(*n as i64)
        }
        (2, ValueRef::Double(n)) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .ok_or_else(|| anyhow!("non-finite Cursor tool argument"))?,
        (3, ValueRef::Bytes(b)) => Value::String(std::str::from_utf8(b)?.into()),
        (4, ValueRef::Int(n)) => Value::Bool(*n != 0),
        (5, ValueRef::Bytes(b)) => {
            let mut values = Map::new();
            for (n, entry) in fields(b)? {
                if let (1, ValueRef::Bytes(entry)) = (n, entry) {
                    let entry = fields(entry)?;
                    values.insert(
                        string(&entry, 1)?,
                        decode_json(data(&entry, 2).unwrap_or_default(), depth + 1)?,
                    );
                }
            }
            Value::Object(values)
        }
        (6, ValueRef::Bytes(b)) => Value::Array(
            fields(b)?
                .into_iter()
                .filter_map(|(n, v)| match (n, v) {
                    (1, ValueRef::Bytes(b)) => Some(decode_json(b, depth + 1)),
                    _ => None,
                })
                .collect::<Result<_>>()?,
        ),
        _ => anyhow::bail!("invalid Cursor protobuf JSON value"),
    })
}

pub(super) fn frame(payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        payload.len() <= MAX_FRAME,
        "Cursor request frame is too large"
    );
    let mut bytes = Vec::with_capacity(payload.len() + 5);
    bytes.push(0);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

#[derive(Default)]
pub(super) struct Frames {
    buffer: BytesMut,
    ended: bool,
}
impl Frames {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<Vec<(bool, Vec<u8>)>> {
        ensure!(
            !self.ended || bytes.is_empty(),
            "Cursor data after Connect termination"
        );
        self.buffer.extend_from_slice(bytes);
        let mut result = Vec::new();
        while self.buffer.len() >= 5 {
            let flags = self.buffer[0];
            ensure!(flags & !3 == 0, "invalid Cursor Connect flags");
            let size = u32::from_be_bytes(self.buffer[1..5].try_into().unwrap()) as usize;
            ensure!(size <= MAX_FRAME, "Cursor Connect frame exceeds size limit");
            if self.buffer.len() < size + 5 {
                break;
            }
            self.buffer.advance(5);
            let raw = self.buffer.split_to(size);
            let payload = if flags & 1 != 0 {
                let mut decoded = Vec::new();
                flate2::read::GzDecoder::new(&raw[..])
                    .take((MAX_FRAME + 1) as u64)
                    .read_to_end(&mut decoded)?;
                ensure!(
                    decoded.len() <= MAX_FRAME,
                    "Cursor inflated frame exceeds size limit"
                );
                decoded
            } else {
                raw.to_vec()
            };
            self.ended = flags & 2 != 0;
            result.push((self.ended, payload));
            ensure!(
                !self.ended || self.buffer.is_empty(),
                "Cursor data after Connect termination"
            );
        }
        Ok(result)
    }
    pub(super) fn finish(&self) -> Result<()> {
        ensure!(
            self.buffer.is_empty(),
            "Cursor stream ended in a partial Connect frame"
        );
        Ok(())
    }
}
