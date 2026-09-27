// The first upstream chunk is forwarded immediately. Subsequent tiny chunks
// share a short 50 ms window so human-visible streaming stays smooth while the
// server receives at most about 20 durability events per second per busy
// stream. Large/tool payloads flush early at 64 KiB.
const SUPPLIER_STREAM_BATCH_MAX_BYTES: usize = 64 * 1024;
const SUPPLIER_STREAM_BATCH_MAX_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug)]
struct SupplierStreamBatch {
    bytes: Vec<u8>,
    source_chunks: usize,
    largest_source_chunk: usize,
}

#[derive(Debug)]
enum SupplierStreamRead {
    Chunk(SupplierStreamBatch),
    End,
    InactivityTimeout,
}

struct SupplierByteStreamBatcher<S> {
    stream: S,
    pending: Vec<u8>,
    pending_chunks: usize,
    largest_pending_chunk: usize,
    deadline: Option<tokio::time::Instant>,
    first_chunk: bool,
    ended: bool,
    deferred_error: Option<anyhow::Error>,
}

impl<S> SupplierByteStreamBatcher<S>
where
    S: futures_util::Stream<
            Item = std::result::Result<bytes::Bytes, reqwest::Error>,
        > + Unpin,
{
    fn new(stream: S) -> Self {
        Self {
            stream,
            pending: Vec::with_capacity(SUPPLIER_STREAM_BATCH_MAX_BYTES),
            pending_chunks: 0,
            largest_pending_chunk: 0,
            deadline: None,
            first_chunk: true,
            ended: false,
            deferred_error: None,
        }
    }

    async fn next(&mut self) -> Result<SupplierStreamRead> {
        loop {
            if self.pending.is_empty() {
                if let Some(error) = self.deferred_error.take() {
                    return Err(error);
                }
                if self.ended {
                    return Ok(SupplierStreamRead::End);
                }
                let next = tokio::time::timeout(
                    Duration::from_secs(
                        crate::protocol::stream::STREAM_INACTIVITY_TIMEOUT_SECS,
                    ),
                    self.stream.next(),
                )
                .await;
                match next {
                    Err(_) => return Ok(SupplierStreamRead::InactivityTimeout),
                    Ok(None) => {
                        self.ended = true;
                        return Ok(SupplierStreamRead::End);
                    }
                    Ok(Some(Err(error))) => return Err(error.into()),
                    Ok(Some(Ok(chunk))) => {
                        if chunk.is_empty() {
                            continue;
                        }
                        if self.first_chunk {
                            self.first_chunk = false;
                            return Ok(SupplierStreamRead::Chunk(SupplierStreamBatch {
                                largest_source_chunk: chunk.len(),
                                source_chunks: 1,
                                bytes: chunk.to_vec(),
                            }));
                        }
                        self.push(chunk.as_ref());
                        if self.pending.len() >= SUPPLIER_STREAM_BATCH_MAX_BYTES {
                            return Ok(SupplierStreamRead::Chunk(self.take_pending()));
                        }
                    }
                }
                continue;
            }

            let deadline = self.deadline.unwrap_or_else(tokio::time::Instant::now);
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    return Ok(SupplierStreamRead::Chunk(self.take_pending()));
                }
                next = self.stream.next() => {
                    match next {
                        None => {
                            self.ended = true;
                            return Ok(SupplierStreamRead::Chunk(self.take_pending()));
                        }
                        Some(Err(error)) => {
                            self.deferred_error = Some(error.into());
                            return Ok(SupplierStreamRead::Chunk(self.take_pending()));
                        }
                        Some(Ok(chunk)) => {
                            if chunk.is_empty() {
                                continue;
                            }
                            self.push(chunk.as_ref());
                            if self.pending.len() >= SUPPLIER_STREAM_BATCH_MAX_BYTES {
                                return Ok(SupplierStreamRead::Chunk(self.take_pending()));
                            }
                        }
                    }
                }
            }
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.pending.is_empty() {
            self.deadline = Some(tokio::time::Instant::now() + SUPPLIER_STREAM_BATCH_MAX_DELAY);
        }
        self.pending_chunks += 1;
        self.largest_pending_chunk = self.largest_pending_chunk.max(chunk.len());
        self.pending.extend_from_slice(chunk);
    }

    fn take_pending(&mut self) -> SupplierStreamBatch {
        self.deadline = None;
        let bytes = std::mem::replace(
            &mut self.pending,
            Vec::with_capacity(SUPPLIER_STREAM_BATCH_MAX_BYTES),
        );
        SupplierStreamBatch {
            bytes,
            source_chunks: std::mem::take(&mut self.pending_chunks),
            largest_source_chunk: std::mem::take(&mut self.largest_pending_chunk),
        }
    }
}

#[cfg(test)]
mod supplier_stream_batch_tests {
    use super::*;

    #[tokio::test]
    async fn first_chunk_is_immediate_and_following_ready_chunks_are_coalesced() {
        let source = futures_util::stream::iter(vec![
            Ok::<_, reqwest::Error>(bytes::Bytes::from_static(b"first")),
            Ok::<_, reqwest::Error>(bytes::Bytes::from_static(b"-second")),
            Ok::<_, reqwest::Error>(bytes::Bytes::from_static(b"-third")),
        ]);
        let mut batches = SupplierByteStreamBatcher::new(source);

        let SupplierStreamRead::Chunk(first) = batches.next().await.unwrap() else {
            panic!("first batch missing");
        };
        assert_eq!(first.bytes, b"first");
        assert_eq!(first.source_chunks, 1);

        let SupplierStreamRead::Chunk(rest) = batches.next().await.unwrap() else {
            panic!("coalesced batch missing");
        };
        assert_eq!(rest.bytes, b"-second-third");
        assert_eq!(rest.source_chunks, 2);
        assert_eq!(rest.largest_source_chunk, 7);
        assert!(matches!(
            batches.next().await.unwrap(),
            SupplierStreamRead::End
        ));
    }

    #[tokio::test]
    async fn batch_size_limit_flushes_without_losing_bytes() {
        let half = vec![b'x'; SUPPLIER_STREAM_BATCH_MAX_BYTES / 2 + 1];
        let source = futures_util::stream::iter(vec![
            Ok::<_, reqwest::Error>(bytes::Bytes::from_static(b"first")),
            Ok::<_, reqwest::Error>(bytes::Bytes::from(half.clone())),
            Ok::<_, reqwest::Error>(bytes::Bytes::from(half.clone())),
        ]);
        let mut batches = SupplierByteStreamBatcher::new(source);
        let _ = batches.next().await.unwrap();

        let SupplierStreamRead::Chunk(batch) = batches.next().await.unwrap() else {
            panic!("size-limited batch missing");
        };
        assert_eq!(batch.bytes.len(), half.len() * 2);
        assert_eq!(batch.source_chunks, 2);
    }

    #[tokio::test]
    async fn one_large_following_chunk_flushes_without_waiting_for_another_read() {
        let large = vec![b'y'; SUPPLIER_STREAM_BATCH_MAX_BYTES + 1];
        let source = futures_util::stream::iter(vec![
            Ok::<_, reqwest::Error>(bytes::Bytes::from_static(b"first")),
            Ok::<_, reqwest::Error>(bytes::Bytes::from(large.clone())),
        ]);
        let mut batches = SupplierByteStreamBatcher::new(source);
        let _ = batches.next().await.unwrap();

        let SupplierStreamRead::Chunk(batch) = batches.next().await.unwrap() else {
            panic!("large batch missing");
        };
        assert_eq!(batch.bytes, large);
        assert_eq!(batch.source_chunks, 1);
    }
}
