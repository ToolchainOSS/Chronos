//! The PostgreSQL implementation of [`HistorySink`].
//!
//! Design notes:
//! - Events land in a single table partitioned by UTC day
//!   (`anomaly_events_YYYYMMDD`), so retention pruning is a `DROP TABLE` of whole
//!   partitions: O(1) and bloat-free, unlike a `DELETE` sweep.
//! - The sink owns one connection, driven by the writer task. It reconnects
//!   lazily on the next call after a failure, so a database outage degrades
//!   gracefully (events are dropped by the writer) and never blocks the engine.
//! - Prefixes are stored as native `inet` (bound as text and cast with
//!   `$n::inet`) so a future read path can run containment queries without a
//!   crate-version-coupled type mapping. AS paths are stored as `bigint[]`.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::collections::HashSet;
use std::str::FromStr;

use tokio_postgres::{Client, Config, NoTls};
use tracing::{info, warn};

use crate::calendar::{date_string, day_index, parse_partition_day, partition_name};
use crate::event::HistoryEvent;
use crate::sink::{HistorySink, PruneOutcome, RetentionPolicy};

/// Single-row insert statement. Rows are inserted individually inside one
/// transaction per batch: anomaly events are low volume (the point of history is
/// the derived signal, not the firehose), so a prepared statement plus a single
/// commit is both simple and efficient, and it sidesteps boxed `dyn ToSql`
/// parameters (which are not `Send`). `to_timestamp` converts the epoch seconds
/// to `timestamptz`; the prefix text is cast to `inet`.
const INSERT_SQL: &str = "INSERT INTO anomaly_events \
     (observed_at, kind, severity, prefix, previous_origin, new_origin, \
      offending_asn, as_path, region, updates_in_window, threshold) \
     VALUES (to_timestamp($1), $2, $3, $4::inet, $5, $6, $7, $8, $9, $10, $11)";

/// A PostgreSQL-backed history store.
pub struct PostgresSink {
    /// Parsed connection configuration; reused on every (re)connect.
    pg_config: Config,
    /// The live client, if currently connected.
    client: Option<Client>,
    /// Whether the schema has been initialized on the current connection.
    schema_ready: bool,
    /// Names of partitions already ensured on the current connection, to skip
    /// redundant `CREATE TABLE` DDL on the hot batch path.
    ensured_partitions: HashSet<String>,
}

impl PostgresSink {
    /// Parse a connection string (for example
    /// `postgres://user:pass@host:5432/db`) without connecting. Connection is
    /// deferred to the first write so startup never blocks or fails on the
    /// database being unavailable.
    pub fn new(url: &str) -> anyhow::Result<Self> {
        let pg_config = Config::from_str(url)
            .map_err(|e| anyhow::anyhow!("invalid history database URL: {e}"))?;
        Ok(Self {
            pg_config,
            client: None,
            schema_ready: false,
            ensured_partitions: HashSet::new(),
        })
    }

    /// Ensure a live connection and an initialized schema, connecting or
    /// reconnecting as needed.
    async fn ensure_ready(&mut self) -> anyhow::Result<()> {
        let need_connect = match &self.client {
            None => true,
            Some(client) => client.is_closed(),
        };
        if need_connect {
            let (client, connection) = self.pg_config.connect(NoTls).await?;
            // The connection future drives the socket; it resolves when the
            // client is dropped or the link fails. Detach it onto the runtime.
            tokio::spawn(async move {
                if let Err(err) = connection.await {
                    warn!(%err, "history: postgres connection closed");
                }
            });
            self.client = Some(client);
            self.schema_ready = false;
            self.ensured_partitions.clear();
            info!("history: connected to postgres");
        }
        if !self.schema_ready {
            let client = self.client.as_ref().expect("client set above");
            init_schema(client).await?;
            self.schema_ready = true;
        }
        Ok(())
    }

    /// Ensure the daily partitions covering every event in the batch exist.
    async fn ensure_partitions(&mut self, events: &[HistoryEvent]) -> anyhow::Result<()> {
        let mut days: Vec<i64> = events.iter().map(|e| day_index(e.observed_at)).collect();
        days.sort_unstable();
        days.dedup();
        for day in days {
            let name = partition_name(day);
            if self.ensured_partitions.contains(&name) {
                continue;
            }
            let client = self.client.as_ref().expect("connected before insert");
            create_partition(client, day).await?;
            self.ensured_partitions.insert(name);
        }
        Ok(())
    }
}

