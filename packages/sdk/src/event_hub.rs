// CradleRing SDK module implements event hub behavior.
// 翻译自 packages/sdk/src/event-hub.ts

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use futures_util::stream::Stream;
use serde_json::Value;

use crate::types::GatewayEvent;

/// Replay settings for EventHub streams.
#[derive(Default, Clone, Copy, Debug)]
pub struct EventHubOptions {
    pub replay_limit: Option<usize>,
}

/// Per-stream options for including replayed events.
#[derive(Default, Clone, Copy, Debug)]
pub struct EventStreamOptions {
    pub replay: bool,
}

type Listener<T> = Arc<dyn Fn(&T) + Send + Sync>;

struct WakeFlag {
    state: Mutex<bool>,
    cv: Condvar,
}

impl WakeFlag {
    fn new() -> Self {
        WakeFlag {
            state: Mutex::new(false),
            cv: Condvar::new(),
        }
    }
    fn signal(&self) {
        let mut s = self.state.lock().unwrap();
        *s = true;
        self.cv.notify_all();
    }
    fn wait(&self) {
        let mut s = self.state.lock().unwrap();
        while !*s {
            s = self.cv.wait(s).unwrap();
        }
    }
    #[allow(dead_code)]
    fn try_take(&self) -> bool {
        let mut s = self.state.lock().unwrap();
        if *s {
            *s = false;
            true
        } else {
            false
        }
    }
}

struct HubInner<T> {
    replay_limit: usize,
    replay_events: Mutex<Vec<T>>,
    closed: Mutex<bool>,
    listeners: Mutex<Vec<Listener<T>>>,
    waiters: Mutex<Vec<Arc<WakeFlag>>>,
}

/// Small publish/subscribe hub used by SDK transports and normalized events.
#[derive(Clone)]
pub struct EventHub<T: Send + Sync + 'static> {
    inner: Arc<HubInner<T>>,
}

impl<T: Send + Sync + 'static + Clone> EventHub<T> {
    pub fn new(options: EventHubOptions) -> Self {
        let replay_limit = options.replay_limit.unwrap_or(0);
        EventHub {
            inner: Arc::new(HubInner {
                replay_limit,
                replay_events: Mutex::new(Vec::new()),
                closed: Mutex::new(false),
                listeners: Mutex::new(Vec::new()),
                waiters: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn publish(&self, event: T) {
        {
            let closed = *self.inner.closed.lock().unwrap();
            if closed {
                return;
            }
        }
        if self.inner.replay_limit > 0 {
            let mut events = self.inner.replay_events.lock().unwrap();
            events.push(event.clone());
            let overflow = events.len() as isize - self.inner.replay_limit as isize;
            if overflow > 0 {
                let drop = overflow as usize;
                events.drain(0..drop);
            }
        }
        let listeners = self.inner.listeners.lock().unwrap().clone();
        for l in listeners {
            l(&event);
        }
    }

    pub fn close(&self) {
        *self.inner.closed.lock().unwrap() = true;
        self.inner.replay_events.lock().unwrap().clear();
        self.inner.listeners.lock().unwrap().clear();
        let waiters = std::mem::take(&mut *self.inner.waiters.lock().unwrap());
        for w in waiters {
            w.signal();
        }
    }

    pub fn snapshot(&self, filter: Option<Arc<dyn Fn(&T) -> bool + Send + Sync>>) -> Vec<T> {
        let events = self.inner.replay_events.lock().unwrap().clone();
        match filter {
            Some(f) => events.into_iter().filter(|e| f(e)).collect(),
            None => events,
        }
    }

    pub fn stream(
        &self,
        filter: Option<Box<dyn Fn(&T) -> bool + Send + Sync>>,
        options: EventStreamOptions,
    ) -> EventHubStream<T> {
        let filter_arc: Option<Arc<dyn Fn(&T) -> bool + Send + Sync>> =
            filter.map(|f| Arc::from(f));
        let initial: VecDeque<T> = if options.replay {
            self.snapshot(filter_arc.clone())
                .into_iter()
                .collect()
        } else {
            VecDeque::new()
        };
        let queue: Arc<Mutex<VecDeque<T>>> = Arc::new(Mutex::new(initial));
        let wake = Arc::new(WakeFlag::new());
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Register waiter so close() can wake us.
        {
            let mut waiters = self.inner.waiters.lock().unwrap();
            waiters.push(wake.clone());
        }

        // Listener pushes matching events into our queue.
        let queue_for_listener = queue.clone();
        let wake_for_listener = wake.clone();
        let listener: Listener<T> = if let Some(f) = filter_arc.clone() {
            Arc::new(move |event: &T| {
                if f(event) {
                    queue_for_listener.lock().unwrap().push_back(event.clone());
                    wake_for_listener.signal();
                }
            }) as Listener<T>
        } else {
            Arc::new(move |event: &T| {
                queue_for_listener.lock().unwrap().push_back(event.clone());
                wake_for_listener.signal();
            }) as Listener<T>
        };

        let listener_index = {
            let mut listeners = self.inner.listeners.lock().unwrap();
            listeners.push(listener);
            listeners.len() - 1
        };

        EventHubStream {
            inner: self.inner.clone(),
            queue,
            wake,
            closed,
            listener_index,
        }
    }
}

/// Stream returned by EventHub::stream that yields replayed + live events.
pub struct EventHubStream<T: Send + Sync + 'static> {
    inner: Arc<HubInner<T>>,
    queue: Arc<Mutex<VecDeque<T>>>,
    wake: Arc<WakeFlag>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    listener_index: usize,
}

impl<T: Send + Sync + 'static + Clone> EventHubStream<T> {
    fn cleanup(&mut self) {
        {
            let mut listeners = self.inner.listeners.lock().unwrap();
            if self.listener_index < listeners.len() {
                listeners.remove(self.listener_index);
            }
        }
        {
            let mut waiters = self.inner.waiters.lock().unwrap();
            waiters.retain(|w| !Arc::ptr_eq(w, &self.wake));
        }
    }
}

impl<T: Send + Sync + 'static + Clone> Stream for EventHubStream<T> {
    type Item = T;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return std::task::Poll::Ready(None);
        }
        if let Some(event) = this.queue.lock().unwrap().pop_front() {
            return std::task::Poll::Ready(Some(event));
        }
        let is_closed = *this.inner.closed.lock().unwrap();
        if is_closed {
            this.closed.store(true, std::sync::atomic::Ordering::SeqCst);
            this.cleanup();
            return std::task::Poll::Ready(None);
        }
        // Block on a wake flag. Real async poll would use std::task::Waker;
        // we keep the synchronous wait to preserve the TS semantics for the
        // minimal transport contract.
        this.wake.wait();
        std::task::Poll::Pending
    }
}

/// Return true when the provided value is shaped like a Gateway event.
pub fn is_gateway_event(value: &Value) -> bool {
    value
        .as_object()
        .and_then(|m| m.get("event"))
        .and_then(|v| v.as_str())
        .is_some()
}

impl GatewayEvent {
    /// Convenience constructor preserving the TS shape.
    pub fn is_gateway(value: &Value) -> bool {
        is_gateway_event(value)
    }
}
