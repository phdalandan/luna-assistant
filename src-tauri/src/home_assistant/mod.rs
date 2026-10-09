mod connection;
pub mod discovery;
pub mod model;

use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tauri::async_runtime::JoinHandle;
use tokio::sync::watch;

use crate::credentials::AccessToken;
use connection::Connection;
pub use connection::HaError;
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

/// The only Home Assistant operations the assistant can use.
pub trait HomeApi {
    fn call_service(&self, call: &ServiceCall) -> impl Future<Output = Result<(), HaError>> + Send;

    /// Reads every state from Home Assistant and applies it to the cache.
    fn refresh_states(&self) -> impl Future<Output = Result<(), HaError>> + Send;
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

impl HomeApi for HomeAssistant {
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

    async fn refresh_states(&self) -> Result<(), HaError> {
        let connection = lock(&self.connection)
            .clone()
            .ok_or(HaError::NotConnected)?;
        let states: Vec<StateEntry> = fetch(&connection, "get_states").await?;
        self.cache.update(|home| {
            for state in states {
                home.apply_state(&state.entity_id.clone(), Some(state));
            }
        });
        Ok(())
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
        // Registries reload alongside state events so verification is never held up.
        let mut reload: Option<BoxFuture<'_, Result<Registries, HaError>>> = None;
        let mut reload_again = false;
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    None => return Ok(()),
                    Some(event) if event.event_type == "state_changed" => {
                        apply_state_changed(&self.cache, &event.data);
                    }
                    // The running reload may have read the registries before this change.
                    Some(_) if reload.is_some() => reload_again = true,
                    Some(_) => reload = Some(load_registries(&connection).boxed()),
                },
                registries = async { reload.as_mut().expect("reload is running").await }, if reload.is_some() => {
                    let registries = registries?;
                    self.cache.update(|home| home.replace_registries(registries));
                    reload = std::mem::take(&mut reload_again)
                        .then(|| load_registries(&connection).boxed());
                }
                _ = keepalive.tick() => {
                    connection.request(json!({"type": "ping"})).await?;
                }
            }
        }
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
    let registries = load_registries(connection).await?;
    let states = fetch(connection, "get_states").await?;
    Ok(Home::build(registries, states))
}

/// Metadata only. States are kept current by `state_changed` events.
async fn load_registries(connection: &Connection) -> Result<Registries, HaError> {
    Ok(Registries {
        floors: fetch(connection, "config/floor_registry/list").await?,
        areas: fetch(connection, "config/area_registry/list").await?,
        devices: fetch(connection, "config/device_registry/list").await?,
        entities: fetch(connection, "config/entity_registry/list").await?,
    })
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

    fn event(event_type: &str, data: Value) -> Value {
        json!({"id": 1, "type": "event", "event": {"event_type": event_type, "data": data}})
    }

    /// Registry requests after a registry event are answered slowly. Midway through, the kitchen
    /// light turns off and the kitchen area is renamed.
    #[tokio::test]
    async fn registry_reloads_keep_states_flowing_and_never_drop_changes() {
        const REPLY_DELAY: Duration = Duration::from_millis(300);
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let state_sent = Arc::new(Mutex::new(None::<tokio::time::Instant>));
        let (recorded, sent) = (requests.clone(), state_sent.clone());
        let url = server(|mut socket| async move {
            authenticate(&mut socket, "2025.1.0").await;
            while let Some(request) = receive(&mut socket).await {
                let is_last = request["type"] == "get_states";
                send(&mut socket, registry_reply(&request)).await;
                if is_last {
                    break;
                }
            }
            send(&mut socket, event("area_registry_updated", json!({}))).await;
            let mut renamed = false;
            while let Some(request) = receive(&mut socket).await {
                let kind = request["type"].as_str().unwrap().to_owned();
                lock(&recorded).push(kind.clone());
                if kind == "config/area_registry/list" && !renamed {
                    // The rename lands after this reply was produced, so a second reload is needed.
                    renamed = true;
                    let off = json!({"entity_id": "light.kitchen", "new_state":
                        {"entity_id": "light.kitchen", "state": "off", "attributes": {}}});
                    send(&mut socket, event("state_changed", off)).await;
                    *lock(&sent) = Some(tokio::time::Instant::now());
                    send(&mut socket, event("area_registry_updated", json!({}))).await;
                    tokio::time::sleep(REPLY_DELAY).await;
                    send(&mut socket, registry_reply(&request)).await;
                } else if kind == "config/area_registry/list" {
                    let mut reply = registry_reply(&request);
                    reply["result"][0]["name"] = json!("Cook Room");
                    tokio::time::sleep(REPLY_DELAY).await;
                    send(&mut socket, reply).await;
                } else {
                    tokio::time::sleep(REPLY_DELAY).await;
                    send(&mut socket, registry_reply(&request)).await;
                }
            }
        })
        .await;

        let home_assistant = HomeAssistant::default();
        let mut status = home_assistant.subscribe_status();
        let mut changes = home_assistant.cache().subscribe();
        home_assistant.configure(Some((url, token())));
        wait_for_status(&mut status, ConnectionStatus::Connected).await;

        let off = |cache: &HomeCache| {
            cache
                .read()
                .entity("light.kitchen")
                .is_some_and(|entity| entity.state == "off")
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            changes.wait_for(|_| off(home_assistant.cache())),
        )
        .await
        .expect("state event not applied")
        .unwrap();
        let state_delay = lock(&state_sent).unwrap().elapsed();

        let renamed = |cache: &HomeCache| cache.read().areas.first().map(|area| area.name.clone());
        let applied = tokio::time::timeout(
            Duration::from_secs(10),
            changes.wait_for(|_| renamed(home_assistant.cache()).as_deref() == Some("Cook Room")),
        )
        .await;
        let requests = lock(&requests).clone();
        println!(
            "state applied {} ms after it was sent during a reload; requests after registry events: {requests:?}",
            state_delay.as_millis()
        );
        assert!(
            applied.is_ok(),
            "a registry change during a reload was lost"
        );
        assert!(
            state_delay < REPLY_DELAY,
            "state events waited for the reload"
        );
        assert!(!requests.contains(&"get_states".to_owned()));
        assert!(
            off(home_assistant.cache()),
            "the reload replaced a newer state"
        );
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
