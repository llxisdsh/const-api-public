//! Client-side read-ahead: keep bursts near the consumer, then backpressure
//! instead of dropping output. This is not durable storage or request replay.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

// Allocated on demand, shared by all lanes of a physical connection (or by
// chunks of one HTTP response). One already decoded frame may exceed the budget;
// it occupies the entire budget and remains subject to the transport frame limit.
pub(crate) const OUTPUT_BUFFER_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct OutputBudget {
    credits: Arc<Semaphore>,
    limit: usize,
}

impl Default for OutputBudget {
    fn default() -> Self {
        Self {
            credits: Arc::new(Semaphore::new(OUTPUT_BUFFER_BYTES)),
            limit: OUTPUT_BUFFER_BYTES,
        }
    }
}

#[cfg(test)]
impl OutputBudget {
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            credits: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }
}

struct BufferedOutput<T> {
    value: T,
    _credit: Option<OwnedSemaphorePermit>,
}

pub(crate) struct OutputSender<T> {
    tx: mpsc::UnboundedSender<BufferedOutput<T>>,
    budget: OutputBudget,
}

impl<T> Clone for OutputSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            budget: self.budget.clone(),
        }
    }
}

pub(crate) struct OutputReceiver<T> {
    rx: mpsc::UnboundedReceiver<BufferedOutput<T>>,
}

pub(crate) fn channel<T>(budget: OutputBudget) -> (OutputSender<T>, OutputReceiver<T>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (OutputSender { tx, budget }, OutputReceiver { rx })
}

impl<T> OutputSender<T> {
    pub(crate) async fn closed(&self) {
        self.tx.closed().await;
    }

    pub(crate) async fn reserve(&self, payload_bytes: usize) -> Option<OwnedSemaphorePermit> {
        // Count overhead so tiny or empty frames are bounded too.
        let cost = payload_bytes
            .saturating_add(size_of::<BufferedOutput<T>>() + 32)
            .min(self.budget.limit) as u32;
        tokio::select! {
            _ = self.tx.closed() => None,
            permit = self.budget.credits.clone().acquire_many_owned(cost) => permit.ok(),
        }
    }

    pub(crate) fn send_reserved(&self, value: T, credit: OwnedSemaphorePermit) -> bool {
        self.tx
            .send(BufferedOutput {
                value,
                _credit: Some(credit),
            })
            .is_ok()
    }

    pub(crate) async fn send(&self, value: T, payload_bytes: usize) -> bool {
        match self.reserve(payload_bytes).await {
            Some(credit) => self.send_reserved(value, credit),
            None => false,
        }
    }

    // Shutdown only: final failure or already-held frames being drained from a
    // bounded reader. Ordinary data must always reserve credits.
    pub(crate) fn finish(&self, value: T) {
        let _ = self.tx.send(BufferedOutput {
            value,
            _credit: None,
        });
    }
}

impl<T> OutputReceiver<T> {
    pub(crate) async fn recv(&mut self) -> Option<T> {
        self.rx.recv().await.map(|item| item.value)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.rx.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.rx.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn full_buffer_waits_and_preserves_terminal_order() {
        let budget = OutputBudget::with_limit(128);
        let (tx, mut rx) = channel(budget.clone());
        assert!(tx.send(1, 128).await);
        let second = tx.send(2, 128);
        tokio::pin!(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        assert_eq!(rx.recv().await, Some(1));
        assert!(second.await);
        tx.finish(3);
        assert_eq!(rx.recv().await, Some(2));
        assert_eq!(rx.recv().await, Some(3));
        assert_eq!(budget.credits.available_permits(), 128);
        assert!(tx.send(4, 128).await);
        drop(rx);
        assert!(!tx.send(5, 128).await);
        assert_eq!(budget.credits.available_permits(), 128);
    }

    #[tokio::test]
    async fn tiny_frames_share_budget_across_lanes() {
        let budget = OutputBudget::with_limit(128);
        let (a, mut ar) = channel::<u8>(budget.clone());
        let (b, mut br) = channel::<u8>(budget.clone());
        assert!(a.send(1, 0).await);
        let credit = budget.credits.available_permits();
        assert!(credit < 128);
        assert!(b.send(2, 0).await);
        assert!(budget.credits.available_permits() < credit);
        assert_eq!(ar.recv().await, Some(1));
        assert_eq!(br.recv().await, Some(2));
        assert_eq!(budget.credits.available_permits(), 128);
    }
}
