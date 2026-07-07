//! Opt-in PostgreSQL integration test for the history sink.
//!
//! This test is `#[ignore]` by default because it needs a real PostgreSQL
//! instance; the offline CI gate does not run it. Provide a throwaway database
//! and run it explicitly:
//!
//! ```bash
//! export CHRONOS_TEST_DATABASE_URL=postgres://chronos:chronos@localhost:5432/chronos
//! cargo test -p chronos-history --test postgres_roundtrip -- --ignored
//! ```
//!
//! It clobbers the `anomaly_events` table, so point it at a disposable database.

use chronos_history::{EventKind, HistoryEvent, HistorySink, PostgresSink, RetentionPolicy};
use tokio_postgres::NoTls;

fn test_url() -> Option<String> {
    std::env::var("CHRONOS_TEST_DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
}

const DAY_SECS: f64 = 86_400.0;

#[tokio::test]
#[ignore = "requires a PostgreSQL instance via CHRONOS_TEST_DATABASE_URL"]
async fn records_and_prunes() {
    let Some(url) = test_url() else {
        eprintln!("skipping: CHRONOS_TEST_DATABASE_URL not set");
        return;
    };

    // Start from a clean slate.
    let (admin, conn) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("connect for cleanup");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    admin
        .batch_execute("DROP TABLE IF EXISTS anomaly_events CASCADE")
        .await
        .expect("drop table");

    let mut sink = PostgresSink::new(&url).expect("valid url");

    // Two events on different days so pruning has partitions to remove.
    let now = 1_780_000_000.0_f64; // an arbitrary fixed instant
    let old = now - 40.0 * DAY_SECS; // 40 days ago (outside a 30-day window)

    let mut hijack = HistoryEvent::new(now, EventKind::PrefixHijack, 2);
    hijack.prefix = Some("192.0.2.0/24".to_string());
    hijack.previous_origin = Some(64500);
    hijack.new_origin = Some(64501);
    hijack.region = Some("US-CA".to_string());

    let mut leak = HistoryEvent::new(old, EventKind::PathLeak, 1);
    leak.offending_asn = Some(64502);
    leak.as_path = Some(vec![64510, 64502, 64500]);

    sink.record_events(&[hijack, leak])
        .await
        .expect("record events");

    // Both rows should be present before pruning.
    assert_eq!(count_rows(&admin).await, 2);

    // Prune with a 30-day window: the 40-day-old event's partition is dropped.
    let outcome = sink
        .prune(&RetentionPolicy {
            now,
            retention_days: 30,
            max_bytes: u64::MAX,
        })
        .await
        .expect("prune");
    assert_eq!(outcome.dropped_partitions, 1);
    assert_eq!(count_rows(&admin).await, 1);

    // A zero byte cap forces dropping the oldest surviving partition, but at
    // least one partition is always kept.
    let outcome = sink
        .prune(&RetentionPolicy {
            now,
            retention_days: 30,
            max_bytes: 0,
        })
        .await
        .expect("prune size cap");
    assert_eq!(outcome.dropped_partitions, 0);
    assert_eq!(count_rows(&admin).await, 1);
}

async fn count_rows(client: &tokio_postgres::Client) -> i64 {
    client
        .query_one("SELECT COUNT(*)::BIGINT FROM anomaly_events", &[])
        .await
        .expect("count")
        .get(0)
}
