use super::{MAX_STREAM_BYTES, StreamError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseFrame {
    pub(crate) event: Option<String>,
    pub(crate) data: String,
}

#[derive(Debug, Default)]
pub(crate) struct Utf8ChunkDecoder {
    buffer: Vec<u8>,
}

impl Utf8ChunkDecoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<String, StreamError> {
        self.buffer.extend_from_slice(bytes);
        match std::str::from_utf8(&self.buffer) {
            Ok(text) => {
                let output = text.to_string();
                self.buffer.clear();
                Ok(output)
            }
            Err(error) if error.error_len().is_none() => {
                let valid = error.valid_up_to();
                let output = std::str::from_utf8(&self.buffer[..valid])
                    .map_err(|error| {
                        StreamError::new(
                            "stream_utf8_invalid",
                            format!("stream contains invalid UTF-8 prefix: {error}"),
                        )
                    })?
                    .to_string();
                self.buffer.drain(..valid);
                Ok(output)
            }
            Err(error) => Err(StreamError::new(
                "stream_utf8_invalid",
                format!("stream contains invalid UTF-8: {error}"),
            )),
        }
    }

    pub(crate) fn finish(&mut self) -> Result<String, StreamError> {
        if self.buffer.is_empty() {
            return Ok(String::new());
        }
        let text = std::str::from_utf8(&self.buffer).map_err(|error| {
            StreamError::new(
                "stream_utf8_incomplete",
                format!("stream ended inside a UTF-8 code point: {error}"),
            )
        })?;
        let output = text.to_string();
        self.buffer.clear();
        Ok(output)
    }
}

#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseFrame>, StreamError> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_STREAM_BYTES {
            return Err(StreamError::new(
                "stream_event_too_large",
                format!("partial SSE event exceeds {MAX_STREAM_BYTES} bytes"),
            ));
        }
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut consumed = 0usize;
        while let Some((boundary, separator)) = find_boundary(&self.buffer[consumed..]) {
            let event_end = consumed.saturating_add(boundary);
            if let Some(frame) = parse_frame(&self.buffer[consumed..event_end])? {
                frames.push(frame);
            }
            consumed = event_end.saturating_add(separator);
        }
        // Remove all completed events in one move. Draining from the front for every frame
        // repeatedly shifted the remaining response and made large SSE bodies quadratic.
        if consumed > 0 {
            self.buffer.drain(..consumed);
        }
        Ok(frames)
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<SseFrame>, StreamError> {
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }
        let event = std::mem::take(&mut self.buffer);
        Ok(parse_frame(&event)?.into_iter().collect())
    }
}

pub(super) fn find_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
    }
}

fn parse_frame(bytes: &[u8]) -> Result<Option<SseFrame>, StreamError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        StreamError::new(
            "stream_utf8_invalid",
            format!("SSE event is not valid UTF-8: {error}"),
        )
    })?;
    let mut event = None;
    let mut data = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.starts_with(':') || line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.strip_prefix(' ').unwrap_or(value).to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    if data.is_empty() {
        return Ok(None);
    }
    Ok(Some(SseFrame {
        event,
        data: data.join("\n"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_terminal_event_waits_for_the_sse_boundary() {
        let mut decoder = SseDecoder::default();

        let first = decoder
            .push(
                b"event: response.completed\n\
                  data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"",
            )
            .expect("partial frame");
        assert!(first.is_empty());

        let second = decoder
            .push(b",\"output\":[]}}")
            .expect("completed JSON without SSE boundary");
        assert!(second.is_empty());

        let completed = decoder.push(b"\n\n").expect("SSE boundary");
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].event.as_deref(), Some("response.completed"));
        assert!(completed[0].data.contains("\"id\":\"resp_1\""));
    }

    #[test]
    fn one_push_decodes_all_complete_frames_and_keeps_the_partial_tail() {
        let mut decoder = SseDecoder::default();
        let mut body = Vec::new();
        for index in 0..128 {
            body.extend_from_slice(format!("event: delta\ndata: {index}\n\n").as_bytes());
        }
        body.extend_from_slice(b"event: delta\ndata: tail");

        let frames = decoder.push(&body).expect("complete frames");
        assert_eq!(frames.len(), 128);
        assert_eq!(frames.first().map(|frame| frame.data.as_str()), Some("0"));
        assert_eq!(frames.last().map(|frame| frame.data.as_str()), Some("127"));

        let tail = decoder.push(b"\n\n").expect("partial tail completion");
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].data, "tail");
    }
}
