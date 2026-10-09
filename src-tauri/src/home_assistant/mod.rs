mod connection;
pub mod discovery;
pub mod model;

use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value, json};
use tauri::async_runtime::JoinHandle;
use tokio::sync::{mpsc, watch};

use crate::credentials::AccessToken;
pub use connection::HaError;
use connection::{Connection, Event};
use model::{Home, Registries, StateEntry};

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
const REGISTRY_EVENTS: [&str; 4] = [
    "floor_registry_updated",
    "area_registry_updated",
    "device_registry_updated",
    "entity_registry_updated",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub enum ConnectionStatus {
    NotConfigured,
    Connecting,
    Connected,
    Reconnecting,
    AuthFailed,
    UnsupportedVersion,
    TokenUnavailable,
}

/// Live copy of the home, kept current by `state_changed` events.
pub struct HomeCache {
    home: RwLock<Home>,
    changes: watch::Sender<u64>,
}

impl HomeCache {
    pub fn new(home: Home) -> Self {
        Self {
            home: RwLock::new(home),
            changes: watch::Sender::new(0),
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, Home> {
        self.home
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn update(&self, change: impl FnOnce(&mut Home)) {
        change(
            &mut self
                .home
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        self.changes.send_modify(|version| *version += 1);
    }

    /// Resolves whenever the home changes after this call.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
}

/// A service call built by the action validator. Domain and service are never free text.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceCall {
    pub domain: &'static str,
    pub service: &'static str,
    pub entity_ids: Vec<String>,
    pub data: Map<String, Value>,
}

pub trait ServiceCaller {
    fn call_service(&self, call: &ServiceCall) -> impl Future<Output = Result<(), HaError>> + Send;
}

pub struct HomeAssistant {
    cache: Arc<HomeCache>,
    status: watch::Sender<ConnectionStatus>,
    connection: Arc<Mutex<Option<Arc<Connection>>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Default for HomeAssistant {
    fn default() -> Self {
        Self {
            cache: Arc::new(HomeCache::new(Home::default())),
            status: watch::Sender::new(ConnectionStatus::NotConfigured),
            connection: Arc::default(),
            task: Mutex::default(),
        }
    }
}

impl HomeAssistant {
    /// Replaces any running connection. Passing `None` disconnects.
    pub fn configure(&self, config: Option<(String, AccessToken)>) {
        self.stop();
        let Some((url, token)) = config else {
            self.status.send_replace(ConnectionStatus::NotConfigured);
            return;
        };
        self.status.send_replace(ConnectionStatus::Connecting);
        let supervisor = Supervisor {
            url,
            token,
            cache: self.cache.clone(),
            status: self.status.clone(),
            connection: self.connection.clone(),
        };
        *lock(&self.task) = Some(tauri::async_runtime::spawn(supervisor.run()));
    }

    /// Disconnects and records why the connection cannot be configured.
    pub fn fail(&self, status: ConnectionStatus) {
        self.stop();
        self.status.send_replace(status);
    }

    fn stop(&self) {
        if let Some(previous) = lock(&self.task).take() {
            previous.abort();
        }
        lock(&self.connection).take();
        self.cache.update(|home| *home = Home::default());
    }

    pub fn status(&self) -> ConnectionStatus {
        *self.status.borrow()
    }

    pub fn subscribe_status(&self) -> watch::Receiver<ConnectionStatus> {
        self.status.subscribe()
    }

    pub fn cache(&self) -> &HomeCache {
        &self.cache
    }
}

impl ServiceCaller for HomeAssistant {
    async fn call_service(&self, call: &ServiceCall) -> Result<(), HaError> {
        let connection = lock(&self.connection)
            .clone()
            .ok_or(HaError::NotConnected)?;
        connection
            .request(json!({
                "type": "call_service",
                "domain": call.domain,
                "service": call.service,
                "service_data": call.data,
                "target": {"entity_id": call.entity_ids},
            }))
            .await
            .map(drop)
    }
}

struct Supervisor {
    url: String,
    token: AccessToken,
    cache: Arc<HomeCache>,
    status: watch::Sender<ConnectionStatus>,
    connection: Arc<Mutex<Option<Arc<Connection>>>>,
}

impl Supervisor {
    async fn run(self) {
        let mut delay = Duration::from_secs(1);
        loop {
            let result = self.session().await;
            lock(&self.connection).take();
            match result {
                Err(HaError::AuthInvalid) => {
                    self.status.send_replace(ConnectionStatus::AuthFailed);
                    return;
                }
                Err(HaError::UnsupportedVersion(version)) => {
                    log::error!("Home Assistant {version} is not supported");
                    self.status
                        .send_replace(ConnectionStatus::UnsupportedVersion);
                    return;
                }
                Err(error) => log::warn!("Home Assistant connection failed: {error}"),
                Ok(()) => {
                    log::info!("Home Assistant disconnected");
                    delay = Duration::from_secs(1);
                }
            }
            self.status.send_replace(ConnectionStatus::Reconnecting);
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(MAX_RETRY_DELAY);
        }
    }

    /// Runs one connection until it drops. Subscribes before loading so no change is missed.
    async fn session(&self) -> Result<(), HaError> {
        let (connection, mut events) = connection::connect(&self.url, &self.token).await?;
        connection.subscribe("state_changed").await?;
        for event_type in REGISTRY_EVENTS {
            connection.subscribe(event_type).await?;
        }
        let home = load_home(&connection).await?;
        self.cache.update(|current| *current = home);

        let connection = Arc::new(connection);
        *lock(&self.connection) = Some(connection.clone());
        self.status.send_replace(ConnectionStatus::Connected);
        log::info!("connected to Home Assistant");

        let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
        keepalive.tick().await;
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(event) => self.handle_event(&connection, event, &mut events).await?,
                    None => return Ok(()),
                },
                _ = keepalive.tick() => {
                    connection.request(json!({"type": "ping"})).await?;
                }
            }
        }
    }

    async fn handle_event(
        &self,
        connection: &Connection,
        event: Event,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> Result<(), HaError> {
        if event.event_type == "state_changed" {
            apply_state_changed(&self.cache, &event.data);
            return Ok(());
        }
        let home = load_home(connection).await?;
        self.cache.update(|current| *current = home);
        // State events queued during the reload are applied on top of the fresh snapshot.
        while let Ok(event) = events.try_recv() {
            if event.event_type == "state_changed" {
                apply_state_changed(&self.cache, &event.data);
            }
        }
        Ok(())
    }
}

fn apply_state_changed(cache: &HomeCache, data: &Value) {
    let Some(entity_id) = data["entity_id"].as_str() else {
        return;
    };
    let state = match &data["new_state"] {
        Value::Null => None,
        value => match serde_json::from_value::<StateEntry>(value.clone()) {
            Ok(state) => Some(state),
            Err(error) => {
                log::warn!("ignoring malformed state for {entity_id}: {error}");
                return;
            }
        },
    };
    cache.update(|home| home.apply_state(entity_id, state));
}

async fn load_home(connection: &Connection) -> Result<Home, HaError> {
    let registries = Registries {
        floors: fetch(connection, "config/floor_registry/list").await?,
        areas: fetch(connection, "config/area_registry/list").await?,
        devices: fetch(connection, "config/device_registry/list").await?,
        entities: fetch(connection, "config/entity_registry/list").await?,
    };
    let states = fetch(connection, "get_states").await?;
    Ok(Home::build(registries, states))
}

async fn fetch<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    command: &str,
) -> Result<Vec<T>, HaError> {
    let result = connection.request(json!({"type": command})).await?;
    serde_json::from_value(result).map_err(|error| HaError::Protocol(format!("{command}: {error}")))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use connection::mock::*;

    use super::*;

    async fn wait_for_status(
        receiver: &mut watch::Receiver<ConnectionStatus>,
        expected: ConnectionStatus,
    ) {
        tokio::time::timeout(
            Duration::from_secs(5),
            receiver.wait_for(|status| *status == expected),
        )
        .await
        .expect("status not reached")
        .unwrap();
    }

    fn registry_reply(request: &Value) -> Value {
        let result = match request["type"].as_str().unwrap() {
            "config/floor_registry/list" => {
                json!([{"floor_id": "downstairs", "name": "Downstairs"}])
            }
            "config/area_registry/list" => {
                json!([{"area_id": "kitchen", "name": "Kitchen", "floor_id": "downstairs"}])
            }
            "config/device_registry/list" => json!([]),
            "config/entity_registry/list" => {
                json!([{"entity_id": "light.kitchen", "area_id": "kitchen"}])
            }
            "get_states" => {
                json!([{"entity_id": "light.kitchen", "state": "on", "attributes": {}}])
            }
            _ => Value::Null,
        };
        json!({"id": request["id"], "type": "result", "success": true, "result": result})
    }

    #[tokio::test]
    async fn loads_home_and_applies_state_events() {
        let url = server(|mut socket| async move {
            authenticate(&mut socket, "2025.1.0").await;
            let mut state_subscription = None;
            while let Some(request) = receive(&mut socket).await {
                if request["event_type"] == "state_changed" {
                    state_subscription = Some(request["id"].clone());
                }
                let is_last = request["type"] == "get_states";
                send(&mut socket, registry_reply(&request)).await;
                if is_last {
                    break;
                }
            }
            send(
                &mut socket,
                json!({"id": state_subscription, "type": "event", "event": {
                    "event_type": "state_changed",
                    "data": {"entity_id": "light.kitchen",
                             "new_state": {"entity_id": "light.kitchen", "state": "off", "attributes": {}}}
                }}),
            )
            .await;
            receive(&mut socket).await;
        })
        .await;

        let home_assistant = HomeAssistant::default();
        let mut status = home_assistant.subscribe_status();
        let mut changes = home_assistant.cache().subscribe();
        home_assistant.configure(Some((url, token())));
        wait_for_status(&mut status, ConnectionStatus::Connected).await;

        tokio::time::timeout(
            Duration::from_secs(5),
            changes.wait_for(|_| {
                home_assistant
                    .cache()
                    .read()
                    .entity("light.kitchen")
                    .is_some_and(|entity| entity.state == "off")
            }),
        )
        .await
        .expect("state event not applied")
        .unwrap();
        let home = home_assistant.cache().read();
        assert_eq!(
            home.floor_id_of(home.entity("light.kitchen").unwrap()),
            Some("downstairs")
        );
    }

    #[tokio::test]
    async fn rejected_token_stops_retrying() {
        let url = server(|mut socket| async move {
            send(&mut socket, json!({"type": "auth_required"})).await;
            receive(&mut socket).await;
            send(&mut socket, json!({"type": "auth_invalid"})).await;
        })
        .await;

        let home_assistant = HomeAssistant::default();
        let mut status = home_assistant.subscribe_status();
        home_assistant.configure(Some((url, token())));
        wait_for_status(&mut status, ConnectionStatus::AuthFailed).await;
    }

    #[tokio::test]
    async fn unreachable_server_keeps_reconnecting() {
        let home_assistant = HomeAssistant::default();
        let mut status = home_assistant.subscribe_status();
        home_assistant.configure(Some(("http://127.0.0.1:1".into(), token())));
        wait_for_status(&mut status, ConnectionStatus::Reconnecting).await;
        home_assistant.configure(None);
        assert_eq!(home_assistant.status(), ConnectionStatus::NotConfigured);
    }

    #[tokio::test]
    async fn service_calls_require_a_connection() {
        let call = ServiceCall {
            domain: "light",
            service: "turn_off",
            entity_ids: vec!["light.kitchen".into()],
            data: Map::new(),
        };
        let result = HomeAssistant::default().call_service(&call).await;
        assert_eq!(result, Err(HaError::NotConnected));
    }
}
