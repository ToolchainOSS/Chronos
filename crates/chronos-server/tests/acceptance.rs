//! End to end acceptance suite for the Chronos egress boundary.
//!
//! These tests drive the real Axum router built by `chronos_server::hub::router`
//! and assert the externally observable contract that the frontend depends on:
//!
//! 1. Health and readiness endpoints report liveness/readiness correctly.
//! 2. The Prometheus `/metrics` endpoint is served with the right content type.
//! 3. The WebSocket egress sends a bounded initial snapshot of the current
//!    topology, then streams live `Delta` frames, all in the exact JSON wire
//!    form shared with the TypeScript client (the "sacred" Delta contract).
//!
//! No mounted data files are required, so this suite runs in CI unchanged.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chronos_server::hub::router;
use chronos_server::metrics::standalone_handle;
use chronos_server::state::AppState;
use chronos_topology::AsGraph;
use chronos_types::{Asn, Delta};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

/// Build an `AppState` with a seeded topology edge and a delta channel.
fn test_state(ready: bool) -> (AppState, broadcast::Sender<Delta>, Arc<AsGraph>) {
    let (deltas, _rx) = broadcast::channel(64);
    let graph = Arc::new(AsGraph::new());
    // Seed one undirected edge so the initial snapshot is non-empty.
    graph.add_edge(Asn(64500), Asn(64501), 1.0);

    let state = AppState {
        deltas: deltas.clone(),
        graph: graph.clone(),
        metrics: Arc::new(standalone_handle()),
        snapshot_max: 100,
        ready: Arc::new(AtomicBool::new(ready)),
    };
    (state, deltas, graph)
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn healthz_reports_ok() {
    let (state, _tx, _graph) = test_state(true);
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "ok");
}

#[tokio::test]
async fn readyz_reflects_readiness_flag() {
    // Not ready yet: the endpoint must report 503 so orchestrators hold traffic.
    let (state, _tx, _graph) = test_state(false);
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Ready: the endpoint must report 200.
    let (state, _tx, _graph) = test_state(true);
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn metrics_endpoint_is_prometheus_text() {
    let (state, _tx, _graph) = test_state(true);
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "unexpected content-type: {content_type}"
    );
}

#[tokio::test]
async fn websocket_streams_snapshot_then_live_deltas() {
    let (state, tx, _graph) = test_state(true);
    let app = router(state);

    // Bind an ephemeral port and serve the real router.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("ws://{addr}/ws");
    let (mut socket, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();

    // 1. The first frame must be the seeded edge, encoded as a LinkUp delta in
    //    the shared wire form.
    let snapshot = next_delta(&mut socket).await;
    assert_eq!(snapshot, Delta::LinkUp { a: 64500, b: 64501 });

    // 2. A delta broadcast after connection must reach the client verbatim,
    //    proving the live streaming path and the AreaDegraded wire form.
    tx.send(Delta::area_degraded("US-CA", 0.75)).unwrap();
    let live = next_delta(&mut socket).await;
    assert_eq!(
        live,
        Delta::AreaDegraded {
            region: "US-CA".to_string(),
            severity: 0.75,
        }
    );

    socket.close(None).await.unwrap();
    server.abort();
}

/// Read the next text frame from the socket and decode it as a `Delta`.
async fn next_delta(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Delta {
    loop {
        match socket.next().await.expect("stream ended").unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}
