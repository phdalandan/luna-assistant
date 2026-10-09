use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use url::Url;

use crate::credentials::AccessToken;
use crate::tls;

/// Floors were added to Home Assistant in 2024.4.
const MIN_VERSION: (u32, u32) = (2024, 4);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, HaError>>>>>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HaError {
    #[error("invalid Home Assistant URL")]
    InvalidUrl,
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("access token rejected")]
    AuthInvalid,
    #[error("Home Assistant {0} is older than the supported minimum")]
    UnsupportedVersion(String),
    #[error("connection closed")]
    Disconnected,
    #[error("not connected")]
    NotConnected,
    #[error("request timed out")]
    Timeout,
    #[error("request failed ({code}): {message}")]
    Request { code: String, message: String },
    #[error("unexpected message: {0}")]
    Protocol(String),
}

#[derive(Debug)]
pub struct Event {
    pub event_type: String,
    pub data: Value,
}

pub struct Connection {
    writer: mpsc::UnboundedSender<Message>,
    pending: Pending,
    next_id: AtomicU64,
    reader: JoinHandle<()>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

pub fn websocket_url(base_url: &str) -> Result<Url, HaError> {
    let mut url = Url::parse(base_url).map_err(|_| HaError::InvalidUrl)?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(HaError::InvalidUrl),
    };
    url.set_scheme(scheme).map_err(|()| HaError::InvalidUrl)?;
    url.set_path("/api/websocket");
    url.set_query(None);
    Ok(url)
}

/// Connects and authenticates. Events from subscriptions arrive on the returned receiver,
/// which closes when the connection drops.
pub async fn connect(
    base_url: &str,
    token: &AccessToken,
) -> Result<(Connection, mpsc::UnboundedReceiver<Event>), HaError> {
    let url = websocket_url(base_url)?;
    let connector = if url.scheme() == "wss" {
        let config = tls::client_config().map_err(|error| HaError::Connect(error.to_string()))?;
        Connector::Rustls(config)
    } else {
        Connector::Plain
    };
    let connecting =
        tokio_tungstenite::connect_async_tls_with_config(url.as_str(), None, true, Some(connector));
    let (socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|_| HaError::Timeout)?
        .map_err(|error| HaError::Connect(error.to_string()))?;
    let (mut sink, mut stream) = socket.split();

    expect_type(&next_json(&mut stream).await?, "auth_required")?;
    let auth = json!({"type": "auth", "access_token": token.expose()});
    sink.send(Message::text(auth.to_string()))
        .await
        .map_err(|_| HaError::Disconnected)?;
    let reply = next_json(&mut stream).await?;
    match message_type(&reply) {
        "auth_ok" => check_version(reply.get("ha_version").and_then(Value::as_str))?,
        "auth_invalid" => return Err(HaError::AuthInvalid),
        other => return Err(HaError::Protocol(other.to_owned())),
    }

    let (writer, mut outgoing) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let pending = Pending::default();
    let (events_tx, events) = mpsc::unbounded_channel();
    let reader = tokio::spawn(read_loop(stream, pending.clone(), events_tx));

    let connection = Connection {
        writer,
        pending,
        next_id: AtomicU64::new(1),
        reader,
    };
    Ok((connection, events))
}

impl Connection {
    /// Sends a command and waits for its result. The `id` field is assigned here.
    pub async fn request(&self, mut payload: Value) -> Result<Value, HaError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        payload["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);

        if self
            .writer
            .send(Message::text(payload.to_string()))
            .is_err()
        {
            lock(&self.pending).remove(&id);
            return Err(HaError::Disconnected);
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(HaError::Disconnected),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(HaError::Timeout)
            }
        }
    }

    pub async fn subscribe(&self, event_type: &str) -> Result<(), HaError> {
        self.request(json!({"type": "subscribe_events", "event_type": event_type}))
            .await
            .map(drop)
    }
}

async fn read_loop(
    mut stream: SplitStream<Socket>,
    pending: Pending,
    events: mpsc::UnboundedSender<Event>,
) {
    while let Ok(message) = next_json(&mut stream).await {
        match message_type(&message) {
            "result" | "pong" => resolve(&pending, &message),
            "event" => {
                let event = &message["event"];
                let event = Event {
                    event_type: event["event_type"].as_str().unwrap_or_default().to_owned(),
                    data: event["data"].clone(),
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            other => log::debug!("ignoring Home Assistant message of type {other}"),
        }
    }
    // Dropping the senders fails every in-flight request with `Disconnected`.
    lock(&pending).clear();
}

fn resolve(pending: &Pending, message: &Value) {
    let Some(sender) = message["id"]
        .as_u64()
        .and_then(|id| lock(pending).remove(&id))
    else {
        return;
    };
    let result = if message_type(message) == "pong" || message["success"].as_bool() == Some(true) {
        Ok(message.get("result").cloned().unwrap_or(Value::Null))
    } else {
        let error = &message["error"];
        Err(HaError::Request {
            code: error["code"].as_str().unwrap_or("unknown").to_owned(),
            message: error["message"].as_str().unwrap_or_default().to_owned(),
        })
    };
    let _ = sender.send(result);
}

async fn next_json(stream: &mut SplitStream<Socket>) -> Result<Value, HaError> {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text)
                    .map_err(|error| HaError::Protocol(error.to_string()));
            }
            Some(Ok(Message::Close(_))) | None => return Err(HaError::Disconnected),
            Some(Ok(_)) => continue,
            Some(Err(error)) => {
                log::warn!("Home Assistant connection error: {error}");
                return Err(HaError::Disconnected);
            }
        }
    }
}

