use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Path, State},
    response::IntoResponse,
    routing::{get, any},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use shared::{TunnelMessage, TunnelRequest, TunnelResponse};
use std::{collections::HashMap, sync::{Arc, Mutex}};
use tokio::sync::oneshot;
use nanoid::nanoid;
use base64::Engine as _;

#[derive(Default)]
struct AppState {
    // clientId -> WebSocket Tx channel (simplified as Arc<Mutex<WebSocket>> for now)
    clients: Arc<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Message>>>>,
    // requestId -> Oneshot Tx channel to resolve the HTTP request
    pending_requests: Arc<Mutex<HashMap<String, oneshot::Sender<TunnelResponse>>>>,
}

const MAX_BODY_SIZE: usize = 5 * 1024 * 1024; // 5MB

fn get_auth_token() -> String {
    std::env::var("PORTAL_AUTH_TOKEN").unwrap_or_else(|_| "portal-secret-123".to_string())
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));
    let state = Arc::new(AppState::default());

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/tunnel/:client_id", any(tunnel_handler))
        .route("/tunnel/:client_id/*path", any(tunnel_handler))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_SIZE))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    log::info!("PortalTech Relay Server (Rust) listening on http://localhost:3000");
    log::info!("Security: Body limit (5MB) and Token Auth enabled");
    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // Basic Token Authentication
    let auth_header = headers.get("X-Portal-Auth")
        .and_then(|h| h.to_str().ok());

    if auth_header != Some(&get_auth_token()) {
        log::warn!("Unauthorized WebSocket connection attempt");
        return (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    ws.max_message_size(10 * 1024 * 1024) // 10MB
      .on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let client_id = nanoid!(8);
    log::info!("Client connected: {}", client_id);

    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    // Store client sender
    state.clients.lock().unwrap().insert(client_id.clone(), tx);

    // Send ready message
    let ready_msg = TunnelMessage::Ready { 
        url: format!("http://localhost:3000/tunnel/{}", client_id) 
    };
    let _ = sender.send(Message::Text(serde_json::to_string(&ready_msg).unwrap())).await;

    // Task to forward messages from our internal channel to the WebSocket
    let sender_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender.send(msg).await.is_err() { break; }
        }
    });

    // Handle incoming messages from the CLI client
    while let Some(Ok(msg)) = receiver.next().await {
        if let Message::Text(text) = msg {
            if let Ok(TunnelMessage::Response(res)) = serde_json::from_str::<TunnelMessage>(&text) {
                if let Some(tx) = state.pending_requests.lock().unwrap().remove(&res.request_id) {
                    let _ = tx.send(res);
                }
            }
        }
    }

    log::info!("Client disconnected: {}", client_id);
    state.clients.lock().unwrap().remove(&client_id);
    sender_task.abort();
}

async fn tunnel_handler(
    Path(params): Path<HashMap<String, String>>,
    State(state): State<Arc<AppState>>,
    method: axum::http::Method,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let client_id = params.get("client_id").cloned().unwrap_or_default();
    let path = params.get("path").cloned().unwrap_or_default();

    log::info!("Incoming tunnel request: {} /{} (Client: {})", method, path, client_id);

    let client_tx = {
        let clients = state.clients.lock().unwrap();
        clients.get(&client_id).cloned()
    };

    let Some(tx) = client_tx else {
        let active_clients: Vec<String> = state.clients.lock().unwrap().keys().cloned().collect();
        log::warn!("Tunnel not found for client: '{}'. Active tunnels: {:?}", client_id, active_clients);
        return (axum::http::StatusCode::NOT_FOUND, format!("Tunnel not found for ID: {}. Active: {:?}", client_id, active_clients)).into_response();
    };

    let request_id = nanoid!();
    let (res_tx, res_rx) = oneshot::channel();
    state.pending_requests.lock().unwrap().insert(request_id.clone(), res_tx);

    let tunnel_req = TunnelMessage::Request(TunnelRequest {
        id: request_id.clone(),
        method: method.to_string(),
        path,
        headers: headers.iter()
            .filter(|(k, _)| is_safe_request_header(k.as_str()))
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect(),
        body: if body.is_empty() { None } else { Some(base64::engine::general_purpose::STANDARD.encode(&body)) },
    });

    let _ = tx.send(Message::Text(serde_json::to_string(&tunnel_req).unwrap()));

    // Wait for response with 10s timeout
    match tokio::time::timeout(tokio::time::Duration::from_secs(10), res_rx).await {
        Ok(Ok(res)) => {
            log::info!("Relaying response: {} for request: {}", res.status, res.request_id);
            let mut builder = axum::response::Response::builder()
                .status(res.status);
            
            for (k, v) in &res.headers {
                if is_safe_response_header(k.as_str()) {
                    builder = builder.header(k, v);
                }
            }

            let body_bytes = res.get_body_bytes();
            builder.body(axum::body::Body::from(body_bytes)).unwrap().into_response()
        }
        _ => {
            log::error!("Timeout waiting for response from client: {}", client_id);
            state.pending_requests.lock().unwrap().remove(&request_id);
            (axum::http::StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout").into_response()
        }
    }
}

fn is_safe_request_header(name: &str) -> bool {
    let name = name.to_lowercase();
    // Block sensitive internal headers
    !matches!(name.as_str(), 
        "host" | "x-portal-auth" | "connection" | "upgrade" | "proxy-connection" | "proxy-authorization"
    )
}

fn is_safe_response_header(name: &str) -> bool {
    let name = name.to_lowercase();
    // Block hop-by-hop and sensitive server headers
    !matches!(name.as_str(), 
        "connection" | "upgrade" | "content-length" | "transfer-encoding" | "server" | "date"
    )
}
