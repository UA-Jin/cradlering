// CradleRing SDK module implements transport behavior.
// 翻译自 packages/sdk/src/transport.ts

use std::pin::Pin;
use std::sync::Arc;

use futures_util::stream::Stream;
use serde_json::Value;

use crate::event_hub::{EventHub, EventHubOptions, EventStreamOptions};
use crate::types::{
    ConnectableOpenClawTransport, GatewayEvent, GatewayRequestOptions, OpenClawTransport, SDKError,
};

const RAW_EVENT_REPLAY_LIMIT: usize = 1000;

/// Callback signatures mirroring the TS GatewayClient options.
pub type OnEventCb = Box<dyn FnMut(Value) + Send + Sync>;
pub type OnHelloOkCb = Box<dyn FnMut(Value) + Send + Sync>;
pub type OnConnectErrorCb = Box<dyn FnMut(SDKError) + Send + Sync>;
pub type OnReconnectPausedCb = Box<dyn FnMut(Value) + Send + Sync>;
pub type OnCloseCb = Box<dyn FnMut(i32, String) + Send + Sync>;
pub type OnGapCb = Box<dyn FnMut(GapInfo) + Send + Sync>;

#[derive(Default, Clone, Debug)]
pub struct GapInfo {
    pub expected: u64,
    pub received: u64,
}

/// Options passed through to the Gateway websocket client.
#[derive(Default)]
pub struct GatewayClientTransportOptions {
    pub url: Option<String>,
    pub request_timeout_ms: Option<u64>,
    pub token: Option<String>,
    pub password: Option<String>,
    pub on_event: Option<OnEventCb>,
    pub on_hello_ok: Option<OnHelloOkCb>,
    pub on_connect_error: Option<OnConnectErrorCb>,
    pub on_close: Option<OnCloseCb>,
    pub on_gap: Option<OnGapCb>,
}

#[allow(dead_code)]
fn to_gateway_event(event: &Value) -> GatewayEvent {
    let record = match event {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    let event_name = record
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    GatewayEvent {
        event: event_name,
        payload: record.get("payload").cloned(),
        seq: record.get("seq").and_then(|v| v.as_u64()),
        state_version: record.get("stateVersion").cloned(),
    }
}

/// Factory type for constructing a gateway client.
pub type GatewayClientFactory =
    Box<dyn FnOnce(&GatewayClientTransportOptions) -> Box<dyn GatewayClientLike> + Send + Sync>;

/// Minimal client surface used by the SDK transport adapter.
pub trait GatewayClientLike: Send + Sync + 'static {
    fn start(&self);
    fn stop_and_wait(&self) -> Pin<Box<dyn std::future::Future<Output = Result<(), SDKError>> + Send>>;
    fn request(
        &self,
        method: &str,
        params: Option<Value>,
        options: Option<GatewayRequestOptions>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, SDKError>> + Send>>;
}

/// Connectable SDK transport backed by a pluggable gateway client.
pub struct GatewayClientTransport {
    events_hub: EventHub<GatewayEvent>,
    options: GatewayClientTransportOptions,
    factory: std::sync::Mutex<Option<GatewayClientFactory>>,
    client: std::sync::Arc<std::sync::Mutex<Option<Arc<dyn GatewayClientLike>>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for GatewayClientTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayClientTransport").finish()
    }
}

