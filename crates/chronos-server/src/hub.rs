//! The Axum egress hub: WebSocket fan out plus health and metrics endpoints
//! (blueprint Phase 4).
//!
//! Delta serialization is minimal: the server never ships the full graph. A new
//! client receives a single bounded snapshot of current links, then a stream of
//! incremental deltas. Backpressure is handled by the broadcast channel's ring
//! buffer: a slow client that falls behind receives a `Lagged` signal (the oldest
//! buffered frames are dropped) rather than stalling the producer.

use crate::state::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chronos_types::Delta;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::Ordering;
use tokio::sync::broadcast::error::RecvError;
use tower_http::cors::CorsLayer;
use tracing::debug;

/// Build the application router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.ready.load(Ordering::Relaxed) {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "starting")
    }
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        state.metrics.render(),
    )
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    metrics::gauge!(crate::metrics::CONNECTED_CLIENTS).increment(1.0);
    let mut receiver = state.deltas.subscribe();
    let (mut sink, mut stream) = socket.split();

    // Send a bounded initial snapshot of current links so the client can render
    // an approximate topology immediately.
    for (a, b) in state.graph.snapshot_edges(state.snapshot_max) {
        let frame = Delta::link_up(a, b);
        if let Ok(text) = serde_json::to_string(&frame) {
            if sink.send(Message::Text(text)).await.is_err() {
                metrics::gauge!(crate::metrics::CONNECTED_CLIENTS).decrement(1.0);
                return;
            }
        }
    }

    loop {
        tokio::select! {
            // Deltas flowing out to this client.
            received = receiver.recv() => {
                match received {
                    Ok(delta) => {
                        match serde_json::to_string(&delta) {
                            Ok(text) => {
                                if sink.send(Message::Text(text)).await.is_err() {
                                    break;
                                }
                            }
                            Err(err) => debug!(%err, "hub: failed to serialize delta"),
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        // The client fell behind; the oldest frames were dropped.
                        debug!(skipped, "hub: client lagged; dropped frames");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            // Inbound frames: we only care about close and pings.
            inbound = stream.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(err)) => {
                        debug!(%err, "hub: client read error");
                        break;
                    }
                }
            }
        }
    }

    metrics::gauge!(crate::metrics::CONNECTED_CLIENTS).decrement(1.0);
}
