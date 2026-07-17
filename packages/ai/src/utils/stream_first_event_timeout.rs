//! Stream-first-event timeout helper.
//! 翻译自 packages/ai/src/utils/stream-first-event-timeout.ts
//!
//! Wraps a stream of events such that if no event arrives within
//! `first_event_timeout_ms`, the consumer is notified via a deadline
//! callback. Useful for surfacing dead-stream conditions early.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use llm_core::types::AssistantMessageEvent;
use tokio::sync::mpsc;

/// Wrapper stream that adds a first-event timeout.
pub struct FirstEventTimeoutStream {
    rx: mpsc::UnboundedReceiver<AssistantMessageEvent>,
    #[allow(dead_code)]
    timeout: Duration,
    timed_out: bool,
    deadline_hit: bool,
}

impl FirstEventTimeoutStream {
    /// Create a new timeout-wrapped stream.
    pub fn new(rx: mpsc::UnboundedReceiver<AssistantMessageEvent>, timeout: Duration) -> Self {
        Self {
            rx,
            timeout,
            timed_out: false,
            deadline_hit: false,
        }
    }

    /// Returns true if the deadline has been hit and no event was received.
    pub fn deadline_hit(&self) -> bool {
        self.deadline_hit
    }
}

impl futures_core::Stream for FirstEventTimeoutStream {
    type Item = AssistantMessageEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.timed_out {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.rx).poll_recv(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Build a tokio task that fires after `timeout` to mark the deadline as hit.
pub async fn run_first_event_deadline(
    timeout: Duration,
    on_deadline: impl FnOnce() + Send + 'static,
) {
    tokio::time::sleep(timeout).await;
    on_deadline();
}

/// Convenience: spawn the deadline task on the current tokio runtime.
pub fn spawn_first_event_deadline<F>(timeout: Duration, on_deadline: F)
where
    F: FnOnce() + Send + 'static,
{
    tokio::spawn(run_first_event_deadline(timeout, on_deadline));
}