impl GatewayClientTransport {
    pub fn new(options: GatewayClientTransportOptions, factory: GatewayClientFactory) -> Self {
        GatewayClientTransport {
            events_hub: EventHub::new(EventHubOptions {
                replay_limit: Some(RAW_EVENT_REPLAY_LIMIT),
            }),
            options,
            factory: std::sync::Mutex::new(Some(factory)),
            client: Arc::new(std::sync::Mutex::new(None)),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn with_client(
        client: Box<dyn GatewayClientLike>,
        options: GatewayClientTransportOptions,
    ) -> Self {
        let client: Arc<dyn GatewayClientLike> = Arc::from(client);
        GatewayClientTransport {
            events_hub: EventHub::new(EventHubOptions {
                replay_limit: Some(RAW_EVENT_REPLAY_LIMIT),
            }),
            options,
            factory: std::sync::Mutex::new(None),
            client: Arc::new(std::sync::Mutex::new(Some(client))),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn build(&self) {
        let slot = self.client.lock().unwrap();
        if slot.is_some() {
            return;
        }
        drop(slot);
        // The factory is a FnOnce; it cannot be cloned. Hosting code should
        // install a pre-built client via `with_client` instead.
    }
}

impl ConnectableOpenClawTransport for GatewayClientTransport {
    fn connect(&self) -> Pin<Box<dyn std::future::Future<Output = Result<(), SDKError>> + Send>> {
        let factory_slot = self.factory.lock().unwrap().take();
        let client = self.client.clone();
        let closed = self.closed.clone();
        let options = self.options.clone_for_callback();
        Box::pin(async move {
            if closed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(SDKError {
                    code: Some("closed".to_string()),
                    message: "gateway transport is closed".to_string(),
                    details: None,
                });
            }
            let mut client_guard = client.lock().unwrap();
            if client_guard.is_none() {
                match factory_slot {
                    Some(factory) => {
                        let c: Box<dyn GatewayClientLike> = factory(&options);
                        c.start();
                        let c: Arc<dyn GatewayClientLike> = Arc::from(c);
                        *client_guard = Some(c);
                    }
                    None => {
                        return Err(SDKError {
                            code: Some("no-factory".to_string()),
                            message: "no gateway client factory installed".to_string(),
                            details: None,
                        });
                    }
                }
            }
            Ok(())
        })
    }
}

impl OpenClawTransport for GatewayClientTransport {
    fn request(
        &self,
        method: &str,
        params: Option<Value>,
        options: Option<GatewayRequestOptions>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, SDKError>> + Send>> {
        let method = method.to_string();
        // The client slot holds an Arc<dyn GatewayClientLike>; cloning the Arc
        // gives us a Send + 'static handle that survives across awaits without
        // holding the transport mutex.
        let client: Option<Arc<dyn GatewayClientLike>> = self
            .client
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.clone());
        Box::pin(async move {
            let client = client.ok_or_else(|| SDKError {
                code: Some("unconnected".to_string()),
                message: "gateway transport is not connected".to_string(),
                details: None,
            })?;
            client.request(&method, params, options).await
        })
    }

    fn events(
        &self,
        filter: Option<Box<dyn Fn(&GatewayEvent) -> bool + Send + Sync>>,
    ) -> Pin<Box<dyn Stream<Item = GatewayEvent> + Send>> {
        Box::pin(
            self.events_hub
                .stream(filter, EventStreamOptions { replay: true }),
        )
    }

    fn close(&self) -> Pin<Box<dyn std::future::Future<Output = Result<(), SDKError>> + Send>> {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let client: Option<Arc<dyn GatewayClientLike>> = self.client.lock().unwrap().take();
        Box::pin(async move {
            if let Some(client) = client {
                let _ = client.stop_and_wait().await;
            }
            Ok(())
        })
    }
}

/// Narrow an SDK transport to one that supports explicit connect.
pub fn is_connectable_transport<T: ConnectableOpenClawTransport>(_: &T) -> bool {
    true
}

pub fn is_connectable_transport_dyn(_: &dyn OpenClawTransport) -> bool {
    false
}

impl Clone for GatewayClientTransportOptions {
    fn clone(&self) -> Self {
        GatewayClientTransportOptions {
            url: self.url.clone(),
            request_timeout_ms: self.request_timeout_ms,
            token: self.token.clone(),
            password: self.password.clone(),
            on_event: None,
            on_hello_ok: None,
            on_connect_error: None,
            on_close: None,
            on_gap: None,
        }
    }
}

impl GatewayClientTransportOptions {
    pub fn clone_for_callback(&self) -> Self {
        self.clone()
    }
}

impl Clone for GatewayClientTransport {
    fn clone(&self) -> Self {
        GatewayClientTransport {
            events_hub: self.events_hub.clone(),
            options: self.options.clone(),
            factory: std::sync::Mutex::new(None),
            client: Arc::new(std::sync::Mutex::new(None)),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(
                self.closed.load(std::sync::atomic::Ordering::SeqCst),
            )),
        }
    }
}