impl HistorySink for PostgresSink {
    async fn record_events(&mut self, events: &[HistoryEvent]) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        self.ensure_ready().await?;
        self.ensure_partitions(events).await?;
        let client = self.client.as_mut().expect("connected above");
        let tx = client.transaction().await?;
        let stmt = tx.prepare(INSERT_SQL).await?;
        for event in events {
            let kind = event.kind.as_str();
            tx.execute(
                &stmt,
                &[
                    &event.observed_at,
                    &kind,
                    &event.severity,
                    &event.prefix,
                    &event.previous_origin,
                    &event.new_origin,
                    &event.offending_asn,
                    &event.as_path,
                    &event.region,
                    &event.updates_in_window,
                    &event.threshold,
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn prune(&mut self, policy: &RetentionPolicy) -> anyhow::Result<PruneOutcome> {
        self.ensure_ready().await?;
        let client = self.client.as_ref().expect("connected above");

        // Enumerate existing daily partitions, oldest first.
        let rows = client
            .query(
                "SELECT c.relname FROM pg_inherits i \
                 JOIN pg_class c ON c.oid = i.inhrelid \
                 WHERE i.inhparent = 'anomaly_events'::regclass",
                &[],
            )
            .await?;
        let mut partitions: Vec<(i64, String)> = rows
            .iter()
            .filter_map(|row| {
                let name: String = row.get(0);
                parse_partition_day(&name).map(|day| (day, name))
            })
            .collect();
        partitions.sort_unstable_by_key(|(day, _)| *day);

        let mut dropped = 0usize;

        // Age based pruning: drop days strictly older than the retention window.
        let cutoff = day_index(policy.now) - i64::from(policy.retention_days);
        let mut remaining: Vec<(i64, String)> = Vec::with_capacity(partitions.len());
        for (day, name) in partitions {
            if day < cutoff {
                drop_partition(client, &name).await?;
                self.ensured_partitions.remove(&name);
                dropped += 1;
            } else {
                remaining.push((day, name));
            }
        }

        // Size cap: while over budget, drop the oldest surviving partition. Keep
        // at least one so the current day always has somewhere to land.
        let mut total = total_size(client).await?;
        let mut idx = 0usize;
        while total > policy.max_bytes && remaining.len() - idx > 1 {
            let (_, name) = &remaining[idx];
            drop_partition(client, name).await?;
            self.ensured_partitions.remove(name);
            dropped += 1;
            idx += 1;
            total = total_size(client).await?;
        }

        Ok(PruneOutcome {
            dropped_partitions: dropped,
            total_bytes: total,
        })
    }
}

/// Create the partitioned parent table and its indexes if they do not exist.
async fn init_schema(client: &Client) -> anyhow::Result<()> {
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS anomaly_events (
                 observed_at       TIMESTAMPTZ      NOT NULL,
                 kind              TEXT             NOT NULL,
                 severity          SMALLINT         NOT NULL,
                 prefix            INET,
                 previous_origin   BIGINT,
                 new_origin        BIGINT,
                 offending_asn     BIGINT,
                 as_path           BIGINT[],
                 region            TEXT,
                 updates_in_window INTEGER,
                 threshold         DOUBLE PRECISION
             ) PARTITION BY RANGE (observed_at);
             CREATE INDEX IF NOT EXISTS anomaly_events_observed_idx
                 ON anomaly_events (observed_at);
             CREATE INDEX IF NOT EXISTS anomaly_events_kind_idx
                 ON anomaly_events (kind, observed_at);
             CREATE INDEX IF NOT EXISTS anomaly_events_region_idx
                 ON anomaly_events (region, observed_at);
             CREATE INDEX IF NOT EXISTS anomaly_events_prefix_idx
                 ON anomaly_events USING gist (prefix inet_ops);",
        )
        .await?;
    Ok(())
}

/// Create the daily partition covering `day` if it does not already exist.
///
/// The table name and boundary literals are derived from an internal integer day
/// index (never user input), so string interpolation here is not an injection
/// vector.
async fn create_partition(client: &Client, day: i64) -> anyhow::Result<()> {
    let name = partition_name(day);
    let lo = date_string(day);
    let hi = date_string(day + 1);
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS \"{name}\" PARTITION OF anomaly_events \
         FOR VALUES FROM ('{lo}') TO ('{hi}')"
    );
    client.batch_execute(&sql).await?;
    Ok(())
}

/// Drop a single daily partition.
async fn drop_partition(client: &Client, name: &str) -> anyhow::Result<()> {
    let sql = format!("DROP TABLE IF EXISTS \"{name}\"");
    client.batch_execute(&sql).await?;
    Ok(())
}

/// Sum the on-disk size (table plus indexes and TOAST) of every partition.
async fn total_size(client: &Client) -> anyhow::Result<u64> {
    let row = client
        .query_one(
            "SELECT COALESCE(SUM(pg_total_relation_size(i.inhrelid)), 0)::BIGINT \
             FROM pg_inherits i WHERE i.inhparent = 'anomaly_events'::regclass",
            &[],
        )
        .await?;
    let bytes: i64 = row.get(0);
    Ok(bytes.max(0) as u64)
}