fn message_type(message: &Value) -> &str {
    message["type"].as_str().unwrap_or_default()
}

fn expect_type(message: &Value, expected: &str) -> Result<(), HaError> {
    match message_type(message) {
        actual if actual == expected => Ok(()),
        actual => Err(HaError::Protocol(actual.to_owned())),
    }
}

fn check_version(version: Option<&str>) -> Result<(), HaError> {
    let version = version.unwrap_or_default();
    let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(year), Some(month)) if (year, month) >= MIN_VERSION => Ok(()),
        _ => Err(HaError::UnsupportedVersion(version.to_owned())),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub mod mock {
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    use super::*;

    pub type ServerSocket = WebSocketStream<TcpStream>;

    /// Accepts one WebSocket client and runs `script` against it.
    pub async fn server<F, Fut>(script: F) -> String
    where
        F: FnOnce(ServerSocket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            script(accept_async(stream).await.unwrap()).await;
        });
        format!("http://{address}")
    }

    pub async fn send(socket: &mut ServerSocket, value: Value) {
        socket.send(Message::text(value.to_string())).await.unwrap();
    }

    pub async fn receive(socket: &mut ServerSocket) -> Option<Value> {
        loop {
            match socket.next().await? {
                Ok(Message::Text(text)) => return serde_json::from_str(&text).ok(),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => continue,
            }
        }
    }

    /// Completes the auth handshake as Home Assistant `version`.
    pub async fn authenticate(socket: &mut ServerSocket, version: &str) {
        send(socket, json!({"type": "auth_required"})).await;
        let auth = receive(socket).await.unwrap();
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["access_token"], "token");
        send(socket, json!({"type": "auth_ok", "ha_version": version})).await;
    }

    pub fn token() -> AccessToken {
        AccessToken::new("token".into()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::mock::*;
    use super::*;

    #[test]
    fn builds_websocket_urls() {
        assert_eq!(
            websocket_url("http://ha.local:8123").unwrap().as_str(),
            "ws://ha.local:8123/api/websocket"
        );
        assert_eq!(
            websocket_url("https://ha.example.com/").unwrap().as_str(),
            "wss://ha.example.com/api/websocket"
        );
        assert_eq!(websocket_url("ftp://ha.local"), Err(HaError::InvalidUrl));
    }

    #[test]
    fn checks_minimum_version() {
        assert!(check_version(Some("2024.4.0")).is_ok());
        assert!(check_version(Some("2025.10.1")).is_ok());
        assert!(check_version(Some("2024.3.3")).is_err());
        assert!(check_version(None).is_err());
    }

    #[tokio::test]
    async fn authenticates_and_resolves_requests() {
        let url = server(|mut socket| async move {
            authenticate(&mut socket, "2025.1.0").await;
            let request = receive(&mut socket).await.unwrap();
            assert_eq!(request["type"], "get_states");
            send(
                &mut socket,
                json!({"id": request["id"], "type": "result", "success": true, "result": [1, 2]}),
            )
            .await;
            receive(&mut socket).await;
        })
        .await;

        let (connection, _events) = connect(&url, &token()).await.unwrap();
        let result = connection.request(json!({"type": "get_states"})).await;
        assert_eq!(result, Ok(json!([1, 2])));
    }

    #[tokio::test]
    async fn rejected_token_is_reported() {
        let url = server(|mut socket| async move {
            send(&mut socket, json!({"type": "auth_required"})).await;
            receive(&mut socket).await;
            send(
                &mut socket,
                json!({"type": "auth_invalid", "message": "bad"}),
            )
            .await;
        })
        .await;
        assert_eq!(
            connect(&url, &token()).await.err(),
            Some(HaError::AuthInvalid)
        );
    }

    #[tokio::test]
    async fn old_versions_are_rejected() {
        let url = server(|mut socket| async move {
            authenticate(&mut socket, "2023.12.0").await;
        })
        .await;
        assert!(matches!(
            connect(&url, &token()).await.err(),
            Some(HaError::UnsupportedVersion(_))
        ));
    }

    #[tokio::test]
    async fn failed_requests_carry_the_error() {
        let url = server(|mut socket| async move {
            authenticate(&mut socket, "2025.1.0").await;
            let request = receive(&mut socket).await.unwrap();
            send(
                &mut socket,
                json!({"id": request["id"], "type": "result", "success": false,
                       "error": {"code": "service_validation_error", "message": "nope"}}),
            )
            .await;
            receive(&mut socket).await;
        })
        .await;

        let (connection, _events) = connect(&url, &token()).await.unwrap();
        let result = connection.request(json!({"type": "call_service"})).await;
        assert!(
            matches!(result, Err(HaError::Request { code, .. }) if code == "service_validation_error")
        );
    }

    #[tokio::test]
    async fn closed_connection_fails_requests_and_ends_events() {
        let url = server(|mut socket| async move {
            authenticate(&mut socket, "2025.1.0").await;
            receive(&mut socket).await;
            socket.close(None).await.unwrap();
        })
        .await;

        let (connection, mut events) = connect(&url, &token()).await.unwrap();
        let result = connection.request(json!({"type": "get_states"})).await;
        assert_eq!(result, Err(HaError::Disconnected));
        assert!(events.recv().await.is_none());
    }

    #[tokio::test]
    async fn unreachable_server_is_a_connection_error() {
        let result = connect("http://127.0.0.1:1", &token()).await;
        assert!(matches!(result.err(), Some(HaError::Connect(_))));
    }
}
