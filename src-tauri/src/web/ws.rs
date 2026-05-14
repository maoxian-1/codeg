use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{Extension, WebSocketUpgrade},
    response::IntoResponse,
};

use super::shutdown::ShutdownSignal;
use crate::app_state::AppState;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<Arc<AppState>>,
    Extension(shutdown_signal): Extension<Arc<ShutdownSignal>>,
) -> impl IntoResponse {
    ws.protocols([super::auth::WS_EVENT_PROTOCOL])
        .on_upgrade(|socket| handle_ws_connection(socket, state, shutdown_signal))
}

async fn handle_ws_connection(
    mut socket: WebSocket,
    state: Arc<AppState>,
    shutdown_signal: Arc<ShutdownSignal>,
) {
    // Late handshake guard: if shutdown already fired before this task
    // even started, exit before subscribing to anything else.
    if shutdown_signal.is_triggered() {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    let mut rx = state.event_broadcaster.subscribe();

    loop {
        tokio::select! {
            // Server-initiated shutdown: close the socket cleanly so hyper's
            // graceful drain can resolve and the listener is dropped without
            // waiting for the 2 s abort fallback in do_stop_web_server.
            _ = shutdown_signal.wait() => {
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        if let Ok(msg) = serde_json::to_string(&event) {
                            if socket.send(Message::Text(msg.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Capacity-shaped by emit_with_state's burst rate vs.
                        // the WebSocket client's read speed. Logged at WARN —
                        // visible-but-non-fatal; the client will receive the
                        // next event but missed the dropped ones.
                        eprintln!("[WS][WARN] receiver lagged, skipped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(_)) => {
                        // Client messages currently unused; reserved for future use
                    }
                    _ => break,
                }
            }
        }
    }
}
