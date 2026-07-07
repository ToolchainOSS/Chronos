//! Integration test: a mock RIS Live WebSocket server verifies that the ingest
//! client connects, sends a subscription, and pushes parsed routing messages into
//! the bounded channel.

use chronos_ingest::{IngestConfig, IngestStats, run_ingest};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

const FIXTURE: &str = r#"{"type":"ris_message","data":{
    "timestamp": 1700000000.0,
    "peer_asn": "64500",
    "type": "UPDATE",
    "path": [64500, 64501, 64502],
    "announcements": [{"next_hop":"10.0.0.1","prefixes":["192.0.2.0/24"]}],
    "withdrawals": []
}}"#;

#[tokio::test]
async fn ingest_client_receives_parsed_messages() {
    // Bind an ephemeral port and serve a single client.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

        // Expect the subscription frame first.
        if let Some(Ok(Message::Text(sub))) = ws.next().await {
            assert!(sub.contains("ris_subscribe"));
            assert!(sub.contains("UPDATE"));
        } else {
            panic!("did not receive a subscription frame");
        }

        // Push a few fixtures, then a control frame that must be skipped.
        for _ in 0..3 {
            ws.send(Message::Text(FIXTURE.to_string().into()))
                .await
                .unwrap();
        }
        ws.send(Message::Text(
            r#"{"type":"pong","data":{}}"#.to_string().into(),
        ))
        .await
        .unwrap();

        // Keep the connection open briefly so the client can drain frames.
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let (tx, mut rx) = mpsc::channel(16);
    let stats = Arc::new(IngestStats::default());
    let config = IngestConfig {
        url: format!("ws://{addr}/"),
        ..IngestConfig::default()
    };

    // Shutdown after we have collected the expected messages.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let ingest = tokio::spawn(run_ingest(config, tx, stats.clone(), async move {
        let _ = shutdown_rx.await;
    }));

    // Collect three routing messages (the control frame must not appear).
    let mut received = 0;
    for _ in 0..3 {
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for a message")
            .expect("channel closed unexpectedly");
        assert_eq!(msg.path.len(), 3);
        assert_eq!(msg.announced_prefixes().count(), 1);
        received += 1;
    }
    assert_eq!(received, 3);

    let _ = shutdown_tx.send(());
    let _ = ingest.await;
    let _ = server.await;

    assert_eq!(stats.parsed.load(std::sync::atomic::Ordering::Relaxed), 3);
}
